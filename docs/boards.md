# Boards

## Altruist Urban (ESP32-C3)

`crates/rohi-hal/src/board/altruist_c3.rs` is the single place that names GPIO pins and configures the buses
(feature `esp`). Drivers and firmware tasks never contain pin numbers: `AltruistC3::new(Hardware::split(peripherals))`
returns the configured I²C bus, the SDS011 UART and the I²S microphone, ready to be handed to the sensor drivers.

| Function | Pin |
|----------|-----|
| I²C0 SDA / SCL | GPIO3 / GPIO0 |
| SDS011 UART1 TX / RX | GPIO10 / GPIO1 |
| I²S BCLK / WS / DIN | GPIO7 / GPIO6 / GPIO8 |

The pin map is verified against the Altruist Urban pinout; the source is recorded in the header of the board file. The
firmware drives no sensor power or control pins. A board revision with a different pin map must get its own module
instead of an edit of this table. Hardware production documentation:
[airalab/hardware](https://github.com/airalab/hardware/tree/main/Altruist), device page:
<https://robonomics.network/devices/altruist/>.
