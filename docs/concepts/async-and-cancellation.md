# Async and cancellation

ROHI code is `async` on Embassy. Cancellation (dropping a future before it completes) is normal in async Rust: `select`,
timeouts and task shutdown all do it. SDK code must therefore never leave a service in an undocumented, partially
mutated state when a future is dropped. Tasks are cooperative on one executor, and every stateful service has one owner
task ([Runtime ownership](runtime-ownership.md)), so a caller's dropped future must not stall or corrupt the owner.

**The cancellation rules are authoritatively specified in the `cancellation` module of the `rohi-sdk` rustdoc**
([link below](#reference)); this page does not restate them. In short, the module defines:

| Topic | What it covers |
|-------|----------------|
| Execution model | cooperative executor; a future is dropped only between two `.await` points |
| Primitives | cancellation behaviour of bounded channels, `Signal` / `Watch`, mutexes, numbered responses, `select` |
| Rules for SDK code | document cancellation semantics, no cancellable gap after a durable commit, peripheral transactions, state changes after the last `.await`, release resources in `Drop` |
| Per-API guarantees | the cancellation semantics of each public async API |
| Testing | the host tests that drop futures at the hazardous points |

## Durable mutation

An operation that commits durable state (for example storing a configuration section) must hand the matching runtime
state to its consumer in the same step as the commit, in the task that owns the store. Cancelling before the request was
queued changes nothing; cancelling after it leaves the operation logically completed, because the owner task still
commits and publishes. The guides on [adding a configuration section](../guides/add-config-section.md) and
[adding a control operation](../guides/add-control-operation.md) apply this rule.

## Reference

- [`rohi_sdk::cancellation`](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html): the authoritative
  cancellation rules and per-API guarantees. Overview of all modules: [docs.rs/rohi-sdk](https://docs.rs/rohi-sdk).
- Source: [`crates/sdk`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/sdk).
- Locally: `cargo doc --no-deps --open -p rohi-sdk`.
- Tests that exercise cancellation: [Host testing](../validation/host-testing.md#cancellation-tests).
