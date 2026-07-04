# Audit TODO Details

This file is the evidence-backed companion to
[audit-todo.md](audit-todo.md). It records known follow-up work found during the
July 2026 audit of the `souffle-rs` runtime, build helper, examples, and CI.

## Generated Rust Identifiers

**Priority:** medium

The generated typed API can still emit invalid Rust for some Souffle names.
`Build::validate()` accepts any identifier-shaped program name, and
`module_identifier()` / `field_name()` raw-prefix every Rust keyword. That works
for keywords such as `type`, but Rust does not allow `r#self`, `r#crate`,
`r#super`, or `r#Self` as ordinary module or field identifiers.

Evidence:

- [`Build::validate()` accepts identifier-shaped program names](../crates/souffle-rs-build/src/config.rs#L1021).
- [`module_identifier()` raw-prefixes every keyword](../crates/souffle-rs-build/src/artifacts.rs#L1141).
- [`field_name()` raw-prefixes every keyword](../crates/souffle-rs-build/src/artifacts.rs#L1175).
- [`rust_keywords()` includes `crate`, `self`, `Self`, and `super`](../crates/souffle-rs-build/src/artifacts.rs#L1205).

Impact:

- A valid Souffle program or attribute name can generate Rust that does not
  compile.
- This is separate from lossy collision deconfliction; collision handling exists,
  but non-rawable keyword handling is incomplete.

Suggested fix:

- Split Rust keyword handling into rawable and non-rawable cases.
- Rename non-rawable keywords with a stable suffix or prefix, then pass them
  through the existing deconfliction path.
- Add generated API compile tests for program names and attributes named
  `self`, `crate`, `super`, and `Self`.

## Union Subtype Identity

**Priority:** medium

Union/subtype values with the same Souffle runtime ABI kind cannot preserve the
exact variant identity through the current runtime boundary.

Evidence:

- Embedded input selects a union variant, then encodes `value.untyped()`.
  See [`encode_input_value`](../crates/souffle-rs/src/embedded/encode.rs#L94).
- Embedded union runtime type selection falls back to the first variant's
  runtime type. See [`declared_runtime_type`](../crates/souffle-rs/src/embedded/encode.rs#L316).
- The generated C++ wrapper chooses the first matching input child. See
  [`pack_input_union_value`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L660).
- Output materialization always decodes a union through `schema.children[0]`.
  See [`materialize_union_value`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L934).
- Rust output decode wraps the result with the union name, not the selected child
  type. See [`decode_output_value`](../crates/souffle-rs/src/embedded/decode.rs#L196).
- The process backend has the same lossy shape when encoding and parsing union
  values. See [`encode_fact_value`](../crates/souffle-rs/src/process/facts.rs#L421)
  and [`parse_output_value`](../crates/souffle-rs/src/process/facts.rs#L612).

Impact:

- For unions of number-like subtypes, callers cannot reliably recover whether a
  value originated as `Small`, `Large`, or another runtime-compatible subtype.
- This may be acceptable if the API documents unions as runtime-kind values, but
  it is incomplete if the goal is schema-visible type identity parity.

Suggested fix:

- Decide whether union variant identity is a supported API contract.
- If supported, carry an explicit discriminant in Rust-side values wherever the
  Souffle runtime representation cannot encode it.
- If unsupported for runtime-compatible variants, document the limitation and
  add tests that lock in the intended behavior.

## Process Work Directory Cleanup

**Priority:** medium

The process backend deletes exchange directories recursively before each run.
Those paths are derived from caller-provided `ProcessConfig::work_dir()`.

Evidence:

- [`prepare_exchange_dir()` removes an existing directory with `remove_dir_all`](../crates/souffle-rs/src/process/facts.rs#L20).
- Process input facts are written under `work_dir/facts`. See
  [`write_input_facts`](../crates/souffle-rs/src/process.rs#L140).
- Process outputs are prepared under `work_dir/output`. See
  [`run_with_options`](../crates/souffle-rs/src/process.rs#L200).
- Process config validation rejects only empty paths. See
  [`validate_process_config`](../crates/souffle-rs/src/process.rs#L253).

Impact:

- If a caller points `work_dir` at an existing directory that already contains
  meaningful `facts/` or `output/` children, the backend deletes those
  subtrees.

Suggested fix:

- Create a private per-run subdirectory under `work_dir`.
- Add an ownership marker before deleting any generated directory.
- Document that process work directories must be scratch directories.

## Process IO Parser Parity

**Priority:** medium-low

The process backend still implements Souffle file exchange parsing manually.
The current parser handles several quoted/composite cases, but it is not an
authoritative Souffle IO parser.

Evidence:

- Output rows are reconstructed by reading lines until the record appears
  complete. See [`ProcessOutputRows::next_row`](../crates/souffle-rs/src/process/facts.rs#L75).
- Field splitting is custom tab/depth scanning. See
  [`split_output_fields`](../crates/souffle-rs/src/process/facts.rs#L248).
- Composite depth tracking is custom quote/escape-aware state. See
  [`OutputSyntax`](../crates/souffle-rs/src/process/facts.rs#L328).
- Input symbols containing top-level fact-file delimiters are rejected. See
  [`encode_symbol`](../crates/souffle-rs/src/process/facts.rs#L865).

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

- [`collect_named_type_definitions()` inserts before returning on duplicate names](../crates/souffle-rs/src/schema.rs#L526).
- [`RelationSchema::validate()` validates against the final collected map](../crates/souffle-rs/src/schema.rs#L1101).
- [`RelationBundle::insert()` is replace-by-name](../crates/souffle-rs/src/schema.rs#L1215),
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

- [`ProgramBuilder::build_memory()` calls `expect()`](../crates/souffle-rs/src/program.rs#L493).
- [`InMemoryProgram::new()` calls `expect()`](../crates/souffle-rs/src/program.rs#L582).

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

- [`parse_type_definitions()` inserts subtypes only when the base is primitive](../crates/souffle-rs-build/src/schema_extract.rs#L194).
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

- [`Program::read_relation()` collects the entire relation](../crates/souffle-rs/src/program.rs#L287).
- [`FileProgram::insert_row()` reloads and rewrites all rows](../crates/souffle-rs/src/export.rs#L497).
- [`SqliteProgram::insert_row()` reloads and rewrites all rows](../crates/souffle-rs/src/sqlite.rs#L406).
- [`SqliteRelationRows::next_row()` performs one query per row](../crates/souffle-rs/src/sqlite.rs#L471).
- Embedded input insertion is per-row FFI. See
  [`EmbeddedProgram::insert_row`](../crates/souffle-rs/src/embedded.rs#L195).
- Embedded iterator chunks materialize up to `max_rows` rows. See
  [`materialize_iterator_chunk`](../crates/souffle-rs-build/src/artifacts/cxx_wrapper.rs#L1161).

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
