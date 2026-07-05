# Audit TODO Outline

This file tracks the short-form follow-up list from the July 2026 project audit.
See [audit-todo-details.md](audit-todo-details.md) for evidence, impact,
status, and suggested or implemented fixes.

## High Priority

No direct memory-safety, command-injection, or remote-code-execution issue was
identified in this pass.

## Medium Priority

- [x] Generated typed API can emit invalid Rust identifiers for non-rawable
      keywords such as `self`, `crate`, `super`, and `Self`.
      Details: [generated Rust identifiers](audit-todo-details.md#generated-rust-identifiers).
- [x] Union/subtype runtime identity is ambiguous for variants with the same
      Souffle ABI/runtime kind.
      Details: [union subtype identity](audit-todo-details.md#union-subtype-identity).
- [x] Process backend cleanup can delete pre-existing `facts/` or `output/`
      directories under a caller-provided work directory.
      Details: [process work directory cleanup](audit-todo-details.md#process-work-directory-cleanup).

## Medium-Low Priority

- [ ] Process backend output parsing is still a custom parser, not an
      authoritative Souffle IO implementation.
      Details: [process IO parser parity](audit-todo-details.md#process-io-parser-parity).
- [x] Hand-written schema metadata can silently overwrite duplicate named type
      definitions.
      Details: [duplicate schema type definitions](audit-todo-details.md#duplicate-schema-type-definitions).

## Low Priority

- [x] Public in-memory builder constructors can panic on invalid user-provided
      schema metadata.
      Details: [panic-based memory constructors](audit-todo-details.md#panic-based-memory-constructors).
- [x] Automatic schema extraction loses subtype-of-subtype hierarchy.
      Details: [subtype hierarchy extraction](audit-todo-details.md#subtype-hierarchy-extraction).
- [x] Large relation paths still have documented performance and memory limits
      that should drive API guidance and future batching work.
      Details: [large relation performance limits](audit-todo-details.md#large-relation-performance-limits).
- [x] `souffle-rs-build` intentionally fails with `--no-default-features`; this
      exact-version feature policy should stay explicit in user-facing docs.
      Details: [version feature compatibility](audit-todo-details.md#version-feature-compatibility).

## Verified Non-Issues

- Lossy generated typed API relation/attribute/variant name collisions are
  deconflicted by the current name allocator.
- ADT `variant_order` metadata is validated when schema JSON is loaded.
- Native embedded enum ADT encoding is implemented and covered by smoke tests.
- Build schema validation runs before Souffle generation and native compilation.
- The reviewed FFI output decode paths guard null pointers before forming Rust
  slices.
