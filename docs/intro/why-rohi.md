# Why ROHI?

Most connected devices ship with a firmware, an app and a cloud that only work together. Changing one means rebuilding
the others, and a firmware written by someone else cannot be operated with the tools you already have.

ROHI separates the three:

```text
 Applications        rohi (desktop), mobile app (planned)
      │               generic: they know no firmware by name
      ▼
 Control protocol    config · status · action, discovered from the device
      │               transport independent, BLE implemented
      ▼
 Firmware            built from reusable components, any firmware that speaks the protocol
```

- **Reusable firmware components.** Sensor drivers, a power-fail-safe configuration store, Wi-Fi and IP networking,
  time, telemetry, device identity and a BLE control plane are libraries with documented contracts. A firmware is a
  composition of them; [Altruist Urban](../platform/boards.md) is the reference.
- **Shared applications.** A client learns what a device offers from the device itself: its settings, its live status
  and its operations. A new firmware module shows up in the client without changing the client.
- **AI-assisted customization.** Because the pieces, their contracts and their support status are written down, in
  rustdoc, in the book and in the machine-readable [support matrix](../support/overview.md), an assistant can adapt a
  firmware without guessing what is verified.
- **Local first.** Configuring and operating a device needs Bluetooth and no cloud account. Robonomics network
  integration (signed telemetry, identity) is an addition, not a prerequisite.

## What ROHI is not

ROHI does not introduce another RTOS (it is built on [Embassy](https://embassy.dev/) and `esp-hal`), it does not require
a central approval of firmware, and it makes no hardware support claim that the
[support matrix](../support/overview.md) cannot back with evidence.

Next: [One App. Any Firmware.](one-app-any-firmware.md).
