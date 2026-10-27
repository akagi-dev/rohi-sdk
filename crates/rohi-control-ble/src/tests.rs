use core::cell::RefCell;
use std::collections::VecDeque;
use std::vec;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_control::{
    AuthorizationPolicy, Controller, Device, DeviceInfo, OpenPolicy, RequireEncryption,
};
use rohi_control_proto::control::{
    self as pb, Request_::Request as Call, Response_::Response as Reply,
};
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
use rohi_net::{BleConnectionInfo, BleError, BleState, BleTransport, MAX_BLE_PAYLOAD};
use rohi_service::{
    ActionDescriptor, ActionError, ActionOutput, FieldDescriptor, ManagedService, ServiceHealth,
    ServiceState, ServiceSummary, StatusWriter, Tracked, Value, ValueKind, WriteError,
};

use crate::ControlBle;
use crate::frame::{FRAME_VERSION, Fragmenter, FrameError, HEADER_LEN, Reassembler};

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

fn frames(id: u8, message: &[u8], max_payload: usize) -> Vec<Vec<u8>> {
    let mut fragmenter = Fragmenter::new(id, message, max_payload).unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; MAX_BLE_PAYLOAD];
    while let Some(len) = fragmenter.next_frame(&mut buf) {
        assert!(len <= max_payload);
        out.push(buf[..len].to_vec());
    }
    assert_eq!(out.len(), fragmenter.count());
    out
}

fn reassemble<const N: usize>(frames: &[Vec<u8>]) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
    let mut reassembler = Reassembler::<N>::new();
    let mut result = None;
    for frame in frames {
        if let Some(message) = reassembler.push(frame)? {
            result = Some((message.id, message.bytes.to_vec()));
        }
    }
    Ok(result)
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + 3) as u8).collect()
}

#[test]
fn single_fragment() {
    let message = pattern(10);
    let frames = frames(5, &message, 20);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].len(), HEADER_LEN + 10);
    assert_eq!(reassemble::<64>(&frames), Ok(Some((5, message))));
}

#[test]
fn multiple_fragments() {
    let message = pattern(100);
    let frames = frames(9, &message, 20);
    assert_eq!(frames.len(), 100usize.div_ceil(20 - HEADER_LEN));
    assert!(frames.iter().all(|frame| frame[1] == 9));
    assert_eq!(reassemble::<128>(&frames), Ok(Some((9, message))));
}

#[test]
fn every_length_and_payload_size_round_trips() {
    for max_payload in [HEADER_LEN + 1, 20, 23, 100, MAX_BLE_PAYLOAD] {
        for len in [1usize, 2, 13, 14, 15, 28, 255, 256] {
            if len.div_ceil(max_payload - HEADER_LEN) > crate::frame::MAX_FRAGMENTS {
                continue;
            }
            let message = pattern(len);
            let frames = frames(1, &message, max_payload);
            assert_eq!(
                reassemble::<MAX_REQUEST_LEN>(&frames),
                Ok(Some((1, message))),
                "payload {max_payload}, length {len}"
            );
        }
    }
}

#[test]
fn maximum_size_message_is_accepted() {
    let message = pattern(MAX_REQUEST_LEN);
    let frames = frames(1, &message, MAX_BLE_PAYLOAD);
    assert_eq!(
        reassemble::<MAX_REQUEST_LEN>(&frames),
        Ok(Some((1, message)))
    );
}

#[test]
fn one_byte_oversized_message_is_rejected() {
    let message = pattern(MAX_REQUEST_LEN + 1);
    let frames = frames(1, &message, MAX_BLE_PAYLOAD);
    assert_eq!(
        reassemble::<MAX_REQUEST_LEN>(&frames),
        Err(FrameError::TooLarge)
    );
}

#[test]
fn truncated_frames_are_rejected() {
    let mut reassembler = Reassembler::<64>::new();
    assert_eq!(reassembler.push(&[]).err(), Some(FrameError::Truncated));
    assert_eq!(
        reassembler.push(&[FRAME_VERSION, 1, 0]).err(),
        Some(FrameError::Truncated)
    );
    // Header without payload.
    assert_eq!(
        reassembler.push(&[FRAME_VERSION, 1, 0, 1, 4, 0]).err(),
        Some(FrameError::Truncated)
    );
}

