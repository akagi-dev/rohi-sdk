# Documentation style and policy

## Where things are documented

| Place | Content |
|-------|---------|
| `README.md` | project landing page and the shortest quickstart, with links to this book and the API docs |
| **This book** (mdBook, `docs/`) | architecture, concepts, SDK component overviews, guides, platform notes, reference, validation |
| **Rustdoc** (`cargo doc --no-deps`) | exact Rust API documentation; do not copy API listings into the book |
| Crate and module comments | local implementation contracts: invariants, cancellation semantics, safety |

## One authoritative location per concept

A fact lives on exactly one page; other pages link to it. Canonical pages:

| Concept | Page |
|---------|------|
| Resource limits and constants | [Resource limits](../reference/resource-limits.md) |
| Persistent ABI rules | [Persistent ABI](../reference/persistent-abi.md) |
| Targets, features and commands | [Targets and features](../reference/targets-and-features.md) |
| Control protocol and its limits | [Control plane](../sdk/control.md) |
| Cancellation rules | [Async and cancellation](../concepts/async-and-cancellation.md) |
| Measurements and sizes | [Resource budget](../validation/resource-budget.md) |

## Style

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

Pure internal refactors with unchanged public behaviour do not need documentation churn.

## Checks

```bash
mdbook build docs          # the book builds
mdbook test docs           # plain Rust snippets compile
cargo doc --no-deps        # rustdoc of the host crates builds without warnings
lychee --offline docs/book # internal links and anchors resolve (CI)
```

CI runs these in the `docs` job. A broken internal link is a defect: fix the link or the page, do not exclude it.
