# souffle-rs

Rust embedded runtime and build tooling for Souffle Datalog programs.

This repository is a Rust workspace for three crates:

- `souffle-rs`: safe Rust API for embedded Souffle programs and relations.
- `souffle-rs-sys`: raw C ABI bindings for the C++ wrapper around Souffle generated code.
- `souffle-rs-build`: `build.rs` helper for generating and compiling Souffle C++ code.

The detailed implementation goal is tracked in
[`docs/souffle-rs-rust-crate-goal.md`](docs/souffle-rs-rust-crate-goal.md).