#[test]
fn shorter_last_fragment_than_announced_is_a_length_mismatch() {
    let message = pattern(30);
    let mut frames = frames(1, &message, 20);
    let last = frames.last_mut().unwrap();
    last.pop();
    assert_eq!(reassemble::<64>(&frames), Err(FrameError::LengthMismatch));
}

#[test]
fn unknown_version_is_rejected() {
    let mut frame = frames(1, &pattern(4), 20).remove(0);
    frame[0] = FRAME_VERSION + 1;
    let mut reassembler = Reassembler::<64>::new();
    assert_eq!(reassembler.push(&frame).err(), Some(FrameError::Version));
}

#[test]
fn invalid_fragment_index_is_rejected() {
    let mut reassembler = Reassembler::<64>::new();
    // index 2 of 2
    assert_eq!(
        reassembler.push(&[FRAME_VERSION, 1, 2, 2, 4, 0, 1]).err(),
        Some(FrameError::InvalidIndex)
    );
    // count 0
    assert_eq!(
        reassembler.push(&[FRAME_VERSION, 1, 0, 0, 4, 0, 1]).err(),
        Some(FrameError::InvalidIndex)
    );
}

#[test]
fn duplicate_fragment_is_rejected_and_the_rest_is_skipped() {
    let message = pattern(60);
    let frames = frames(3, &message, 20);
    assert!(frames.len() >= 4);
    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&frames[0]).unwrap().is_none());
    assert!(reassembler.push(&frames[1]).unwrap().is_none());
    assert_eq!(
        reassembler.push(&frames[1]).err(),
        Some(FrameError::OutOfOrder)
    );
    // The tail of the broken message is skipped without further errors.
    for frame in &frames[2..] {
        assert!(reassembler.push(frame).unwrap().is_none());
    }
    // And a fresh message works.
    let fresh = pattern(5);
    let frames = super::tests::frames(4, &fresh, 20);
    let message = reassembler.push(&frames[0]).unwrap().unwrap();
    assert_eq!((message.id, message.bytes), (4, fresh.as_slice()));
}

#[test]
fn missing_fragment_is_rejected() {
    let message = pattern(60);
    let frames = frames(3, &message, 20);
    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&frames[0]).unwrap().is_none());
    assert_eq!(
        reassembler.push(&frames[2]).err(),
        Some(FrameError::OutOfOrder)
    );
}

#[test]
fn changing_the_header_mid_message_is_rejected() {
    let frames = frames(3, &pattern(60), 20);
    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&frames[0]).unwrap().is_none());
    let mut other_id = frames[1].clone();
    other_id[1] = 4;
    assert_eq!(
        reassembler.push(&other_id).err(),
        Some(FrameError::Inconsistent)
    );

    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&frames[0]).unwrap().is_none());
    let mut other_total = frames[1].clone();
    other_total[4] = 59;
    assert_eq!(
        reassembler.push(&other_total).err(),
        Some(FrameError::Inconsistent)
    );
}

#[test]
fn continuation_without_a_first_fragment_is_rejected() {
    let frames = frames(3, &pattern(60), 20);
    let mut reassembler = Reassembler::<64>::new();
    assert_eq!(
        reassembler.push(&frames[1]).err(),
        Some(FrameError::OutOfOrder)
    );
}

#[test]
fn a_new_first_fragment_replaces_a_partial_message() {
    let first = frames(1, &pattern(60), 20);
    let second = pattern(8);
    let second_frames = frames(2, &second, 20);
    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&first[0]).unwrap().is_none());
    assert!(reassembler.is_receiving());
    let message = reassembler.push(&second_frames[0]).unwrap().unwrap();
    assert_eq!((message.id, message.bytes), (2, second.as_slice()));
}

#[test]
fn reset_drops_the_partial_message() {
    let frames = frames(1, &pattern(60), 20);
    let mut reassembler = Reassembler::<64>::new();
    assert!(reassembler.push(&frames[0]).unwrap().is_none());
    reassembler.reset();
    assert!(!reassembler.is_receiving());
    assert_eq!(
        reassembler.push(&frames[1]).err(),
        Some(FrameError::OutOfOrder)
    );
}

