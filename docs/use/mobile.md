# Mobile application

> **Not implemented.** This page describes the intended experience of the shared mobile application. No mobile
> application exists in this repository or is published. Everything that works today is the
> [desktop application](desktop/index.md).

The intended application is a generic client of the [control protocol](../build/control-protocol.md): like `rohi`, it knows no
firmware by name and builds its screens from the schemas the device sends.

## Discovery & connection

Connect directly over BLE, without Wi-Fi or a cloud account. The control transport the SDK implements today is BLE, which
is what makes this possible. Pairing and encryption of the link are not implemented yet and would need to exist before
the application is suitable for general use.

## Configuration

Present the settings that the firmware defines: names, types, units, allowed values and secrets are described by the
device, so a new setting appears without an application update. The semantics are those of
[`rohi` configuration](desktop/configuration.md): effective values, overrides and reset.

## Status & diagnostics

Show the health of the device, the state of each service, its metrics and its active issues, as
[`rohi status`](desktop/status.md) does.

## Actions

List the operations the firmware offers and invoke them with typed arguments, using the schemas the firmware provides
(see [Actions](desktop/actions.md)).

## Firmware compatibility

The same application is meant to operate verified firmware and custom compatible firmware. Compatibility is defined by
the control protocol, not by an approval list: see [Compatibility](../verify/compatibility.md).
