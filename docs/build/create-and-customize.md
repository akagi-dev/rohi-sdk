# Create & customize

A firmware is a composition of SDK components plus your own. These guides show how to add each kind of capability; every
one of them becomes visible to generic clients without changing a client.

| I want to add... | Guide | Visible to clients as |
|------------------|-------|-----------------------|
| a sensor | [Add a sensor](../guides/add-sensor.md) | status fields of the `sensors` service |
| a setting | [Add a ConfigSection](../guides/add-config-section.md) | a section in `rohi config` |
| a runtime part with state | [Add a service](../guides/add-service.md) | a service in `rohi status` |
| an operation | [Add a control operation or an action](../guides/add-control-operation.md) | an action in `rohi action` |
| networking | [Use IP networking](../guides/use-network.md) | the `network` / `wifi` services |
| another board | [Add or support a board](../guides/add-board.md) | a new row in the [support matrix](../support/matrix.md) |

## With an AI assistant

Every guide above can be driven by an AI coding assistant. [Customize firmware with AI](customize-with-ai.md) explains the
workflow, what to give the assistant and ready-to-use prompts for each kind of change.

## The path from code to a client

1. Write the portable logic in a feature crate and test it on the host ([Host testing](../validation/host-testing.md)).
2. Describe it: a config section for its settings, a service for its status and, if it has operations, its actions.
3. Register it in the firmware (`firmwares/altruist-urban`: `config.rs` for sections, `control.rs` for services).
4. Build for the chip and flash it ([Quick start](quick-start.md)).
5. Open it with `rohi`: `rohi status`, `rohi config`, `rohi action` show it.
6. Record what you verified in the [support matrix](../support/overview.md#maintenance-policy).

## Complete examples

- [`firmwares/altruist-urban`](https://github.com/akagi-dev/rohi-sdk/tree/master/firmwares/altruist-urban): the reference firmware.
- [`examples/`](https://github.com/akagi-dev/rohi-sdk/tree/master/examples/src/bin): one program per sensor and a configuration self-test.
- [Location](https://docs.rs/rohi-location): a small, complete service with a section, status and telemetry.

For the contracts behind each step, follow the links to the SDK component pages and the crate rustdoc in
[SDK crates](sdk-crates.md).
