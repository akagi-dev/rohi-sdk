# One App. Any Firmware.

A ROHI client does not contain knowledge of a particular device. It talks to the device through the **ROHI control
protocol**, and the device describes itself with three kinds of things:

| Concept | The user asks | The firmware provides |
|---------|---------------|-----------------------|
| **Config** | what is the device set up to do? | typed settings with defaults, validation and persistence |
| **Status** | what is it doing now? | live state, health, readiness, metrics and active issues |
| **Action** | what can I ask it to do? | discoverable operations with typed arguments and results |

Because these are described by schemas the device sends, the same client can show the settings of an Altruist Urban
today and those of a firmware that did not exist when the client was built.

```text
 firmware A ─┐                          ┌─ rohi desktop (implemented)
 firmware B ─┼─ control protocol ───────┼─ mobile app    (planned)
 your firmware ┘   over BLE today       └─ your own client
```

## What this gives you

- **Users** keep one tool for every compatible device: [Use a Device](../use/getting-started.md).
- **Firmware authors** get configuration, diagnostics and operations in generic clients for free:
  [Build Firmware](../build/quick-start.md).
- **The community** can publish firmware without a client per firmware, and users can tell what has been checked:
  [Verify & Share](../verify/compatibility.md).

## Implemented and planned

| Piece | State |
|-------|-------|
| Control protocol v1 (protobuf), schema-driven config, status, actions | implemented; still under development, keep client and firmware on the same revision |
| BLE as the control transport | implemented |
| Other transports (for example TCP) | planned: the `tcp://` device URI is parsed but not implemented |
| `rohi` desktop client | implemented |
| Shared mobile application | planned, not implemented: [Mobile application](../use/mobile.md) |
| BLE pairing and encryption | not implemented yet: anyone in radio range can use the control plane |
| Firmware distribution and verification infrastructure | not implemented: [Firmware distribution](../verify/distribution.md) |

See [Current capabilities](current-capabilities.md) for the supported hardware and sensors.
