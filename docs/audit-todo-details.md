# Audit TODO Details

This file is the evidence-backed companion to
[audit-todo.md](audit-todo.md). It records resolved and outstanding follow-up
work found during the July 2026 audit of the `souffle-rs` runtime, build helper,
examples, and CI.

## Generated Rust Identifiers

**Priority:** medium

**Status:** fixed.

The generated typed API previously emitted invalid Rust for some Souffle names.
`Build::validate()` accepts any identifier-shaped program name, and the old
identifier mapping raw-prefixed every Rust keyword. That works for keywords such
as `type`, but Rust does not allow `r#self`, `r#crate`, `r#super`, or `r#Self`
as ordinary module or field identifiers.

Evidence:

- [`Build::validate()` accepts identifier-shaped program names](../crates/souffle-rs-build/src/config.rs#L1021).
- [`module_identifier()` now routes generated module names through `rust_identifier`](../crates/souffle-rs-build/src/artifacts.rs#L1141).
- [`field_name()` now routes generated field names through `rust_identifier`](../crates/souffle-rs-build/src/artifacts.rs#L1170).
- [`rust_identifier()` suffixes non-rawable identifiers and raw-prefixes rawable keywords](../crates/souffle-rs-build/src/artifacts.rs#L1207).
- [`generated_typed_api_deconflicts_non_rawable_rust_identifiers` covers the regression](../crates/souffle-rs-build/src/tests.rs#L1667).

Impact before fix:

- A valid Souffle program or attribute name can generate Rust that does not
  compile.
- This is separate from lossy collision deconfliction; collision handling exists,
  but non-rawable keyword handling is incomplete.

Implemented fix:

- Split Rust keyword handling into rawable and non-rawable cases.
- Rename non-rawable keywords with a stable suffix, then pass them
  through the existing deconfliction path.
- Add generated API compile tests for program names and attributes named
  `self`, `crate`, `super`, and `Self`.

## Union Subtype Identity

**Priority:** medium

**Status:** fixed for the Rust runtime/API boundary.

Union/subtype values with the same Souffle runtime ABI kind previously could not
preserve the exact variant identity through embedded or process backend IO. The
Souffle runtime representation for same-kind subtype unions is still the same
underlying scalar, so the fix carries schema-visible identity as Rust/ABI
metadata and as nested `Value::typed` wrappers.

Evidence:

- [`SouffleRsValue` ABI v6 carries an optional `declared_type`](../crates/souffle-rs-sys/src/lib.rs#L170).
- Embedded input attaches the selected union variant's declared type after
  encoding the scalar/runtime value. See [`encode_input_value`](../crates/souffle-rs/src/embedded/encode.rs#L88).
- Embedded output decode consults ABI `declared_type` before falling back to
  runtime-kind matching. See [`decode_union_value`](../crates/souffle-rs/src/embedded/decode.rs#L263).
- The generated C++ wrapper consumes input `declared_type` for union variant
  selection. See [`pack_input_union_value`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L668).
- The generated C++ wrapper annotates output wrapper values with their schema
  type. See [`materialize_schema_value`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L976).
- The process backend preserves nested union variant wrappers when encoding and
  parsing. See [`encode_fact_value`](../crates/souffle-rs/src/process/facts.rs#L456)
  and [`parse_output_value`](../crates/souffle-rs/src/process/facts.rs#L622).
- Regression tests cover embedded declared-type encoding and process backend
  round-trips. See
  [`embedded_union_input_rows_encode_selected_declared_type`](../crates/souffle-rs/src/tests.rs#L1365)
  and
  [`process_backend_preserves_declared_identity_for_subtype_and_union_io`](../crates/souffle-rs/src/tests.rs#L792).

Impact before fix:

- For unions of number-like subtypes, callers cannot reliably recover whether a
  value originated as `Small`, `Large`, or another runtime-compatible subtype.
- This may be acceptable if the API documents unions as runtime-kind values, but
  it is incomplete if the goal is schema-visible type identity parity.

Implemented fix:

- Preserve selected variant identity in Rust values and in the C ABI whenever
  the Souffle runtime value alone cannot distinguish same-kind variants.
- Reject typed union values whose inner declared wrapper does not name one of the
  union variants.
- Retain runtime-kind fallback for older or unannotated ABI values.

## Process Work Directory Cleanup

**Priority:** medium

**Status:** fixed.

The process backend previously deleted exchange directories recursively before
each run. Those paths are derived from caller-provided
`ProcessConfig::work_dir()`, so a caller could lose pre-existing `facts/` or
`output/` contents by reusing a directory that was not exclusively managed by
`souffle-rs`.

Evidence:

- [`prepare_exchange_dir()` now uses a managed marker and refuses non-empty unmanaged directories](../crates/souffle-rs/src/process/facts.rs#L22).
- Process input facts are written under `work_dir/facts`. See
  [`write_input_facts`](../crates/souffle-rs/src/process.rs#L140).
- Process outputs are prepared under `work_dir/output`. See
  [`run_with_options`](../crates/souffle-rs/src/process.rs#L200).
- Process config validation rejects only empty paths. See
  [`validate_process_config`](../crates/souffle-rs/src/process.rs#L253).
- Regression tests assert that unmanaged `facts/` and `output/` contents are not
  deleted. See
  [`process_backend_refuses_unmanaged_facts_directory`](../crates/souffle-rs/src/tests.rs#L652)
  and
  [`process_backend_refuses_unmanaged_output_directory`](../crates/souffle-rs/src/tests.rs#L703).

Impact before fix:

- If a caller points `work_dir` at an existing directory that already contains
  meaningful `facts/` or `output/` children, the backend deletes those
  subtrees.

Implemented fix:

- Add an ownership marker before deleting any generated directory.
- Refuse to remove non-empty unmanaged exchange directories.
- Allow adoption of empty exchange directories and cleanup of directories that
  already carry the `souffle-rs` managed marker.

## Process IO Parser Parity

**Priority:** medium-low

The process backend still implements Souffle file exchange parsing manually.
The current parser handles several quoted/composite cases, but it is not an
authoritative Souffle IO parser.

Evidence:

- Output rows are reconstructed by reading lines until the record appears
  complete. See [`ProcessOutputRows::next_row`](../crates/souffle-rs/src/process/facts.rs#L120).
- Field splitting is custom tab/depth scanning. See
  [`split_output_fields`](../crates/souffle-rs/src/process/facts.rs#L292).
- Composite depth tracking is custom quote/escape-aware state. See
  [`OutputSyntax`](../crates/souffle-rs/src/process/facts.rs#L373).
- Input symbols containing top-level fact-file delimiters are rejected. See
  [`encode_symbol`](../crates/souffle-rs/src/process/facts.rs#L946).

Impact:

- Smoke tests cover important cases, but the process backend can still diverge
  from Souffle's official IO semantics for less common escaping or composite
  output forms.

Suggested fix:

- Add authoritative fixture tests generated by Souffle itself for symbols,
  records, lists, ADTs, quotes, escapes, tabs, and multiline values.
- Prefer an official parser or exact Souffle-compatible parser if one is
  available to the Rust side.

## Duplicate Schema Type Definitions

**Priority:** medium-low

Hand-written schema metadata can define the same named type more than once with
different structures, and the collector silently overwrites the previous entry.

Evidence:

- [`collect_named_type_definitions()` inserts before returning on duplicate names](../crates/souffle-rs/src/schema.rs#L513).
- [`RelationSchema::validate()` validates against the final collected map](../crates/souffle-rs/src/schema.rs#L1120).
- [`RelationBundle::insert()` is replace-by-name](../crates/souffle-rs/src/schema.rs#L1236),
  which is intentional for explicit mutation but means `FromIterator` also keeps
  the last relation with a duplicated name.

Impact:

- A manually constructed or externally generated schema can be internally
  inconsistent but still validate, depending on insertion order.

Suggested fix:

- Change named type collection to reject duplicate names unless the definitions
  are structurally identical.
- Add validation tests for duplicate ADT, subtype, union, and relation names.

## Panic-Based Memory Constructors

**Priority:** low

The in-memory backend exposes constructors that panic on invalid schema even
though fallible alternatives exist.

Evidence:

- [`ProgramBuilder::build_memory()` calls `expect()`](../crates/souffle-rs/src/program.rs#L495).
- [`InMemoryProgram::new()` calls `expect()`](../crates/souffle-rs/src/program.rs#L583).

Impact:

- Hand-written schema is a supported user path, so invalid schema can panic in
  public API code instead of returning a typed error.

Suggested fix:

- Prefer documenting `try_build_memory()` and `try_new()` as primary APIs.
- Consider deprecating the panic constructors or renaming them to make the panic
  contract explicit.

## Subtype Hierarchy Extraction

**Priority:** low

Automatic schema extraction records only subtype definitions whose base is a
primitive type. Souffle accepts subtype chains such as `.type B <: A`, but the
extractor loses that hierarchy.

Evidence:

- [`type_definitions()` inserts subtypes only when the base is primitive](../crates/souffle-rs-build/src/schema_extract.rs#L178).
- [`declared_scalar_type()` falls back to `TypeRef::Declared` when the subtype is missing](../crates/souffle-rs-build/src/schema_extract.rs#L568).

Impact:

- Runtime kind remains usable, but schema introspection loses the declared
  subtype chain.

Suggested fix:

- Store subtype bases by name as well as primitive runtime bases.
- Resolve subtype chains recursively with cycle detection.
- Add extraction tests for subtype-of-subtype programs.

## Large Relation Performance Limits

**Priority:** low

Large relation support depends on callers choosing streaming APIs and avoiding
debug/export backends for bulk insertion.

Evidence:

- [`Program::read_relation()` collects the entire relation](../crates/souffle-rs/src/program.rs#L293).
- [`FileProgram::insert_row()` reloads and rewrites all rows](../crates/souffle-rs/src/export.rs#L497).
- [`SqliteProgram::insert_row()` reloads and rewrites all rows](../crates/souffle-rs/src/sqlite.rs#L406).
- [`SqliteRelationRows::next_row()` performs one query per row](../crates/souffle-rs/src/sqlite.rs#L472).
- Embedded input insertion is per-row FFI. See
  [`EmbeddedProgram::insert_row`](../crates/souffle-rs/src/embedded.rs#L195).
- Embedded iterator chunks materialize up to `max_rows` rows. See
  [`materialize_iterator_chunk`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L1194).

Impact:

- Large outputs can consume memory unexpectedly if callers use `read_relation()`.
- File and SQLite backends are unsuitable for row-by-row ingestion of large
  inputs.
- Embedded throughput is still bounded by per-row encoding and FFI calls.

Suggested fix:

- Keep rustdoc guidance focused on `iter_relation()`, `next_row()`, and
  `next_chunk()` for large outputs.
- Add batch insert APIs for embedded input paths.
- Add chunked SQLite output queries or a held cursor/prepared statement.

## Version Feature Compatibility

**Priority:** low

`souffle-rs-build` intentionally requires an exact supported Souffle version
feature. Building it with `--no-default-features` fails at compile time.

Evidence:

- [`compile_error!` requires `souffle-2-4-1`](../crates/souffle-rs-build/src/config.rs#L29).
- The default feature enables the supported version in
  [`crates/souffle-rs-build/Cargo.toml`](../crates/souffle-rs-build/Cargo.toml#L9).

Impact:

- This matches the current exact-version support policy, but tools or consumers
  that blanket-disable default features may see a compile error instead of a
  normal optional dependency reduction.

Suggested fix:

- Keep the exact-version compile error if that remains the support policy.
- Document that `souffle-rs-build` must be built with exactly one supported
  `souffle-*` feature enabled.
- Add future version features as explicit compatibility work when Souffle
  support expands.

## Verified Non-Issues

The audit also checked several previously suspected problems that are now fixed
or not applicable:

- Generated typed API lossy Rust-name collisions are deconflicted by
  `TypedApiNames` and `unique_identifier`.
- ADT `variant_order` validation rejects missing or inconsistent ordered variant
  metadata when schema JSON is loaded.
- Native embedded enum ADT encoding records `is_enum` and uses bare branch IDs
  for enum ADTs.
- Build schema bundles are resolved and validated before Souffle generation and
  native compilation.
- The reviewed FFI decode paths guard null pointers before forming Rust slices
  and handle zero-length output buffers without calling `slice::from_raw_parts`
  on null pointers.
