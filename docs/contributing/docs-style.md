# Documentation style and policy

## Where things are documented

| Place | Content |
|-------|---------|
| `README.md` | project landing page and the shortest quickstart, with links to this book and the API docs |
| **This book** (mdBook, `docs/`) | product vision, current capabilities, user journeys (Use a Device, Build Firmware, Verify & Share), tutorials and navigation |
| **Rustdoc** (`cargo doc --no-deps`, [docs.rs](https://docs.rs/rohi-sdk)) | the authoritative technical documentation of each crate and of the cross-cutting concepts (`rohi-sdk`): APIs, contracts, ownership, cancellation, resource constraints, integration examples; do not copy API listings into the book |
| Crate and module comments | local implementation contracts: invariants, cancellation semantics, safety |
| Source and protocol definitions | authoritative schemas (`control.proto`), implementations, tests, platform behaviour |
| [Support matrix](../support/overview.md) | hardware compatibility, firmware support, verification levels and evidence |

The book explains how to use and extend ROHI; the crates explain how ROHI works internally. All 19 crates carry their
technical documentation in rustdoc, and the book has **no** technical reference section: no API listings, contracts,
limits tables, ABI or protocol details. Link to the rustdoc instead (see [SDK crates](../build/sdk-crates.md)) and put a
new contract in the crate that owns it.

How to write and validate crate rustdoc (structure, templates, doctest policy, agent workflow, checklist): [Rustdoc authoring guide](rustdoc-for-agents.md).

## One authoritative location per concept

A fact lives on exactly one page; other pages link to it. Canonical pages:

| Concept | Where |
|---------|-------|
| Resource limits and constants | the product constants in `firmwares/altruist-urban/src/esp/resources.rs` and the [`rohi-sdk` memory rustdoc](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html) |
| Persistent ABI rules | the [`rohi-config` rustdoc](https://docs.rs/rohi-config) and the [`rohi-sdk` versioning rustdoc](https://docs.rs/rohi-sdk/latest/rohi_sdk/versioning/index.html) |
| Targets, features and commands | the [`rohi-sdk` portability rustdoc](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features) |
| Control protocol and its limits | the [`rohi-control-proto` rustdoc](https://docs.rs/rohi-control-proto); concept: [Control protocol](../build/control-protocol.md) |
| Cancellation rules | the [`rohi-sdk` cancellation rustdoc](https://docs.rs/rohi-sdk/latest/rohi_sdk/cancellation/index.html) |
| Measurements and sizes | [Resource budget](../validation/resource-budget.md) |
| Supported boards, firmware and verification evidence | [Support matrix](../support/overview.md) |

## Style

- Mark what is **implemented**, **planned** and **not verified** explicitly; never describe a plan as a feature (see the mobile application page).
- Describe the **current** product behaviour and workflows with concrete examples (commands, what a client shows). Keep
  Rust API names, contracts, limits and architecture out of the book: link to the rustdoc instead of pasting them.
- Do not leave obsolete work-package terminology or history in normative pages; history belongs to the commit log and
  the measured-data tables of the validation pages.
- Mark claims that are **not validated on hardware** explicitly ("not yet measured", "not verified on hardware").
- State security limitations plainly (no BLE pairing, no TLS yet).
- Prefer tables for support and verification status, and short paragraphs.
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