#[test]
fn fragmenter_limits() {
    assert_eq!(
        Fragmenter::new(0, &[1], HEADER_LEN).err(),
        Some(FrameError::PayloadTooSmall)
    );
    assert_eq!(
        Fragmenter::new(0, &[], 20).err(),
        Some(FrameError::TooLarge)
    );
    // 256 fragments of one byte each do not fit the count field.
    let message = pattern(256);
    assert_eq!(
        Fragmenter::new(0, &message, HEADER_LEN + 1).err(),
        Some(FrameError::TooLarge)
    );
    assert!(Fragmenter::new(0, &message[..255], HEADER_LEN + 1).is_ok());
}

#[test]
fn the_largest_response_fits_the_default_mtu() {
    let message = pattern(MAX_RESPONSE_LEN);
    let fragmenter = Fragmenter::new(0, &message, rohi_net::DEFAULT_BLE_PAYLOAD).unwrap();
    assert!(fragmenter.count() <= crate::frame::MAX_FRAGMENTS);
}

// ---------------------------------------------------------------------------
// Control loop
// ---------------------------------------------------------------------------

enum Event {
    Frame(Vec<u8>),
    Disconnect,
    Connect,
}

struct Shared {
    events: RefCell<VecDeque<Event>>,
    sent: RefCell<Vec<Vec<u8>>>,
    connected: RefCell<bool>,
    epoch: RefCell<u32>,
    info: RefCell<BleConnectionInfo>,
    payload: usize,
    fail_send: RefCell<bool>,
}

impl Shared {
    fn new(payload: usize) -> Self {
        Self {
            events: RefCell::new(VecDeque::new()),
            sent: RefCell::new(Vec::new()),
            connected: RefCell::new(true),
            epoch: RefCell::new(1),
            info: RefCell::new(BleConnectionInfo::default()),
            payload,
            fail_send: RefCell::new(false),
        }
    }

    fn push_frames(&self, frames: Vec<Vec<u8>>) {
        for frame in frames {
            self.events.borrow_mut().push_back(Event::Frame(frame));
        }
    }

    fn push(&self, event: Event) {
        self.events.borrow_mut().push_back(event);
    }

    fn drop_link_and_reconnect(&self) {
        *self.epoch.borrow_mut() += 1;
    }

    fn take_sent(&self) -> Vec<Vec<u8>> {
        core::mem::take(&mut *self.sent.borrow_mut())
    }
}

impl BleTransport for &Shared {
    async fn recv(&mut self, out: &mut [u8]) -> Result<usize, BleError> {
        loop {
            let event = self.events.borrow_mut().pop_front().expect("script ended");
            match event {
                Event::Frame(frame) => {
                    out[..frame.len()].copy_from_slice(&frame);
                    return Ok(frame.len());
                }
                Event::Disconnect => {
                    *self.connected.borrow_mut() = false;
                    return Err(BleError::Disconnected);
                }
                Event::Connect => {
                    *self.connected.borrow_mut() = true;
                    *self.epoch.borrow_mut() += 1;
                }
            }
        }
    }

    async fn send(&mut self, data: &[u8]) -> Result<(), BleError> {
        assert!(data.len() <= self.payload, "frame exceeds the ATT payload");
        if *self.fail_send.borrow() {
            return Err(BleError::Busy);
        }
        self.sent.borrow_mut().push(data.to_vec());
        Ok(())
    }

    fn connection_info(&self) -> Option<BleConnectionInfo> {
        (*self.connected.borrow()).then(|| *self.info.borrow())
    }

    fn max_payload(&self) -> usize {
        self.payload
    }

    fn state(&self) -> BleState {
        BleState::Advertising
    }

    fn connection_epoch(&self) -> u32 {
        *self.epoch.borrow()
    }
}

/// A device with one service that records the actions it ran.
struct Radio<'a> {
    shared: &'a Shared,
    calls: RefCell<Vec<&'static str>>,
    /// Scans never complete while set.
    hold_scan: RefCell<bool>,
}

