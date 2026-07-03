//! Build helper for `souffle-rs`.
//!
//! The build crate owns deterministic Souffle command planning, Cargo rebuild
//! directives, link configuration, and machine-readable metadata. Native
//! execution is intentionally driven from this typed configuration instead of
//! ad-hoc `build.rs` strings.
//!
//! # Example
//!
//! Configure a `build.rs` path that generates Souffle C++, emits the C ABI
//! header/wrapper plus typed Rust artifacts, and records schema metadata:
//!
//! ```no_run
//! use souffle_rs::{
//!     AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef,
//! };
//! use souffle_rs_build::{
//!     Build, CppStandard, ExternalLibrary, GeneratedMode, LinkMode, NativeLinkMode,
//!     OpenMpConfig,
//! };
//!
//! # fn main() -> Result<(), souffle_rs_build::BuildError> {
//! let schema: RelationBundle = [
//!     RelationSchema::input(
//!         RelationId::new(0),
//!         "Input",
//!         [
//!             AttributeSchema::new("id", TypeRef::Number),
//!             AttributeSchema::new("label", TypeRef::Symbol),
//!         ],
//!     ),
//!     RelationSchema::output(
//!         RelationId::new(1),
//!         "Output",
//!         [
//!             AttributeSchema::new("id", TypeRef::Number),
//!             AttributeSchema::new("label", TypeRef::Symbol),
//!         ],
//!     ),
//! ]
//! .into_iter()
//! .collect();
//!
//! let metadata = Build::new()
//!     .program("analysis", "logic/main.dl")
//!     .souffle_bin("souffle")
//!     .souffle_include("/opt/souffle/include")
//!     .generated_namespace("analysis")
//!     .generated_mode(GeneratedMode::Directory)
//!     .define("PROJECT_DIR", "/workspace")
//!     .include_dir("logic/include")
//!     .library_dir("souffle-addon")
//!     .cpp_standard(CppStandard::Cxx17)
//!     .target_triple("aarch64-apple-darwin")
//!     .openmp(OpenMpConfig::disabled())
//!     .external_library(ExternalLibrary::z3("z3").link_mode(NativeLinkMode::Dynamic))
//!     .link_mode(LinkMode::Dynamic)
//!     .rpath("/opt/souffle/lib")
//!     .install_name("@rpath/libanalysis.dylib")
//!     .emit_c_header(true)
//!     .emit_cxx_wrapper(true)
//!     .emit_schema(true)
//!     .emit_typed_api(true)
//!     .emit_typed_api_module(true)
//!     .schema_bundle("analysis", schema)
//!     .compile()?;
//!
//! assert_eq!(metadata.programs[0].program, "analysis");
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

mod artifacts;
mod config;
mod error;
mod execute;
mod metadata;
mod plan;
mod schema_extract;

pub use config::{
    Build, CppStandard, ExternalLibrary, ExternalLibraryKind, FunctorLibrary, GeneratedMode,
    LinkMode, NativeLinkMode, OpenMpConfig,
};
pub use error::{BuildError, CommandFailure, NativeCompileFailure};
pub use metadata::{
    BuildMetadata, ExternalLibraryMetadata, FunctorMetadata, NativeBuildMetadata, OpenMpMetadata,
    ProgramMetadata,
};
pub use plan::{BuildPlan, CargoDirective, SouffleCommand};

#[cfg(test)]
mod tests;
