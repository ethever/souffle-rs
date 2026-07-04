//! Safe Rust API for Souffle Datalog programs.
//!
//! The crate-level API is intentionally backend-neutral. Embedded C++,
//! process/file, SQLite, and pure in-memory relation exchange all target the
//! same value, schema, and error model.
//!
//! # Example
//!
//! Build a schema-backed dynamic program facade, insert a loadable row, and
//! read printable rows through the shared [`Program`] API:
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
//!     .build_memory();
//!
//! program.insert_row("Input", [Value::Number(1), Value::Symbol("entry".into())])?;
//! program.replace_relation_rows(
//!     "Output",
//!     [Row::new([Value::Number(1), Value::Symbol("entry".into())])],
//! )?;
//! program.run()?;
//!
//! let output = program.read_relation("Output")?;
//! assert_eq!(output.schema().name(), "Output");
//! assert_eq!(output.rows().len(), 1);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

mod backend;
mod embedded;
mod error;
mod export;
mod ffi;
mod info;
mod parity;
mod performance;
mod process;
mod program;
mod schema;
mod sqlite;
mod value;

pub use backend::{Backend, CpuBudget, ProcessConfig, ProgramConfig, RunOptions};
pub use embedded::EmbeddedProgram;
pub use error::{AbiError, BuildError, LinkError, SouffleError};
pub use export::{FileExportManifest, FileProgram, FileRelationArtifact, FileRelationStore};
pub use info::BuildInfo;
pub use parity::verify_backend_parity;
pub use performance::{PerformanceMetrics, PerformanceRecorder};
pub use process::ProcessProgram;
pub use program::{InMemoryProgram, Program, ProgramBuilder, RelationIterator, RelationOutput};
pub use schema::{
    AttributeSchema, RelationBundle, RelationHandle, RelationId, RelationKind, RelationSchema,
    TypeRef,
};
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
