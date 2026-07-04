# Examples

Each subdirectory is a standalone package with its own `Cargo.toml` and
`src/main.rs`. The lightweight examples are workspace members so they build in
normal CI. The embedded examples are intentionally opt-in because they need
Souffle headers and a C++ compiler during Cargo's build-script phase.
The build-helper examples currently require exactly Souffle `2.4.1`, selected
by the default `souffle-rs-build` feature `souffle-2-4-1`; `Build::compile()`
checks the configured `souffle` binary before generating artifacts.

The main examples cover the schema/build integration matrix:

| Schema source | API/backend shape | Cargo build script | Example |
| --- | --- | --- | --- |
| Hand-written `RelationBundle` | Runtime dynamic API | No | `dynamic-api` |
| Hand-written `RelationBundle` | Generated typed/native API | Yes | `embedded-build` |
| Extracted from Souffle | Artifact export only | No | `auto-schema` |
| Extracted from Souffle | Generated typed/native API | Yes | `embedded-auto-schema` |

Run the dynamic runtime API example:

```bash
cargo run -p souffle-rs-example-dynamic-api
```

Run a build-helper example that extracts schema metadata from Souffle instead
of passing a hand-written `RelationBundle`. It reads
`examples/auto-schema/logic/reachability.dl`, calls `Build::compile()` without
`.schema_bundle(...)`, and prints the generated schema and typed API artifact
paths:

```bash
cargo run -p souffle-rs-example-auto-schema
```

The auto-schema example requires supported Souffle on `PATH` or
`SOUFFLE_RS_SOUFFLE_BIN`.

Print the build-helper plan for a typed Souffle integration. This example reads
`examples/build-plan/logic/reachability.dl`, builds a `Build` configuration, and
prints the planned Souffle command, Cargo directives, and typed API artifact
path without invoking external tools:

```bash
cargo run -p souffle-rs-example-build-plan
```

Run a complete process-backend example. It uses the standalone Datalog source at
`examples/process-backend/logic/reachability.dl`, compiles it with Souffle,
inserts facts through `ProcessProgram`, executes the generated process, and reads
dynamic output rows:

```bash
cargo run -p souffle-rs-example-process-backend
```

The process example requires Souffle on `PATH` or `SOUFFLE_RS_SOUFFLE_BIN`.

Run the full embedded build-script flow explicitly:

```bash
cargo run --manifest-path examples/embedded-build/Cargo.toml
```

That package demonstrates the standard Cargo integration path:

1. `logic/reachability.dl` is the independent Souffle source file.
2. `build.rs` calls `souffle-rs-build` with `out_dir_from_cargo_env()` and
   `BuildProfile::EmbeddedTypedApi`.
3. Souffle emits generated C++ and schema metadata.
4. The build helper emits the C ABI wrapper and typed Rust API.
5. Cargo compiles the generated C++ into a native library.
6. `src/main.rs` uses `souffle_rs::include_generated_programs!()`,
   `EmbeddedProgram`, the generated `schema_bundle()`, and the generated typed
   API.

Run the embedded build-script flow with automatically extracted schema metadata:

```bash
cargo run --manifest-path examples/embedded-auto-schema/Cargo.toml
```

That package demonstrates the same standard Cargo integration path without a
hand-written `.schema_bundle(...)`. `build.rs` uses the Cargo build-script
output API plus `BuildProfile::EmbeddedTypedApi`, extracts schema metadata from
Souffle, emits schema JSON and typed Rust API, compiles the generated C++ into a
native library, and `src/main.rs` uses the generated `schema_bundle()` before
using the generated typed API.

Set `SOUFFLE_RS_SOUFFLE_BIN` and `SOUFFLE_RS_SOUFFLE_INCLUDE` when supported
Souffle is installed outside `PATH` or its headers are not under the same
installation prefix.
