# Getting started with a device

This section is for people who operate ROHI devices. If you write firmware, start with [Build Firmware](../build/quick-start.md).

## 1. Check that your device is supported

Look up the board in the [support matrix](../support/matrix.md). It says which firmware targets the board and what has
been verified on hardware. Today that is the Altruist Urban; the Altruist Insight is not supported by ROHI.

## 2. Get a firmware onto the device

The reference firmware for the Altruist Urban is in [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban).
Build and flash it as described in [Quick start](../build/quick-start.md) and [First device](../getting-started/first-device.md).
This repository does not publish pre-built firmware images.

## 3. Connect with a client

A flashed device advertises over Bluetooth LE. The `rohi` desktop application finds it and operates it:

```bash
rohi scan          # find the device
rohi status        # health and every service
rohi config        # the settings the device offers
rohi action        # the operations the device offers
```

| Workflow | Page |
|----------|------|
| Find a device and connect | [Discovery & connection](desktop/discovery.md) |
| Inspect and change settings | [Configuration](desktop/configuration.md) |
| Check health, services and issues | [Status & diagnostics](desktop/status.md) |
| Run device operations | [Actions](desktop/actions.md) |
| Script all of the above | [Automation](desktop/automation.md) |

A shared [mobile application](mobile.md) is planned but not implemented.

## Limits to know about

- The control link is **not encrypted or authenticated yet**: anyone in Bluetooth range can configure the device.
- A device accepts one connection at a time.
- Keep `rohi` and the firmware on the same revision: the control protocol is still changing.
