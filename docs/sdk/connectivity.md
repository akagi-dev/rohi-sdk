# Telemetry (`rohi-connectivity`)

`rohi-connectivity` turns an immutable `MeasurementsSnapshot` into a signed
[`airalab/connectivity-protocol`](https://github.com/airalab/connectivity-protocol) frame and delivers it through a
pluggable transport. HTTP is implemented first, on top of `rohi_net::NetworkHandle`.

The wire format is pinned to `airalab/connectivity-protocol@33b4e7ec6eaa19a930b91bfb59bd3337c1ec88ac`. The `.proto`
files are vendored in `crates/rohi-connectivity-proto/proto/` (verbatim copy, license included) and the revision is
recorded in `crates/rohi-connectivity-proto/UPSTREAM_REV`; no build downloads anything. The reference implementation is
[`airalab/robins`](https://github.com/airalab/robins).

```text
sensor tasks ─► MeasurementStore ─snapshot─► UrbanEncoder ─► core.v1.Message   (encoded once)
                                                                │
        UtcClock ─ Meta.timestamp ──────────────────────────────┤
        NonceSource ─ 16-byte nonce ────────────────────────────┤
        FrameSigner ─ sign(sensor_id ‖ nonce ‖ message) ────────┤
                                                                ▼
                                          EnvelopeBuilder ─► EnvelopeFrame     (encoded once, immutable)
                                                                │
                                              Outbox (bounded) ◄┘
                                                │
                                     EnvelopeTransport ─► HttpTransport ─► NetworkHandle
```

## Wire contract

`crypto.v1.SignedEnvelope` has exactly `sensor_id`, `nonce`, `message` and `signature` at the pinned revision; there is
no envelope timestamp. The signature covers `sensor_id ‖ nonce ‖ message`, where `message` is the exact serialized
`core.v1.Message`. The timestamp is `core.v1.Meta.timestamp`, Unix UTC milliseconds.

* The message is serialized **once**, signed as those bytes, wrapped in the envelope, and the envelope is serialized
  **once** into an `EnvelopeFrame`.
* A retry resends the same frame byte for byte: nonce, timestamp and signature are never regenerated and nothing is
  re-encoded from decoded fields.
* The signing preimage is documented and tested in `rohi_connectivity_proto::signing_preimage`; golden vectors
  generated with `robins` pin the whole chain (see below).

## Measurement mapping

Only `MeasurementState::Valid` measurements are emitted. A cached value of a `WarmingUp`, `Stale` or `Error`
measurement is never sent. Every `Urban` sensor message carries one measurement, so each measurement is one
`UrbanSensor` entry.

| Rohi observation | Protocol field | Conversion |
|---|---|---|
| BMx280 temperature, only when the humidity channel exists (BME280) | `BME280.temperature` | °C → 0.01 °C (`sint32`) |
| BME280 humidity | `BME280.humidity` | % → 0.01 % (clamped to 0..100 %) |
| BME280 pressure | `BME280.pressure` | Pa → 0.1 Pa |
| SDS011 PM2.5 / PM10 | `SDS011.pm25` / `pm10` | µg/m³ → 0.1 µg/m³ |
| Microphone average / maximum | `ICS43434.noise_avg` / `noise_max` | dB → whole dB |
| BMP280 (no humidity channel), SCD4x, ZMOD4510, AGS3871, RadSens | — | omitted |
| GPS | — | not produced (no GPS observation in the store) |

A sensor without an unambiguous representation is **omitted, never relabeled**; omissions are counted
(`Diagnostics::measurements_unsupported`). Values that cannot be represented (NaN, infinite, negative where the field
is unsigned) are omitted and counted too. Supporting SCD4x or a generic BMx280 needs a protocol change and is out of
scope.

## UTC and entropy

`Meta.timestamp` is **UTC**, from a `UtcClock`; monotonic time (`embassy_time::Instant`, `MonotonicTimestamp`) is never
used as UTC. The timestamp is the UTC time of the measurement set: UTC now minus the age of the snapshot
(`monotonic now − snapshot.generated_at`). `TimeService` implements `UtcClock`; while it is not synchronized (or the
age exceeds the clock value) **no frame is created** and sensing continues.

`NonceSource` supplies the 16-byte nonce, requested once per frame before the frame exists. The firmware uses the
hardware RNG (true random while the radio runs; nonces only need to be unique for replay protection). Without entropy
no frame is created.

## Identity

The service sees a `FrameSigner` (`sensor_id()` and `sign_payload()`): only the public key and signatures cross the
boundary, never key bytes. Every `rohi_identity::Signer` is a `FrameSigner`. The firmware's `StaticSigner` follows the
runtime identity (provisioned or erased at any time). Without identity no frame is created.

## Transport

```rust,ignore
pub trait EnvelopeTransport {
    async fn send(&mut self, frame: &EnvelopeFrame) -> Result<TransportAck, TransportError>;
}
```

Transports operate on the immutable frame; they cannot change what was signed. `HttpTransport` sends

```http
POST /v1/telemetry HTTP/1.1
Host: <host>[:<port>]
Content-Type: application/protobuf
Content-Length: <frame length>
Connection: close

<exact SignedEnvelope bytes>
```

over plain TCP (DNS or an IPv4 literal, then `NetworkHandle::connect`), one request per connection, with a total
timeout per attempt (20 s). TLS can later wrap the connection without touching encoding or signing.

### Error classification

Centralized in `TransportError::disposition` / `classify_status`:

| Result | Disposition |
|---|---|
| `202 Accepted` | delivered, frame removed |
| network unavailable, DNS, connect, timeout, I/O error, malformed response | **retry** the same frame with backoff |
| `408`, `429`, `5xx` | retry with backoff |
| `1xx`, `3xx`, `2xx` other than `202` (captive portal, proxy) | retry (not an acceptance) |
| `401`, `403` | **unauthorized**: frame dropped, pause for the longest backoff (no hot loop) |
| `400`, `413`, any other `4xx` | **permanent**: frame dropped |

## Outbox, retry, runtime

`ConnectivityService::run` is one task, separate from sensing: every interval it creates a frame (identity? UTC?
snapshot, encode, sign), queues it in the bounded in-memory `Outbox` (no allocation), and delivers the oldest frame.

* Retryable failures keep the frame at the front and back off exponentially (2 s, doubling, capped at 5 min); a success
  resets the backoff. There is never a zero-length wait.
* When the outbox is full, **no new frame is created** (nothing is evicted) and `skipped_outbox_full` is incremented.
* The service only reads snapshots. Wi-Fi, DNS, TCP, server, time or authorization failures cannot stop or delay the
  sensors or the measurement store.
* The queue is in memory: frames are lost on reboot (a flash spool is out of scope).
* `ConnectivityShared::status()` publishes the state (`Idle`, `WaitingForUtc`, `WaitingForIdentity`, `Backoff(error)`,
  `Unauthorized(error)`) and the `Diagnostics` counters.

## Configuration

`ConnectivityConfig` is a persistent `ConfigSection` (owned by `rohi-connectivity`; see
[Add a ConfigSection](../guides/add-config-section.md)): `enabled`, `node_id`, `interval_s` (10 s … 24 h),
`host` (letters, digits, `.`, `-`; written into the `Host` header, so no spaces or line breaks) and `port`. The
CBOR layout is pinned by `cbor_encoding_is_stable`. Control-protocol operations to edit it are not part of this work
package.

In `altruist-urban`, when no section is stored the build-time defaults apply: the environment variables
`ROHI_CONNECTIVITY_HOST`, `ROHI_CONNECTIVITY_PORT`, `ROHI_CONNECTIVITY_NODE_ID` and `ROHI_CONNECTIVITY_INTERVAL_S`
at compile time. Without a host, telemetry is disabled.

## Limits

| Constant | Value |
|---|---|
| `MAX_SENSORS_PER_MESSAGE` | 12 |
| `MAX_MESSAGE_LEN` | 400 B (worst case of the schema: 399 B) |
| `MAX_ENVELOPE_LEN` | 544 B |
| `NONCE_LEN` | 16 B |
| `CONNECTIVITY_OUTBOX_DEPTH` (firmware) | 4 frames |

A full Altruist Urban frame (7 measurements) is about 200 B. Private (encrypted) sections and envelope batches are not
generated: decoding skips them as unknown fields.

## Testing and interoperability

* Unit tests cover valid-only emission, per-field mapping, omitted and unsupported sensors, timestamp placement, the
  signing preimage, nonce use, byte-identical retries, status classification, backoff, outbox-full behaviour and the
  HTTP exchange against the in-memory IP backend.
* `src/tests/golden.rs` holds interoperability vectors (message, preimage, Ed25519 signature, envelope) generated with
  the `robins` `edge` CLI (`a6c1d9c`, whose protocol submodule is exactly the pinned revision). Note: the CLI help says
  pressure is in hPa, but it takes pascal and encodes `101325` as `deci_pascal = 1013250`, as the schema documents.
* `tests/robins_e2e.rs` is an opt-in end-to-end test: a snapshot goes through the service and `HttpTransport` into a
  real `edge gateway`, which must answer `202`:

  ```sh
  ROHI_ROBINS_EDGE=/path/to/robins/target/debug/edge \
      cargo test -p rohi-connectivity --test robins_e2e -- --ignored
  ```

Compatibility assumptions: the ingress accepts `POST /v1/telemetry` with `application/protobuf` and answers `202`;
the sensor id must be accepted by the ingress authorization (robins `auth.mode = "none"` or whitelisting the device
public key).