impl ManagedService for &Radio<'_> {
    const ID: u32 = 1;
    const NAME: &'static str = "radio";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(1, "a", ValueKind::String),
        FieldDescriptor::new(2, "b", ValueKind::String),
        FieldDescriptor::new(3, "c", ValueKind::String),
        FieldDescriptor::new(4, "d", ValueKind::String),
    ];
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(1, "scan"),
        ActionDescriptor::new(2, "reconnect"),
        ActionDescriptor::new(3, "disconnect").privileged(),
        ActionDescriptor::new(4, "store").args(&[FieldDescriptor::new(
            1,
            "data",
            ValueKind::Bytes,
        )]),
    ];

    async fn summary(&self) -> ServiceSummary {
        ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        // Long enough to need several fragments.
        for id in 1..=4 {
            out.text(id, &"x".repeat(32))?;
        }
        Ok(())
    }

    async fn invoke(
        &self,
        action: u32,
        _: &[(u32, Value)],
        _: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        match action {
            1 => {
                if *self.hold_scan.borrow() {
                    core::future::pending::<()>().await;
                }
                Ok(())
            }
            2 => {
                self.calls.borrow_mut().push("reconnect");
                // The central drops and reconnects while the request is handled.
                self.shared.drop_link_and_reconnect();
                Ok(())
            }
            3 => {
                self.calls.borrow_mut().push("disconnect");
                Ok(())
            }
            4 => Ok(()),
            _ => Err(ActionError::UnknownAction),
        }
    }
}

type Bench<'a> = Device<(), (Tracked<&'a Radio<'a>, NoopRawMutex>,)>;

fn uptime() -> u64 {
    600
}

fn radio(shared: &Shared) -> Radio<'_> {
    Radio {
        shared,
        calls: RefCell::new(Vec::new()),
        hold_scan: RefCell::new(false),
    }
}

fn device<'a>(radio: &'a Radio<'a>) -> Bench<'a> {
    Device::new(
        DeviceInfo {
            firmware: "fake-0",
            board: "bench-esp32c3",
        },
        uptime,
        (),
        (Tracked::new(radio),),
    )
}

fn invoke(service_id: u32, action_id: u32, args: &[pb::ActionArg]) -> Call {
    let mut list = heapless::Vec::new();
    for arg in args {
        list.push(arg.clone()).ok().unwrap();
    }
    Call::InvokeAction(pb::InvokeActionRequest {
        service_id,
        action_id,
        args: list,
    })
}

fn status_request() -> Call {
    Call::GetServiceStatus(pb::GetServiceStatusRequest { service_id: 1 })
}

fn request_bytes(id: u32, call: Call) -> Vec<u8> {
    let mut buf = [0u8; MAX_REQUEST_LEN];
    let len = encode(
        &pb::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: id,
            request: Some(call),
        },
        &mut buf,
    )
    .unwrap();
    buf[..len].to_vec()
}

fn run<P: AuthorizationPolicy>(
    shared: &Shared,
    controller: &Controller<Bench<'_>, P>,
    steps: usize,
) {
    let mut service = ControlBle::new(shared, controller);
    for _ in 0..steps {
        block_on(service.step());
    }
}

fn response_bytes(frames: &[Vec<u8>]) -> (u8, Vec<u8>) {
    reassemble::<MAX_RESPONSE_LEN>(frames).unwrap().unwrap()
}

#[test]
fn single_frame_request_gets_a_single_frame_response() {
    let shared = Shared::new(MAX_BLE_PAYLOAD);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    shared.push_frames(frames(
        7,
        &request_bytes(11, Call::GetInfo(pb::GetInfoRequest {})),
        MAX_BLE_PAYLOAD,
    ));
    run(&shared, &controller, 1);

    let sent = shared.take_sent();
    assert_eq!(sent.len(), 1);
    let (id, bytes) = response_bytes(&sent);
    assert_eq!(id, 7);
    let response: pb::Response = decode(&bytes).unwrap();
    assert_eq!(response.request_id, 11);
    assert!(matches!(response.response, Some(Reply::GetInfo(_))));
}

