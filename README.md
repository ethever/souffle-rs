# souffle-rs

Rust embedded runtime and build tooling for Souffle Datalog programs.

This repository is a Rust workspace for three crates:

- `souffle-rs`: safe Rust API for embedded Souffle programs and relations.
- `souffle-rs-sys`: raw C ABI bindings for the C++ wrapper around Souffle generated code.
- `souffle-rs-build`: `build.rs` helper for generating and compiling Souffle C++ code.

The detailed implementation goal is tracked in
[`docs/souffle-rs-rust-crate-goal.md`](docs/souffle-rs-rust-crate-goal.md).

## Examples

The repository includes runnable standalone example packages under
[`examples/`](examples/). The lightweight examples are workspace members; the
embedded build-script example is opt-in because it needs Souffle headers and a
C++ compiler during Cargo's build-script phase:

| Schema source | API/backend shape | Cargo build script | Example |
| --- | --- | --- | --- |
| Hand-written `RelationBundle` | Runtime dynamic API | No | `dynamic-api` |
| Hand-written `RelationBundle` | Generated typed/native API | Yes | `embedded-build` |
| Extracted from Souffle | Artifact export only | No | `auto-schema` |
| Extracted from Souffle | Generated typed/native API | Yes | `embedded-auto-schema` |

```bash
cargo run -p souffle-rs-example-dynamic-api
cargo run -p souffle-rs-example-auto-schema
cargo run -p souffle-rs-example-build-plan
```

`dynamic-api` builds a schema-backed runtime facade, inserts rows, runs the
program facade, and reads printable output through the shared `Program` API.
`auto-schema` reads `examples/auto-schema/logic/reachability.dl`, runs schema
extraction without a hand-written `RelationBundle`, validates the generated
schema JSON, and prints the generated schema and typed API artifact paths.
`build-plan` reads `examples/build-plan/logic/reachability.dl`, shows the
minimal `build.rs` configuration for one Souffle program, and prints the planned
`souffle` command, Cargo directives, and typed Rust API artifact path without
invoking external tools.

For a complete process-backend run, install Souffle or set
`SOUFFLE_RS_SOUFFLE_BIN`, then run:

```bash
cargo run -p souffle-rs-example-process-backend
```

That example uses the standalone Datalog source at
`examples/process-backend/logic/reachability.dl`, compiles it with `souffle -o`,
inserts facts through `ProcessProgram`, runs the generated executable, and reads
the printable relation.

For the full embedded Cargo build flow, run the opt-in example directly:

```bash
cargo run --manifest-path examples/embedded-build/Cargo.toml
cargo run --manifest-path examples/embedded-auto-schema/Cargo.toml
```

`embedded-build` demonstrates `logic/reachability.dl -> build.rs ->
souffle-rs-build -> generated C++/schema/typed API -> native library ->
EmbeddedProgram` with a hand-written schema bundle. `embedded-auto-schema`
demonstrates the same Cargo build-script flow, but lets `souffle-rs-build`
extract schema metadata before emitting the generated typed Rust API used from
`src/main.rs`. Set
`SOUFFLE_RS_SOUFFLE_BIN` and `SOUFFLE_RS_SOUFFLE_INCLUDE` if Souffle is not
discoverable from `PATH` and its install prefix.

`souffle-rs-build` currently supports exactly Souffle `2.4.1`, selected by the
default Cargo feature `souffle-2-4-1`. `Build::compile()` checks
`souffle --version` before schema extraction or code generation and fails if the
configured binary reports a different version. If your workspace disables
default features, re-enable exactly one supported Souffle version feature, for
example:

```toml
souffle-rs-build = { version = "0.1", default-features = false, features = ["souffle-2-4-1"] }
```

Building `souffle-rs-build` with `--no-default-features` and no `souffle-*`
feature is intentionally a compile-time error, because generated C++ metadata
and wrapper compatibility are version-specific.

The safe runtime exposes backend-neutral `PerformanceRecorder` /
`PerformanceMetrics` values for benchmark harnesses. The metrics record total
time, Souffle run time, relation insertion time, relation output decode time,
relation-exchange file count, bytes written, metadata operations, RSS and CPU
utilization when supplied by the harness or sampled from the host, OpenMP thread
count, Rust worker count, and backend type.
Embedded and in-memory relation exchange should report zero relation-exchange
files, bytes, and metadata operations; explicit file or SQLite export paths can
record their durable artifacts through the same metrics shape by scanning the
artifact directory or database path. On Linux, the recorder can sample peak RSS
and core-equivalent CPU utilization from `/proc/self` without adding a benchmark
dependency.

## Build integration

Use `souffle-rs-build` from `build.rs` to plan Souffle generation, emit schema
metadata, compile generated C++ plus the C ABI wrapper, and generate an optional
typed Rust API:

```rust,no_run
use souffle_rs::{
    AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef,
};
use souffle_rs_build::{
    Build, BuildProfile, FunctorLibrary, GeneratedMode, LinkMode, NativeLinkMode,
    OpenMpConfig,
};

# fn main() -> Result<(), souffle_rs_build::BuildError> {
let schema: RelationBundle = [
    RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new("id", TypeRef::Number)],
    ),
    RelationSchema::output(
        RelationId::new(1),
        "Output",
        [AttributeSchema::new("id", TypeRef::Number)],
    ),
]
.into_iter()
.collect();

Build::new()
    .program_with_namespace("analysis", "logic/main.dl", "analysis_ns")
    .souffle_bin("souffle")
    .souffle_include("/opt/souffle/include")
    .generated_mode(GeneratedMode::Directory)
    .define("PROJECT_DIR", "/workspace")
    .include_dir("logic/include")
    .library_dir("native/lib")
    .functor_library(
        FunctorLibrary::new("functors")
            .search_path("native/functors")
            .link_library("z3")
            .link_mode(NativeLinkMode::Dynamic),
    )
    .openmp(OpenMpConfig::enabled("gomp"))
    .link_mode(LinkMode::StaticGeneratedAndConfiguredExternal)
    .profile(BuildProfile::EmbeddedTypedApi)
    .schema_bundle("analysis", schema)
    .compile()?;
# Ok(())
# }
```

The generated metadata records the Souffle binary, entrypoints, macros,
include/library directories, generated namespace, output mode, OpenMP settings,
link mode, native compiler inputs, wrapper/header artifacts, schema artifacts,
typed API artifacts, external libraries, and ABI version.

### Schema metadata

Build integrations have two schema sources:

- Pass an explicit `.schema_bundle(program, schema)` when the Rust build script
  already knows the relation schema. This is the most reliable path for
  production typed API generation because the schema is normal Rust data and is
  validated before artifacts are emitted.
- Omit `.schema_bundle(...)` when schema-dependent artifacts are requested and
  let `souffle-rs-build` extract schema metadata by running
  `souffle --show=transformed-ast`.

Automatic extraction is useful, but it is not a full Souffle parser. The current
extractor reads Souffle's transformed AST output as text: relation `params` and
`types` payloads are parsed as JSON, while surrounding `.decl`, `.input`,
`.output`, and `.type` directives are discovered with line-oriented string
matching and top-level delimiter splitting. That means the extractor can lag
behind Souffle syntax and metadata formatting changes. Subtype chains such as
`.type B <: A` are preserved in extracted schema metadata, but for uncommon type
syntax or schema-critical generated APIs, prefer a hand-written
`RelationBundle` or add tests that assert the extracted schema shape you rely
on.

## Runtime usage

All backends share the same `Program` facade. Embedded programs use the generated
C ABI wrapper and keep relation exchange in memory; process, file, and SQLite
backends remain explicit choices for parity, debugging, export, timeout, and
crash isolation. The process backend uses Souffle's default file I/O boundary:
input facts and output rows are tab-delimited unless the Datalog program
configures another Souffle I/O mode. Delimiter characters in input symbols are
rejected before writing `.facts`; output decoding preserves delimiter characters
where the default record/list/ADT text format remains unambiguous.

The runtime crate exposes five backend features: `embedded`, `process`, `file`,
`memory`, and `sqlite`. Defaults enable `embedded`, `process`, `file`, and
`memory`; `sqlite` is opt-in so applications do not compile SQLite dependencies
unless they ask for SQLite-backed relation storage.

```rust
use souffle_rs::{
    AttributeSchema, Backend, InMemoryProgram, Program, RelationBundle,
    RelationId, RelationSchema, Row, TypeRef, Value,
};

# fn main() -> Result<(), souffle_rs::SouffleError> {
let schema: RelationBundle = [
    RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new("id", TypeRef::Number)],
    ),
    RelationSchema::output(
        RelationId::new(1),
        "Output",
        [AttributeSchema::new("id", TypeRef::Number)],
    ),
]
.into_iter()
.collect();

let mut program = InMemoryProgram::builder("analysis")
    .backend(Backend::Memory)
    .schema(schema)
    .build_memory()?;

program.insert_row("Input", [Value::Number(7)])?;
program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
program.run()?;

let mut output = program.iter_relation("Output")?;
assert_eq!(output.next_row()?.unwrap().values(), &[Value::Number(7)]);
# Ok(())
# }
```

### Large relations

Use streaming APIs for large outputs. `Program::read_relation()` and generated
typed `read()` helpers collect the complete relation into a Rust `Vec`, so prefer
`Program::iter_relation()`, generated `iter_typed()`, `RelationIterator::next_row()`,
and `RelationIterator::next_chunk()` when output size is not bounded.

`Program::insert_row()` is a convenience API, not a bulk ingestion API. The file
and SQLite backends reload and rewrite relation storage for each inserted row,
the SQLite iterator currently performs one indexed query per row, and the
embedded backend still encodes and crosses the C ABI once per input row. For
large fixtures or exports, prefer `replace_relation_rows()`, streaming export
helpers, and embedded/process backends where appropriate. Batch embedded input
and chunked SQLite cursors are future performance work, not current guarantees.

Generated typed APIs wrap the same dynamic runtime core. A generated module can
insert strongly typed input rows, stream typed output rows, and still share
`verify_backend_parity` with process/file/SQLite backends.

## Verification

The repository CI covers formatting, workspace tests, rustdoc examples with
missing-doc warnings, and clippy on Linux GCC, Linux Clang, and macOS Clang. A
native release smoke job installs Souffle, compiles generated C++/wrapper code,
runs process-backend ignored tests, and runs the ignored `native_smoke` suite,
including custom functor dynamic/static link coverage and linked embedded vs
process parity.
