# Summary

[Introduction](./README.md)

- [Why ROHI?](intro/why-rohi.md)
- [One App. Any Firmware.](intro/one-app-any-firmware.md)
- [Current capabilities](intro/current-capabilities.md)

# Use a Device

- [Supported devices](support/overview.md)
  - [Support matrix](support/matrix.md)
- [Getting started](use/getting-started.md)
- [Desktop application](use/desktop/index.md)
  - [Discovery & connection](use/desktop/discovery.md)
  - [Configuration](use/desktop/configuration.md)
  - [Status & diagnostics](use/desktop/status.md)
  - [Actions](use/desktop/actions.md)
  - [Automation](use/desktop/automation.md)
- [Mobile application](use/mobile.md)

# Build Firmware

- [Quick start](build/quick-start.md)
  - [Installation](getting-started/installation.md)
  - [Workspace layout](getting-started/workspace.md)
  - [First device: Air Quality Terminal](getting-started/first-device.md)
  - [Build and test](getting-started/build-and-test.md)
- [Core concepts](build/core-concepts.md)
  - [Config](build/config.md)
  - [Status](build/status.md)
  - [Action](build/action.md)
- [Control protocol](build/control-protocol.md)
- [Create & customize](build/create-and-customize.md)
  - [Customize firmware with AI](build/customize-with-ai.md)
  - [Add a sensor](guides/add-sensor.md)
  - [Add a ConfigSection](guides/add-config-section.md)
  - [Add or support a board](guides/add-board.md)
  - [Use IP networking](guides/use-network.md)
  - [Add a service](guides/add-service.md)
  - [Add a control operation or an action](guides/add-control-operation.md)
- [SDK crates](build/sdk-crates.md)

# Verify & Share

- [Compatibility](verify/compatibility.md)
- [Validation](verify/validation.md)
  - [Host testing](validation/host-testing.md)
  - [Hardware testing](validation/hardware-testing.md)
  - [Resource budget](validation/resource-budget.md)
- [Firmware distribution](verify/distribution.md)

# Technical reference (moving to crate rustdoc)

- [What is the ROHI SDK?](getting-started/overview.md)
- [Architecture](concepts/architecture.md)
- [Crate map](concepts/crate-map.md)
- [Runtime ownership](concepts/runtime-ownership.md)
- [Async and cancellation](concepts/async-and-cancellation.md)
- [Portability](concepts/portability.md)
- [Memory model](concepts/memory-model.md)
- [Versioning and ABI](concepts/versioning-and-abi.md)
- [Sensor HAL (rohi-hal)](sdk/hal.md)
- [Sensors](sdk/sensors.md)
- [Configuration store](sdk/config.md)
- [Networking (rohi-net)](sdk/net.md)
- [Service pipeline (rohi-service)](sdk/service.md)
- [Control plane (BLE)](sdk/control.md)
- [Device identity](sdk/identity.md)
- [Time service](sdk/time.md)
- [Telemetry (rohi-connectivity)](sdk/connectivity.md)
- [Location](sdk/location.md)

# Platform and reference

- [ESP32-C3 and ESP32-C6](platform/esp32.md)
- [Boards](platform/boards.md)
- [Wireless runtime](platform/wireless.md)
- [Resource limits](reference/resource-limits.md)
- [Targets and features](reference/targets-and-features.md)
- [Errors](reference/errors.md)
- [Persistent ABI](reference/persistent-abi.md)
- [Glossary](reference/glossary.md)

# Contributing

- [Documentation style and policy](contributing/docs-style.md)