#[test]
fn multi_fragment_response_preserves_the_protobuf_bytes() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    let request = request_bytes(2, status_request());
    let request_frames = frames(1, &request, 20);
    let count = request_frames.len();
    shared.push_frames(request_frames);
    run(&shared, &controller, count);

    let sent = shared.take_sent();
    assert!(sent.len() > 1, "the response must need several fragments");
    assert!(sent.iter().all(|frame| frame.len() <= 20));
    let (id, bytes) = response_bytes(&sent);
    assert_eq!(id, 1);

    // The bytes equal what the dispatcher produces directly.
    let mut direct = [0u8; MAX_RESPONSE_LEN];
    let len = block_on(controller.handle(&Default::default(), &request, &mut direct));
    assert_eq!(bytes, direct[..len]);

    let response: pb::Response = decode(&bytes).unwrap();
    let Some(Reply::GetServiceStatus(status)) = response.response else {
        panic!("not a status");
    };
    assert_eq!(status.fields.len(), 4);
}

fn long_request() -> Vec<u8> {
    let data = pb::ActionArg {
        field_id: 1,
        value: Some(pb::ActionArg_::Value::BytesValue(
            heapless::Vec::from_slice(&[0x42; 60]).unwrap(),
        )),
    };
    request_bytes(3, invoke(1, 4, &[data]))
}

#[test]
fn multi_fragment_request_is_reassembled() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    let request = long_request();
    let request_frames = frames(8, &request, 20);
    assert!(request_frames.len() > 1);
    let count = request_frames.len();
    shared.push_frames(request_frames);
    run(&shared, &controller, count);

    let sent = shared.take_sent();
    let (id, bytes) = response_bytes(&sent);
    assert_eq!(id, 8);
    let response: pb::Response = decode(&bytes).unwrap();
    assert_eq!(response.request_id, 3);
    assert!(matches!(response.response, Some(Reply::InvokeAction(_))));
}

#[test]
fn rejected_frames_get_a_typed_error_once() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    let request = long_request();
    let mut broken = frames(6, &request, 20);
    broken.insert(2, broken[1].clone());
    let count = broken.len();
    shared.push_frames(broken);
    run(&shared, &controller, count);

    let sent = shared.take_sent();
    let (id, bytes) = response_bytes(&sent);
    assert_eq!(id, 6);
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(
        response.response,
        Some(Reply::Error(pb::Error {
            code: pb::ErrorCode::InvalidArgument,
            ..
        }))
    ));

    // The next request works.
    let good = frames(
        7,
        &request_bytes(3, Call::GetInfo(pb::GetInfoRequest {})),
        20,
    );
    let count = good.len();
    shared.push_frames(good);
    run(&shared, &controller, count);
    let (id, _) = response_bytes(&shared.take_sent());
    assert_eq!(id, 7);
}

#[test]
fn oversized_request_is_rejected_with_an_error() {
    let shared = Shared::new(MAX_BLE_PAYLOAD);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    let message = pattern(MAX_REQUEST_LEN + 1);
    let oversized = frames(5, &message, MAX_BLE_PAYLOAD);
    let count = oversized.len();
    shared.push_frames(oversized);
    run(&shared, &controller, count);

    let (id, bytes) = response_bytes(&shared.take_sent());
    assert_eq!(id, 5);
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(response.response, Some(Reply::Error(_))));
}

#[test]
fn disconnect_discards_the_partial_request() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    let request = long_request();
    let request_frames = frames(4, &request, 20);
    assert!(request_frames.len() > 2);

    // First fragment, then the link drops and a new central continues with
    // what would have been the second fragment.
    shared.push(Event::Frame(request_frames[0].clone()));
    shared.push(Event::Disconnect);
    shared.push(Event::Connect);
    shared.push(Event::Frame(request_frames[1].clone()));
    run(&shared, &controller, 3);

    // No response to the stale tail except a typed error.
    let (_, bytes) = response_bytes(&shared.take_sent());
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(response.response, Some(Reply::Error(_))));
    assert!(radio.calls.borrow().is_empty());

    // The state is clean: a complete request on the new link works.
    let count = request_frames.len();
    shared.push_frames(request_frames);
    run(&shared, &controller, count);
    let (_, bytes) = response_bytes(&shared.take_sent());
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(response.response, Some(Reply::InvokeAction(_))));
}

