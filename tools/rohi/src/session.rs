///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//
///////////////////////////////////////////////////////////////////////////////
//! Request/response exchange over a chunked or a message transport.

use std::time::Duration;

use rohi_control_ble::frame::{Fragmenter, Reassembler};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::limits::{MAX_INNER_REQUEST_LEN, MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
use rohi_identity::{IdentitySigner, Signer};
use tokio::time::timeout;
use zeroize::Zeroize;

use crate::error::{Error, Result};
use crate::model::ErrorCode;

/// Default time to wait for a response.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// What one `send` and one `recv` of a [`ClientTransport`] carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Framing {
    /// Chunks of a message that the [`Session`] fragments and reassembles with
    /// [`rohi_control_ble::frame`]: the BLE link.
    #[default]
    Chunks,
    /// Whole messages: the link keeps message boundaries itself, as the TCP
    /// records of [`rohi_control_tcp::frame`] do.
    Messages,
}

/// A link to a device that moves chunks of at most
/// [`ClientTransport::max_payload`] bytes, or whole messages, in both
/// directions.
///
/// Implemented over BLE by [`BleLink`](crate::BleLink) (chunks) and over TCP by
/// [`TcpLink`](crate::TcpLink) (messages); tests implement it in memory.
#[allow(async_fn_in_trait)]
pub trait ClientTransport {
    /// Largest chunk the link carries; for [`Framing::Messages`] the largest
    /// message.
    fn max_payload(&self) -> usize;

    /// What `send` and `recv` carry. Chunks unless the link says otherwise.
    fn framing(&self) -> Framing {
        Framing::Chunks
    }

    /// Writes one chunk (or message) to the device.
    async fn send(&mut self, chunk: &[u8]) -> Result<()>;

    /// Waits for the next chunk (or message) the device sent.
    async fn recv(&mut self) -> Result<Vec<u8>>;
}

/// The domain separator of the signed preimage, see `control.proto`.
const AUTH_DOMAIN: &[u8] = b"rohi-control-auth-v1";

/// The administrator key of a session and the challenge of its connection.
struct Admin {
    signer: IdentitySigner,
    nonce: Option<[u8; 32]>,
    sequence: u64,
}

impl Drop for Admin {
    fn drop(&mut self) {
        self.nonce.zeroize();
    }
}

/// One connection to a device: frames requests, matches responses.
///
/// One request at a time, as the device allows only one in flight.
///
/// With an administrator key ([`set_admin_signer`](Self::set_admin_signer)) a
/// request that a restricted TCP endpoint refuses as unsigned
/// (`auth_required`) is sent again as a signed request: the first attempt was
/// refused before it ran, so nothing runs twice. The answer to a signed
/// request that is lost is never retried, because the outcome is unknown.
pub struct Session<T> {
    transport: T,
    next_request: u32,
    next_message: u8,
    timeout: Duration,
    admin: Option<Admin>,
}

