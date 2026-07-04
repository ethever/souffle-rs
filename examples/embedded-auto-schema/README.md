# Embedded Auto-Schema Example

This is an opt-in standalone package that demonstrates the full embedded flow
without a hand-written schema bundle:

1. `logic/reachability.dl` is the Souffle source.
2. `build.rs` calls `souffle-rs-build` with `out_dir_from_cargo_env()` and
   `BuildProfile::EmbeddedTypedApi`, but without `.schema_bundle(...)`.
3. Souffle generates C++ and transformed AST schema metadata.
4. The build helper emits schema JSON, the C ABI wrapper, and typed Rust API.
5. Cargo compiles the generated C++ into a native library.
6. `src/main.rs` uses `souffle_rs::include_generated_programs!()`, creates
   `EmbeddedProgram` from the generated `schema_bundle()`, and uses the
   generated typed API.

It is not a default workspace member because it requires Souffle headers and a
C++ compiler during Cargo's build-script phase.
The build helper currently supports exactly Souffle `2.4.1`, selected by the
default `souffle-2-4-1` Cargo feature, and checks the configured binary before
generation.

Run it explicitly:

```bash
cargo run --manifest-path examples/embedded-auto-schema/Cargo.toml
```

If supported Souffle is not on `PATH`, set:

```bash
SOUFFLE_RS_SOUFFLE_BIN=/path/to/souffle \
SOUFFLE_RS_SOUFFLE_INCLUDE=/path/to/souffle/include \
cargo run --manifest-path examples/embedded-auto-schema/Cargo.toml
```
