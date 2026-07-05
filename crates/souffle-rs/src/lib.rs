//! Safe Rust API for Souffle Datalog programs.
//!
//! The crate-level API is intentionally backend-neutral. Enabled backend
//! features all target the same value, schema, and error model. The default
//! feature set enables embedded C++, process, file, and pure in-memory relation
//! exchange; enable the `sqlite` feature explicitly for SQLite-backed storage.
//!
//! # Example
//!
//! Build a schema-backed dynamic program facade, insert a loadable row, and
//! stream printable rows through the shared [`Program`] API:
//!
//! ```
//! use souffle_rs::{
//!     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
//!     RelationSchema, Row, TypeRef, Value,
//! };
//!
//! # fn main() -> Result<(), souffle_rs::SouffleError> {
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
//! let mut program = InMemoryProgram::builder("analysis")
//!     .schema(schema)
//!     .build_memory()?;
//!
//! program.insert_row("Input", [Value::Number(1), Value::Symbol("entry".into())])?;
//! program.replace_relation_rows(
//!     "Output",
//!     [Row::new([Value::Number(1), Value::Symbol("entry".into())])],
//! )?;
//! program.run()?;
//!
//! let mut output = program.iter_relation("Output")?;
//! assert_eq!(output.schema().name(), "Output");
//! assert_eq!(output.next_row()?.unwrap().values(), &[
//!     Value::Number(1),
//!     Value::Symbol("entry".into()),
//! ]);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

mod backend;
#[cfg(feature = "embedded")]
mod embedded;
mod error;
#[cfg(feature = "file")]
mod export;
#[cfg(feature = "embedded")]
mod ffi;
mod info;
mod parity;
mod performance;
#[cfg(feature = "process")]
mod process;
mod program;
mod schema;
#[cfg(feature = "sqlite")]
mod sqlite;
mod value;

#[cfg(feature = "process")]
pub use backend::ProcessConfig;
pub use backend::{Backend, CpuBudget, ProgramConfig, RunOptions};
#[cfg(feature = "embedded")]
pub use embedded::EmbeddedProgram;
pub use error::{AbiError, BuildError, LinkError, SouffleError};
#[cfg(feature = "file")]
pub use export::{FileExportManifest, FileProgram, FileRelationArtifact, FileRelationStore};
pub use info::BuildInfo;
pub use parity::verify_backend_parity;
pub use performance::{PerformanceMetrics, PerformanceRecorder};
#[cfg(feature = "process")]
pub use process::ProcessProgram;
#[cfg(feature = "memory")]
pub use program::InMemoryProgram;
pub use program::{Program, ProgramBuilder, RelationIterator, RelationOutput};
pub use schema::{
    AttributeSchema, RelationBundle, RelationHandle, RelationId, RelationKind, RelationSchema,
    TypeRef,
};
#[cfg(feature = "sqlite")]
pub use sqlite::{SqliteProgram, SqliteRelationArtifact, SqliteRelationStore};
pub use value::{Row, Value, ValueKind};

/// Include the generated typed API module index emitted by `souffle-rs-build`.
///
/// This macro is intended for crates whose `build.rs` calls
/// `souffle_rs_build::Build::emit_typed_api_module(true)` and then
/// `compile()`. The build helper publishes the generated module index path
/// through Cargo's `SOUFFLE_RS_TYPED_API_MODULE` compile-time environment
/// variable.
///
/// # Example
///
/// ```ignore
/// mod generated {
///     souffle_rs::include_generated_programs!();
/// }
///
/// use generated::analysis;
///
/// let schema = analysis::schema_bundle()?;
/// ```
#[macro_export]
macro_rules! include_generated_programs {
    () => {
        include!(env!("SOUFFLE_RS_TYPED_API_MODULE"));
    };
}

#[cfg(test)]
mod tests;
