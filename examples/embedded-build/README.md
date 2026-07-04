# Embedded Build Example

This is an opt-in standalone package that demonstrates the full embedded flow:

1. `logic/reachability.dl` is the Souffle source.
2. `build.rs` calls `souffle-rs-build` with `out_dir_from_cargo_env()` and
   `BuildProfile::EmbeddedTypedApi`.
3. Souffle generates C++ and schema metadata.
4. The C ABI wrapper and typed Rust API are emitted.
5. The generated C++ is compiled into a native library.
6. `src/main.rs` uses `souffle_rs::include_generated_programs!()`,
   `EmbeddedProgram`, the generated `schema_bundle()`, and the generated typed
   API.

It is not a default workspace member because it requires Souffle headers and a
C++ compiler during Cargo's build-script phase.

Run it explicitly:

```bash
cargo run --manifest-path examples/embedded-build/Cargo.toml
```

If Souffle is not on `PATH`, set:

```bash
SOUFFLE_RS_SOUFFLE_BIN=/path/to/souffle \
SOUFFLE_RS_SOUFFLE_INCLUDE=/path/to/souffle/include \
cargo run --manifest-path examples/embedded-build/Cargo.toml
```
