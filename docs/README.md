# Introduction

<img class="right" src="images/robo-hw-logo-black.png" alt="The ROHI logo" />

**ROHI** (Robonomics Open Hardware Initiative) is an open ecosystem for IoT firmware built around a common
**control protocol**. Firmware that implements the protocol can be operated by shared clients, whether it was written
by the ROHI developers, the community or you.

> **One App. Any Firmware.**

The SDK in this repository is the Rust toolkit behind it: libraries, a reference firmware for
[Altruist Urban](https://robonomics.network/devices/altruist/) and the `rohi` desktop client, for devices powered by the
[Robonomics](https://robonomics.network/) network.

## Where to start

| I want to... | Start here |
|--------------|-----------|
| understand what ROHI is and what works today | [Why ROHI?](intro/why-rohi.md), [Current capabilities](intro/current-capabilities.md) |
| operate a ROHI device | [Use a Device](use/getting-started.md): [Desktop application](use/desktop/index.md) |
| know whether my hardware is supported | [Supported devices](support/overview.md) |
| write firmware | [Build Firmware](build/quick-start.md): [Core concepts](build/core-concepts.md), [Create & customize](build/create-and-customize.md) |
| trust or share a firmware | [Verify & Share](verify/compatibility.md) |
| look something up | [Reference](reference/resource-limits.md), the [crate rustdoc](build/sdk-crates.md) |

## How the documentation is organised

| Place | Responsibility |
|-------|----------------|
| **This book** | vision, current capabilities, user journeys, tutorials, navigation |
| **Crate rustdoc** (`cargo doc --no-deps`) | APIs, contracts, ownership, resource constraints, integration examples |
| **Source and protocol definitions** | authoritative schemas, implementations, tests, platform behaviour |
| **[Support matrix](support/overview.md)** | hardware compatibility, firmware support, verification levels and evidence |

The book explains how to use and extend ROHI; the crates explain how ROHI works internally. See
[Documentation style and policy](contributing/docs-style.md).

> The book assumes basic Rust knowledge for the firmware sections. If you are unsure about any language aspects, please
> refer to the [Rust book](https://doc.rust-lang.org/book/).

# Contributing

ROHI SDK is free and open source. You can find the source code on GitHub, where issues and feature requests can be posted on the issue tracker.
The project relies on community contributions to fix bugs and add features - if you'd like to help, please consider opening a pull request.

# License

The ROHI SDK source and documentation are released under the [Apache v2.0](https://www.apache.org/licenses/LICENSE-2.0.txt).
