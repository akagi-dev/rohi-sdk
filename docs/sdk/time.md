# Time service (`rohi-time`, `rohi-time-ntp`)

`rohi-time` gives the firmware **one** source of wall-clock time and synchronization state. Components that need a
real-world timestamp (persistent logs, datalog, diagnostics, measurement timestamps) ask the `TimeService`. They never
contact an NTP server, manage a clock chip, keep clock offsets or decide whether a timestamp is trustworthy.

```text
   NtpTimeSource            future RtcTimeSource        external time
   (rohi-time-ntp)          (hardware RTC)
        │  DNS + UDP              │  I²C / SPI
        └────────────┬────────────┴─────────────┘
                     ▼   TimeSample { unix_time, uncertainty }
              Synchronizer        background task, backoff, resync
                     ▼
              ┌─────────────┐   validation (plausibility window)
              │ TimeService │   anchor = absolute time + monotonic instant
              └──────┬──────┘   state, failure counters
                     │  now() = anchor + monotonic elapsed
        ┌────────────┼────────────┐
        ▼            ▼            ▼
     logging       datalog     control plane (`time` service)
```

## Monotonic time versus wall-clock time

These are different concepts with different types, and there is no ambiguous `timestamp()`.

| | Monotonic time | Wall-clock time |
|---|----------------|-----------------|
| Type | `rohi_hal::MonotonicTimestamp` (from `embassy-time`) | `rohi_time::UnixTime` (Unix milliseconds, UTC) |
| Available | always, from boot | only after a valid sample; otherwise `None` |
| Can jump | never | **yes**, forward or backward, when a sample corrects it |
| Use for | deadlines, timeouts, intervals, retries, scheduling, elapsed time, ordering events in a boot | absolute timestamps only |

> **Wall-clock time may step.** A newly accepted sample replaces the anchor at once. The clock is not slewed and not
> made monotonic. Never use wall-clock time to measure durations or order events; use the monotonic clock. A future work
> item may add smoothing.

## Guarantees

- `TimeService::now()` is `None` until the first valid sample. Uptime is never returned as Unix time and no
  placeholder date (such as 1970-01-01) is ever invented.
- `now()` is cheap: it adds the monotonic time elapsed since the accepted sample to that sample. No network, no hardware
  access, no allocation.
- An invalid, implausible or failed sample never replaces a valid anchor.
- Firmware startup never waits for time. Everything else runs unsynchronized and failures are only recorded.
- **Synchronization is not authentication.** Plain NTP is not cryptographically protected, so the time must not be an
  input to security-critical decisions. `TimeStatus` reports synchronization quality, not authenticity. Authenticated
  network time (NTS) is out of scope.

## Consumer API

```rust,ignore
let time = altruist_urban::time::service();

match time.now() {
    Some(unix) => attach_wall_timestamp(unix),   // real, synchronized time
    None => monotonic_only(),                     // never a fabricated date
}

let status = time.status();      // state, source, last_sync_age, uncertainty, consecutive_failures, last_failure
let snapshot = time.snapshot();  // monotonic + wall time of the same instant, plus state
```

| `TimeSyncState` | Meaning |
|-----------------|---------|
| `Unsynchronized` | no valid sample yet; `now()` is `None` |
| `Synchronized` | a valid sample was accepted within the stale threshold |
| `Stale` | the anchor is older than the threshold (`TimeConfig::stale_after`, default 6 h); `now()` still works from the last anchor and may have drifted |

`TimeSource`/`TimeSourceKind` (`Ntp`, `Rtc`, `External`) appear in diagnostics only; consumers do not normally branch
on them. The latest valid sample of any source wins; the policy is deliberately not generalized yet, but the public API
carries no NTP assumption.

## Sources: the `TimeSource` trait

```rust,ignore
trait TimeSource {
    type Error: Into<SyncFailure>;
    fn kind(&self) -> TimeSourceKind;
    fn is_available(&self) -> bool { true }          // e.g. network ready
    async fn sample(&mut self) -> Result<TimeSample, Self::Error>;
}
```

A source only produces samples. A hardware RTC source would implement the same trait (`HardwareRtcSource<R>`), so a board
with an RTC can establish the initial time before the network exists and let NTP refine it later; consumers do not
change. A source must bound its own waits and be cancel-safe.

### NTP source (`rohi-time-ntp`)

