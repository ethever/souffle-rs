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

```bash
cargo run -p souffle-rs-example-dynamic-api
cargo run -p souffle-rs-example-build-plan
```

`dynamic-api` builds a schema-backed runtime facade, inserts rows, runs the
program facade, and reads printable output through the shared `Program` API.
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
```

It demonstrates `logic/reachability.dl -> build.rs -> souffle-rs-build ->
generated C++/schema/typed API -> native library -> EmbeddedProgram` with the
generated typed Rust API used from `src/main.rs`. Set
`SOUFFLE_RS_SOUFFLE_BIN` and `SOUFFLE_RS_SOUFFLE_INCLUDE` if Souffle is not
discoverable from `PATH` and its install prefix.

`souffle-rs-build` currently supports exactly Souffle `2.4.1`, selected by the
default Cargo feature `souffle-2-4-1`. `Build::compile()` checks
`souffle --version` before schema extraction or code generation and fails if the
configured binary reports a different version.

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
    .build_memory();

program.insert_row("Input", [Value::Number(7)])?;
program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
program.run()?;

let output = program.read_relation("Output")?;
assert_eq!(output.rows()[0].values(), &[Value::Number(7)]);
# Ok(())
# }
```

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
