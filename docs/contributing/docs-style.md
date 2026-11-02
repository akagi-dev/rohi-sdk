# Documentation style and policy

## Where things are documented

| Place | Content |
|-------|---------|
| `README.md` | project landing page and the shortest quickstart, with links to this book and the API docs |
| **This book** (mdBook, `docs/`) | product vision, current capabilities, user journeys (Use a Device, Build Firmware, Verify & Share), tutorials and navigation |
| **Rustdoc** (`cargo doc --no-deps`) | the authoritative technical documentation of each crate: APIs, contracts, ownership, resource constraints, integration examples; do not copy API listings into the book |
| Crate and module comments | local implementation contracts: invariants, cancellation semantics, safety |
| Source and protocol definitions | authoritative schemas (`control.proto`), implementations, tests, platform behaviour |
| [Support matrix](../support/overview.md) | hardware compatibility, firmware support, verification levels and evidence |

The book explains how to use and extend ROHI; the crates explain how ROHI works internally. Some SDK component and
concept pages still carry technical detail that belongs in the crate rustdoc; they are grouped under
[SDK crates](../build/sdk-crates.md) and are migrated crate by crate, not copied. Do not add new detailed contracts to the
book when the crate can hold them.

## One authoritative location per concept

A fact lives on exactly one page; other pages link to it. Canonical pages:

| Concept | Page |
|---------|------|
| Resource limits and constants | [Resource limits](../reference/resource-limits.md) |
| Persistent ABI rules | [Persistent ABI](../reference/persistent-abi.md) |
| Targets, features and commands | [Targets and features](../reference/targets-and-features.md) |
| Control protocol and its limits | [Control plane](../sdk/control.md); concept: [Control protocol](../build/control-protocol.md) |
| Desktop workflows (`rohi` commands) | [Desktop application](../use/desktop/index.md) |
| Cancellation rules | [Async and cancellation](../concepts/async-and-cancellation.md) |
| Measurements and sizes | [Resource budget](../validation/resource-budget.md) |
| Supported boards, firmware and verification evidence | [Support matrix](../support/overview.md) |

## Style

- Mark what is **implemented**, **planned** and **not verified** explicitly; never describe a plan as a feature (see the mobile application page).
- Describe the **current** architecture with stable source names (`WifiHandle`, `resources.rs`), small diagrams, concrete
  examples and explicit ownership and dependency boundaries. Link to source and rustdoc instead of pasting them.
- Do not leave obsolete work-package terminology or history in normative pages; history belongs to the commit log and
  the measured-data tables of the validation pages.
- Mark claims that are **not validated on hardware** explicitly ("not yet measured", "not verified on hardware").
- State security limitations plainly (no BLE pairing, no TLS yet).
- Prefer tables for limits and error maps, text diagrams for structure, and short paragraphs.
- Examples are compiled where possible: put host-compilable code in a test file, show it with
  `\{{#include path:anchor}}` and mark the block `rust,ignore`; use `text` or `rust,ignore` for hardware-only snippets.

## Update policy

A change must update the SDK book **in the same pull request** when it changes:

- public architecture or crate responsibility,
- public API usage,
- persistent ABI or the wire protocol,
- target support,
- resource defaults,
- the developer workflow.
- a board, a sensor driver, a firmware wiring or a hardware result: update the [support matrix](../support/overview.md) in the same pull request.

Pure internal refactors with unchanged public behaviour do not need documentation churn.

## Checks

```bash
mdbook build docs          # the book builds
mdbook test docs           # plain Rust snippets compile
cargo doc --no-deps        # rustdoc of the host crates builds without warnings
lychee --offline --include-fragments docs/book  # internal links and anchors resolve (CI)
cargo test -p rohi-support-matrix      # support matrix schema, references and generated page
```

CI runs these in the `docs` job (and the support matrix test in the `host` job). A broken internal link is a defect: fix the link or the page, do not exclude it.
