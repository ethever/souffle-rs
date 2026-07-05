# Souffle Addon Example

This standalone package demonstrates an embedded Souffle build that links a
small C++ addon library. The addon exports a user-defined functor named
`plus_one`, and `logic/addon.dl` calls it with `@plus_one(value)`.

Run it directly:

```bash
cargo run --manifest-path examples/souffle-addon/Cargo.toml
```

The package intentionally has its own `[workspace]` section because it needs a
local Souffle install, Souffle headers, and a C++ compiler during `build.rs`.
Set `SOUFFLE_RS_SOUFFLE_BIN`, `SOUFFLE_RS_SOUFFLE_INCLUDE`, and `CXX` if they
are not discoverable from `PATH` or the Souffle install prefix.