impl<T: ClientTransport> Session<T> {
    /// A session over `transport`.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            next_request: 1,
            next_message: 0,
            timeout: DEFAULT_TIMEOUT,
            admin: None,
        }
    }

    /// Signs the requests that the device wants signed with `signer`, the
    /// administrator key the device trusts.
    pub fn set_admin_signer(&mut self, signer: IdentitySigner) {
        self.admin = Some(Admin {
            signer,
            nonce: None,
            sequence: 0,
        });
    }

    /// Changes the response timeout of following calls.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Response timeout of calls.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Ends the session and returns the transport, for example to close it.
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Sends `call` and returns the matching response.
    ///
    /// A typed error response of the device becomes [`Error::Device`].
    pub async fn call(&mut self, call: Call) -> Result<Reply> {
        let limit = self.timeout;
        timeout(limit, self.call_unbounded(call))
            .await
            .map_err(|_| Error::Timeout)?
    }

    fn take_request_id(&mut self) -> u32 {
        let id = self.next_request;
        self.next_request = self.next_request.wrapping_add(1).max(1);
        id
    }

    async fn call_unbounded(&mut self, call: Call) -> Result<Reply> {
        let request_id = self.take_request_id();
        let mut wire = [0u8; MAX_REQUEST_LEN];
        let len = encode(
            &pb::Request {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                request: Some(call),
            },
            &mut wire,
        )?;
        let first = self.exchange(request_id, &wire[..len]).await;
        let signed = self.admin.is_some()
            && matches!(&first, Err(Error::Unauthorized(reason)) if reason == "auth_required");
        let result = if signed {
            self.call_signed(request_id, &wire[..len]).await
        } else {
            first
        };
        wire.fill(0);
        result
    }

    // The refused request again, signed. A challenge that the device no longer
    // knows (a key rotation, a lost session) is asked for once more: that
    // refusal came before the request ran.
    async fn call_signed(&mut self, request_id: u32, inner: &[u8]) -> Result<Reply> {
        if inner.len() > MAX_INNER_REQUEST_LEN {
            return Err(Error::TooLarge);
        }
        let mut attempt = 0;
        loop {
            let nonce = match self.admin.as_ref().and_then(|admin| admin.nonce) {
                Some(nonce) => nonce,
                None => self.challenge().await?,
            };
            let Some(admin) = self.admin.as_mut() else {
                return Err(Error::Unauthorized("no_admin_key".into()));
            };
            admin.sequence += 1;
            let sequence = admin.sequence;
            let mut preimage = Vec::with_capacity(AUTH_DOMAIN.len() + 40 + inner.len());
            preimage.extend_from_slice(AUTH_DOMAIN);
            preimage.extend_from_slice(&nonce);
            preimage.extend_from_slice(&sequence.to_le_bytes());
            preimage.extend_from_slice(inner);
            let signature = admin
                .signer
                .sign(&preimage)
                .map_err(|_| Error::InvalidInput("the administrator key cannot sign".into()))?;
            preimage.zeroize();
            let mut signed = pb::SignedRequest {
                sequence,
                ..Default::default()
            };
            signed
                .request_bytes
                .extend_from_slice(inner)
                .map_err(|_| Error::TooLarge)?;
            signed
                .signature
                .extend_from_slice(signature.as_bytes())
                .map_err(|_| Error::TooLarge)?;
            let mut envelope = pb::SignedEnvelope {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                ..Default::default()
            };
            envelope.set_signed_request(signed);
            let mut wire = [0u8; MAX_REQUEST_LEN];
            let len = encode(&envelope, &mut wire)?;
            let result = self.exchange(request_id, &wire[..len]).await;
            wire.fill(0);
            match result {
                Err(Error::Unauthorized(reason)) if reason == "no_challenge" && attempt == 0 => {
                    attempt += 1;
                    if let Some(admin) = self.admin.as_mut() {
                        admin.nonce = None;
                    }
                }
                other => return other,
            }
        }
    }

    // Asks for a fresh nonce for this connection; the sequence starts again.
    async fn challenge(&mut self) -> Result<[u8; 32]> {
        let request_id = self.take_request_id();
        let mut wire = [0u8; 16];
        let len = encode(
            &pb::Request {
                protocol_version: PROTOCOL_VERSION,
                request_id,
                request: Some(Call::GetAuthChallenge(pb::GetAuthChallengeRequest {})),
            },
            &mut wire,
        )?;
        let Reply::GetAuthChallenge(challenge) = self.exchange(request_id, &wire[..len]).await?
        else {
            return Err(Error::Protocol("unexpected answer to a challenge".into()));
        };
        if !challenge.admin_key_enrolled {
            return Err(Error::Unauthorized("no_admin_key".into()));
        }
        let nonce: [u8; 32] = challenge
            .session_nonce
            .as_slice()
            .try_into()
            .map_err(|_| Error::Protocol("the challenge is not 32 bytes".into()))?;
        if let Some(admin) = self.admin.as_mut() {
            admin.nonce = Some(nonce);
            admin.sequence = 0;
        }
        Ok(nonce)
    }

    // One request and its answer, on the wire.
    async fn exchange(&mut self, request_id: u32, wire: &[u8]) -> Result<Reply> {
        let message_id = self.next_message;
        self.next_message = self.next_message.wrapping_add(1);

        if self.transport.framing() == Framing::Messages {
            self.transport.send(wire).await?;
            loop {
                let bytes = self.transport.recv().await?;
                if let Some(result) = answer(&bytes, request_id)? {
                    return result;
                }
            }
        }

        let max_payload = self.transport.max_payload();
        let mut fragments = Fragmenter::new(message_id, wire, max_payload)?;
        let mut chunk = vec![0u8; max_payload];
        while let Some(len) = fragments.next_frame(&mut chunk) {
            self.transport.send(&chunk[..len]).await?;
        }

        let mut response = Reassembler::<MAX_RESPONSE_LEN>::new();
        loop {
            let frame = self.transport.recv().await?;
            // Tail frames of an abandoned response are not errors of this call.
            let Ok(Some(message)) = response.push(&frame) else {
                continue;
            };
            if message.id != message_id {
                // Late response of an earlier, abandoned request.
                continue;
            }
            if let Some(result) = answer(message.bytes, request_id)? {
                return result;
            }
        }
    }
}

/// Interprets one complete response message: `None` when it answers another
/// request, which the caller skips.
fn answer(bytes: &[u8], request_id: u32) -> Result<Option<Result<Reply>>> {
    let response: pb::Response = decode(bytes)?;
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "device speaks protocol version {}, expected {}",
            response.protocol_version, PROTOCOL_VERSION
        )));
    }
    // Id 0: the device could not even parse the request.
    if response.request_id != request_id && response.request_id != 0 {
        return Ok(None);
    }
    Ok(Some(match response.response {
        Some(Reply::Error(error)) => Err(match error.reason() {
            Some(reason) if error.code == pb::ErrorCode::Rejected => {
                Error::Rejected(reason.as_str().to_owned())
            }
            Some(reason) if error.code == pb::ErrorCode::PermissionDenied => {
                Error::Unauthorized(reason.as_str().to_owned())
            }
            _ => Error::Device(ErrorCode::from_proto(error.code)),
        }),
        Some(reply) => Ok(reply),
        None => Err(Error::Protocol("empty response".into())),
    }))
}