`NtpTimeSource` resolves a server with `NetworkHandle` DNS, sends one SNTPv4 datagram over a UDP socket of the same
handle and returns a sample: the server's transmit time plus half the round trip, with the round trip and the server's
root delay/dispersion as uncertainty. It uses the [`sntpc`](https://crates.io/crates/sntpc) client for the protocol
(mode, version, stratum, origin timestamp, leap indicator, kiss-o'-death, root distance and reply address checks); the
result is mapped to coarse `SyncFailure` categories. It rotates through at most `MAX_SERVERS` (4) configured servers,
moving on after a failure, and holds one UDP socket only for the duration of one exchange. DNS and the exchange are
bounded by `NtpConfig::timeout_ms` (5 s). It never owns the global concept of time.

**NTP era.** NTP timestamps wrap every 2³² s (the next wrap is in 2036), so a wire value alone is ambiguous. The source
takes a *pivot*, the best known current time, and picks the era closest to it. In firmware the pivot is
`TimeService::pivot()`: the synchronized time when there is one, otherwise the **firmware build time plus uptime** (not
a timestamp, never recorded). A pivot more than about 68 years off picks the wrong era; such a result is far outside
the plausibility window and is rejected.

## Validation

`TimeConfig` (named, documented values) decides whether a sample may become the anchor:

| Rule | Default |
|------|---------|
| `not_before` (plausibility floor) | firmware build time (`BUILD_TIMESTAMP`, set by the firmware `build.rs`) |
| `not_after` | floor + `DEFAULT_HORIZON` (40 years) |
| `max_uncertainty` | `DEFAULT_MAX_UNCERTAINTY` (5 s) |
| `stale_after` | `DEFAULT_STALE_AFTER` (6 h) |

A rejected sample is recorded as `SyncFailure::Implausible` and the existing anchor is kept.

## Background synchronization

`Synchronizer` runs in one task (`altruist_urban::time::ntp_task`):

- **Network down** (`is_available() == false`): polls every 2 s, **without counting a failure**; the first sample is
  taken as soon as the network returns. Network appearing and disappearing at runtime is expected.
- **Failure** (DNS, timeout, unreachable, invalid response, kiss-o'-death, implausible time): recorded
  (`consecutive_failures`, `last_failure`), retried with bounded exponential backoff, 15 s doubling up to 10 minutes.
  The anchor is untouched; there is no panic.
- **Success**: backoff is reset, the failure counters are cleared and the next sample is taken after 1 hour.
  Oscillator drift is thereby corrected, while polling stays well within NTP etiquette (RFC 4330: never more often than
  every 15 s).

The intervals are `SyncSchedule` fields; `SyncSchedule::DEFAULT` holds the values above.

## Control plane

The `time` [service](service.md) (`TimeStatusService` over `TimeService`, read-only, no secure link needed) reports its
state and, when synchronized, `unix_time` (ms; absent while unsynchronized), `source`, `sync_age`, `uncertainty` and
`consecutive_failures`; the time servers are in the `time` config section, not repeated in the status. An unsynchronized device that has not failed yet is `starting`; after failures it is `running` and
`degraded` with the active issue of the last failure category (`network_unavailable`, `resolve_failed`, `timeout`,
`invalid_response`, `rejected`, `implausible`, `source_failure`). A stale time is usable but `degraded`. The service is not
essential for the device. See [Services and status](../getting-started/services.md):

```text
$ rohi status time
Service:  time
State:    running
Health:   ok
Ready:    yes
Since:    2h 13m

unix_time                1780000000250 ms
source                   ntp
sync_age                 90000 ms
uncertainty              40 ms
consecutive_failures     0
```

The servers are the `time` config section (`NtpServersConfig`, fields `server1` to `server4`: host names or IPv4
addresses of letters, digits, `.` and `-`, up to 64 bytes). The defaults are `pool.ntp.org` and `time.cloudflare.com`
(`server3` and `server4` have no value). `FIELD=null` removes an override and restores the default, so
`server2=null` brings back `time.cloudflare.com`; a field whose default is a server cannot be forced to "none", and
`rohi config reset time` removes every override. A change is applied at once: the NTP task restarts with the new
servers and samples immediately.

```text
$ rohi config time server1=ntp.example.org server2=time.example.net
```

Manual resynchronization is not offered: periodic automatic synchronization is authoritative.

## Resources

All storage is static and bounded: the anchor and counters are a few words behind a critical-section mutex;
`NtpTimeSource` keeps no state besides a server index; one UDP socket (`UDP_SOCKETS = 1`, `UDP_BUFFER = 128` B per
direction) is allocated from the `NetResources` pool for each exchange. See
[Resource limits](../reference/resource-limits.md).

## Testing

Everything runs on the host without Internet access: fake clock, fake sources, and `rohi_net::testing::FakeIp` with a
scripted NTP server (valid and malformed answers, kiss-o'-death, timeouts, DNS failure, unreachable server, era
rollover, implausible time, network loss and recovery). See [Host testing](../validation/host-testing.md).

## Not in scope

A hardware RTC driver, persisted last-known time, clock slewing, time zones and daylight saving, authenticated NTP
(NTS), GPS/PTP, an NTP server. Persistent logging will consume `TimeService::now()` and attach a wall timestamp only
when it is `Some`; it must keep working before the first synchronization.