#[test]
fn response_is_dropped_when_the_link_changed_during_the_request() {
    let shared = Shared::new(MAX_BLE_PAYLOAD);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    shared.push_frames(frames(
        1,
        &request_bytes(1, invoke(1, 2, &[])),
        MAX_BLE_PAYLOAD,
    ));
    run(&shared, &controller, 1);

    assert_eq!(radio.calls.borrow().as_slice(), ["reconnect"]);
    assert!(shared.take_sent().is_empty());
}

#[test]
fn cancelled_step_abandons_the_request_and_the_next_one_works() {
    let shared = Shared::new(MAX_BLE_PAYLOAD);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    *radio.hold_scan.borrow_mut() = true;
    let mut service = ControlBle::new(&shared, &controller);

    shared.push_frames(frames(
        1,
        &request_bytes(1, invoke(1, 1, &[])),
        MAX_BLE_PAYLOAD,
    ));
    {
        // The scan never finishes: the caller gives up and drops the step.
        let mut step = core::pin::pin!(service.step());
        assert!(embassy_futures::poll_once(step.as_mut()).is_pending());
    }
    assert!(shared.take_sent().is_empty());

    // The abandoned request leaves no partial state behind and no response is
    // ever sent for it.
    *radio.hold_scan.borrow_mut() = false;
    shared.push_frames(frames(
        2,
        &request_bytes(2, Call::GetInfo(pb::GetInfoRequest {})),
        MAX_BLE_PAYLOAD,
    ));
    block_on(service.step());
    let sent = shared.take_sent();
    assert_eq!(sent.len(), 1);
    let (id, bytes) = response_bytes(&sent);
    assert_eq!(id, 2);
    let response: pb::Response = decode(&bytes).unwrap();
    assert_eq!(response.request_id, 2);
    assert!(matches!(response.response, Some(Reply::GetInfo(_))));
}

#[test]
fn send_failure_resets_the_state() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    *shared.fail_send.borrow_mut() = true;
    let request = request_bytes(2, status_request());
    let request_frames = frames(1, &request, 20);
    let count = request_frames.len();
    shared.push_frames(request_frames.clone());
    run(&shared, &controller, count);
    assert!(shared.take_sent().is_empty());

    *shared.fail_send.borrow_mut() = false;
    shared.push_frames(request_frames);
    run(&shared, &controller, count);
    assert!(!shared.take_sent().is_empty());
}

#[test]
fn link_security_reaches_the_policy() {
    let shared = Shared::new(MAX_BLE_PAYLOAD);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), RequireEncryption);
    let request = request_bytes(1, invoke(1, 3, &[]));

    shared.push_frames(frames(1, &request, MAX_BLE_PAYLOAD));
    run(&shared, &controller, 1);
    let (_, bytes) = response_bytes(&shared.take_sent());
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(
        response.response,
        Some(Reply::Error(pb::Error {
            code: pb::ErrorCode::PermissionDenied,
            ..
        }))
    ));
    assert!(radio.calls.borrow().is_empty());

    *shared.info.borrow_mut() = BleConnectionInfo {
        encrypted: true,
        authenticated: false,
    };
    shared.push_frames(frames(2, &request, MAX_BLE_PAYLOAD));
    run(&shared, &controller, 1);
    let (_, bytes) = response_bytes(&shared.take_sent());
    let response: pb::Response = decode(&bytes).unwrap();
    assert!(matches!(response.response, Some(Reply::InvokeAction(_))));
    assert_eq!(radio.calls.borrow().as_slice(), ["disconnect"]);
}

#[test]
fn stray_frames_are_answered_without_panicking() {
    let shared = Shared::new(20);
    let radio = radio(&shared);
    let controller = Controller::new(device(&radio), OpenPolicy);
    shared.push(Event::Frame(vec![]));
    shared.push(Event::Frame(vec![0xff; 20]));
    run(&shared, &controller, 2);
    assert_eq!(shared.take_sent().len(), 2);
}
