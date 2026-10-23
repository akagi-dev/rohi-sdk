# Introduction 

<img class="right" src="images/robo-hw-logo-black.png" alt="The ROHI logo" />

**ROHI** (Robonomics Open Hardware Initiative) SDK is a project offering libraries and firmware to build 
IoT devices powered by [Robonomics](https://robonomics.network/) network - a Web3 cloud platform designed to manage IoT devices and robotics.

## SDK Features

- Type- and memory-safe using [Rust Language](https://www.rust-lang.org/)
- Fully async code powered by [Embassy Framework](https://embassy.dev/)
- Secure & decentralized infrastructure on [Robonomics Network](https://robonomics.network/)

## Where to start

- New to ROHI: [What is the ROHI SDK?](getting-started/overview.md), then [Installation](getting-started/installation.md) and
  [First device](getting-started/first-device.md).
- Understand the design: [Architecture](concepts/architecture.md) and [Crate map](concepts/crate-map.md).
- Extend it: the [guides](guides/add-sensor.md).
- Look something up: [Reference](reference/resource-limits.md) and [Validation](validation/host-testing.md).

The book explains architecture, contracts and workflows; exact Rust APIs are in the generated rustdoc
(`cargo doc --no-deps`).

> The following sections assume basic Rust knowledge. If you're unsure about any language aspects, please refer to the [Rust book](https://doc.rust-lang.org/book/).

# Contributing

ROHI SDK is free and open source. You can find the source code on GitHub, where issues and feature requests can be posted on the issue tracker.
The project relies on community contributions to fix bugs and add features - if you'd like to help, please consider opening a pull request.

# License

The ROHI SDK source and documentation are released under the [Apache v2.0](https://www.apache.org/licenses/LICENSE-2.0.txt).
