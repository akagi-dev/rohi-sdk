# Boards

## Altruist Urban

`rohi_hal_esp::board::altruist_urban` is the single place that names GPIO pins and configures the buses. The API is
chip independent; the chip is selected by the Cargo feature `esp32c3` or `esp32c6` (exactly one, forwarded by the
firmware), which picks the backend module `board/altruist_urban/esp32c3.rs` or `esp32c6.rs`. Drivers and firmware
tasks never contain pin numbers: `AltruistUrban::new(Hardware::split(peripherals))` returns the configured I²C bus, the
SDS011 UART and the I²S microphone, ready to be handed to the sensor drivers.

| Function | ESP32-C3 | ESP32-C6 |
|----------|----------|----------|
| I²C0 SDA / SCL | GPIO3 / GPIO0 | GPIO3 / GPIO2 |
| SDS011 UART1 TX / RX | GPIO10 / GPIO1 | GPIO5 / GPIO4 |
| I²S BCLK / WS / DIN | GPIO7 / GPIO6 / GPIO8 | GPIO10 / GPIO1 / GPIO11 |

The ESP32-C3 map is verified against the Altruist Urban pinout. The ESP32-C6 map comes from the Altruist Urban main
board schematic (ESP32-C6-WROOM-1-N8, "GPIO Map" sheet) in the hardware repository; it is **not yet verified on
hardware**. The schematic names the SDS011 nets `PM_TX` (GPIO5) and `PM_RX` (GPIO4) without a viewpoint; they are taken
from the ESP32-C6 side, so swap them if the sensor stays silent. GPIO4 and GPIO5 are strapping pins, but they
only select the JTAG source when the `JTAG_SEL_ENABLE` eFuse is burned, so the SDS011 does not affect booting. The C6 board also
wires a button (GPIO7), a data-logging LED (GPIO0) and the CN4 expansion connector that the firmware does not drive.
The firmware drives no sensor power or control pins. A board revision with a different pin map must get its own module
instead of an edit of these tables. Hardware production documentation:
[airalab/hardware](https://github.com/airalab/hardware/tree/main/Altruist), device page:
<https://robonomics.network/devices/altruist/>.
