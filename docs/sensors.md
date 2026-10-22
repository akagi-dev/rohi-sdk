# Sensors

Supported by the Altruist Urban firmware (ESP32-C3 and ESP32-C6). Each driver is hardware independent and publishes typed
`rohi-hal` observations; see [Sensor HAL (rohi-hal)](rohi-hal.md) for lifecycle, timing and events.

| Sensor | Bus | Cadence | Notes |
|--------|-----|---------|-------|
| BMP280 / BME280 | I²C `0x76` / `0x77` | 10 s | Temperature, pressure, humidity (BME280 only) |
| SCD40 / SCD41 | I²C `0x62` | 5 s | CO₂, temperature, humidity |
| SDS011 | UART, 9600 baud | 125–150 s duty cycle | PM2.5, PM10, fan off between cycles |
| I²S microphone | I²S | 5 s window | Noise level, dB SPL |

## BMP280 / BME280

Forced mode: temperature ×2, pressure ×4, humidity ×1, IIR filter off, one conversion (≈19 ms) per sample; the sensor
sleeps in between.

**Address.** The driver probes `0x76` first, then `0x77`, and uses the first address whose chip ID register reads a
BMP280 (`0x56..=0x58`) or a BME280 (`0x60`); devices with another ID are skipped. Either strapping works without a
rebuild. If valid sensors answer at both addresses, `0x76` wins (two BMx280 are not supported). Probing is repeated on
every re-init, so a sensor moved to the other address is found again. If nothing is found, the failure is a bus error,
an unsupported chip ID or no device, in that order of precedence; the last two keep the sensor `NotDetected`.

## SCD40 / SCD41

Probing stops any running measurement, reads and validates the serial number, then starts periodic measurement (a new
value every 5 s). Each sample waits on the data-ready flag (poll every 500 ms, at most 20 polls) and validates every
word with CRC-8. The first valid value arrives about 5 s after start, until then the sensor is `WarmingUp`.

## SDS011

The command flow and wire protocol (`sensors/sds011/protocol.rs`) are derived from the community
[`sds011-rs`](https://github.com/kenohassler/sds011-rs) driver (MIT OR Apache-2.0). The sensor is kept in query mode
and asleep between cycles, so the fan and laser run only while measuring:

```text
wake -> warm-up (30 s) -> sample 1..N -> filter / aggregate -> publish PM2.5 / PM10 -> sleep (90 s)
```

Constants (`sensors/sds011/mod.rs`), also available as `sds011::Config`:

| Constant | Default | Meaning |
|----------|---------|---------|
| `WARMUP_MS` | 30 s | Fan spin-up before sampling |
| `SAMPLE_COUNT` | 5 | Valid samples required per cycle |
| `SAMPLE_INTERVAL_MS` | 1.5 s | Pause between two queries |
| `CYCLE_TIMEOUT_MS` | 30 s | Sampling window |
| `TRIM_MIN_MAX` | on | Discard the lowest and highest sample |
| `SLEEP_MS` | 90 s | Pause between cycles (the poll interval) |

These values are a starting point and are to be confirmed on hardware against the legacy firmware behaviour.

* **Valid samples only.** Malformed, truncated and checksum-invalid frames and values above 999.9 µg/m³ are dropped.
  Sampling stops when `SAMPLE_COUNT` valid samples exist or the window is over.
* **Aggregation** (`sds011/aggregate.rs`, fixed arrays, no heap): rounded integer mean per channel; with at least
  5 samples the lowest and highest are discarded first, independently for PM2.5 and PM10. Example, PM2.5
  `10 12 11 80 9`: drop `9` and `80`, result `11`.
* **Failure.** Too few valid samples give `Error::InsufficientSamples` and nothing is published. A silent sensor
  fails the cycle at once (`NoResponse`). Either way the sensor is told to sleep and the runner retries after its
  recovery backoff.
* **Robustness.** Every UART read is bounded to 5 s, stale bytes (e.g. frames of a sensor still in active mode) are
  discarded, unexpected frames are skipped and the wake-up is retried up to 3 times.
* The first measurement starts right after boot; the cycle's own warm-up is the only wait.

## Noise

Metric: **sound pressure level in dB SPL (re 20 µPa), unweighted**. It is *not* dBA and no A-weighting is applied.

| Property | Value |
|----------|-------|
| Microphone | ICS-43434 assumed: −26 dBFS at 94 dB SPL (1 kHz), so 24-bit full scale is 120 dB SPL |
| Sample rate | 16 kHz mono, 24-bit samples |
| Filtering | ≈10 Hz integer DC blocker only, otherwise flat |
| Sub-window | 125 ms mean square gives one level |
| Measurement window | 5 s (40 sub-windows), continuous; *average* is the energy average (`Leq`), *maximum* the loudest sub-window |
| Calibration | factory sensitivity only (`CALIBRATION_OFFSET_DB`), no per-device calibration |

Levels include the microphone's frequency response and the device's own noise (e.g. the SDS011 fan). They compare
between devices with the same microphone model, not with a calibrated, A-weighted sound level meter. This is the v1
metric the telemetry schema can rely on; an A-weighted level would be added as a separate, tested metric under its own
name.

Raw audio is never stored: a 256-sample stack chunk plus the 8 KiB DMA ring buffer (≈128 ms) are the only buffers.
