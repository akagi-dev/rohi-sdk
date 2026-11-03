# Time service

`rohi-time` gives the firmware **one** source of wall-clock time and synchronization state, the `TimeService`.
Components that need a real-world timestamp ask it; they never contact an NTP server, manage a clock chip or decide
whether a timestamp is trustworthy. `rohi-time-ntp` is the NTP source and the persistent `time` configuration section
that selects the servers.

The service keeps an *anchor* (an accepted absolute time plus the monotonic instant it was accepted at) and answers
`now()` as anchor plus elapsed monotonic time. `now()` is `None` until the first valid sample: uptime is never reported
as Unix time and no placeholder date is invented. Wall-clock time may **step** when a new sample is accepted, so
durations and ordering must use the monotonic clock. Startup never waits for time. Plain NTP is not authenticated, so
the time must not feed security decisions.

A `Synchronizer` task samples the source in the background with bounded backoff and hourly resynchronization. In the
Altruist Urban firmware it runs an `NtpTimeSource` and restarts when the `time` section changes
([Configuration store](config.md)). The read-only `time` status service is shown by `rohi status time`
([Status & diagnostics](../use/desktop/status.md)). Only the first NTP server is required: `rohi config time
server2=none` disables the second one and `FIELD=null` restores the default
([Configuration](../use/desktop/configuration.md)).

| Item | Role |
|------|------|
| `TimeService`, `TimeConfig` | anchored wall-clock time, plausibility window and stale threshold, `time` status service |
| `TimeSource`, `TimeSample` | trait and sample exchanged between a source and the service |
| `Synchronizer`, `SyncSchedule` | background synchronization, backoff, resync interval |
| `NtpTimeSource`, `NtpConfig` | SNTPv4 over the `rohi-net` DNS and UDP, NTP era resolution, server rotation |
| `NtpServersConfig` | the persistent `time` section (`server1` to `server4`) |

Status: NTP synchronization is implemented and host-tested with a scripted NTP server; it is **not verified on
hardware** ([support matrix](../support/matrix.md)). Only the NTP source exists: `TimeSourceKind::Rtc` and `External`
are reserved, there is no RTC driver, and no logging or datalog consumer exists yet. Resource use is in
[Resource limits](../reference/resource-limits.md).

## Reference

The crate rustdoc is the authoritative technical documentation.

| Crate | docs.rs | Source |
|-------|---------|--------|
| `rohi-time` | <https://docs.rs/rohi-time> | <https://github.com/akagi-dev/rohi-sdk/tree/master/crates/time> |
| `rohi-time-ntp` | <https://docs.rs/rohi-time-ntp> | <https://github.com/akagi-dev/rohi-sdk/tree/master/crates/time-ntp> |

SDK-level concepts: <https://docs.rs/rohi-sdk>. Build the rustdoc locally with
`cargo doc --no-deps --open -p rohi-time` (or `-p rohi-time-ntp`).
