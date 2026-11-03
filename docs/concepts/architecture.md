# Architecture

The SDK is layered. **Portable crates** define typed models, contracts and logic and build and test on a developer
machine. Thin **`*-esp` platform crates** implement those contracts on the ESP32-C3 and ESP32-C6. The **firmware**
composes them and owns the product decisions: which features run and how much RAM each resource gets. Everything sits
on Embassy, `esp-rtos` and `esp-hal` / `esp-radio`; ROHI adds contracts on top, not another runtime.

Five rules keep the layering true:

1. Dependencies point from platform and product towards portable crates, never the other way.
2. A feature crate owns its own configuration sections; `rohi-config` knows none of them.
3. Every stateful service has one owner task and is reached through bounded handles
   ([Runtime ownership](runtime-ownership.md)).
4. Vendor types never cross a portable API ([Portability](portability.md)).
5. The product, not the platform crate, decides the RAM budget ([Memory model](memory-model.md)).

| Layer | Examples | Role |
|-------|----------|------|
| Firmware | `altruist-urban`, `examples` | composition, tasks, resource budget |
| Platform (ESP) | `rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp` | implement the portable seams on the chip |
| Portable | `rohi-hal`, `rohi-config`, `rohi-net`, `rohi-service`, `rohi-control*`, ... | models, contracts, logic |

The full dependency diagram, the data flows (sensors, control requests, configuration) and the "where does new code go"
table are in the rustdoc; the [Crate map](crate-map.md) lists every crate.

## Reference

- [`rohi_sdk::architecture`](https://docs.rs/rohi-sdk/latest/rohi_sdk/architecture/index.html): dependency model,
  rules, data flows. Crate map and "where does new code go": [`rohi_sdk`](https://docs.rs/rohi-sdk/latest/rohi_sdk/index.html).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
