use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Backend, RelationBundle};

/// Runtime-visible build inventory for one generated program facade.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, Backend, BuildInfo, RelationBundle, RelationId,
///     RelationSchema, TypeRef,
/// };
///
/// let schema: RelationBundle = [RelationSchema::output(
///     RelationId::new(0),
///     "Output",
///     [AttributeSchema::new("id", TypeRef::Number)],
/// )]
/// .into_iter()
/// .collect();
///
/// let info = BuildInfo::new("analysis", Backend::Embedded, 5, schema)
///     .with_metadata_path("target/souffle-rs/build-metadata.json");
///
/// assert_eq!(info.program(), "analysis");
/// assert_eq!(info.backend(), Backend::Embedded);
/// assert_eq!(info.schema_bundle().len(), 1);
/// assert!(info.metadata_path().is_some());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    program: String,
    backend: Backend,
    abi_version: u32,
    schema: RelationBundle,
    metadata_path: Option<PathBuf>,
}

impl BuildInfo {
    /// Create build info from runtime-owned schema and ABI metadata.
    pub fn new(
        program: impl Into<String>,
        backend: Backend,
        abi_version: u32,
        schema: RelationBundle,
    ) -> Self {
        Self {
            program: program.into(),
            backend,
            abi_version,
            schema,
            metadata_path: None,
        }
    }

    /// Attach the machine-readable metadata path produced by build tooling.
    pub fn with_metadata_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.metadata_path = Some(path.into());
        self
    }

    /// Generated program name.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Runtime backend that produced this info.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// C ABI version expected by the safe Rust facade.
    pub fn abi_version(&self) -> u32 {
        self.abi_version
    }

    /// Schema bundle used by runtime validation and decoding.
    pub fn schema_bundle(&self) -> &RelationBundle {
        &self.schema
    }

    /// Optional path to build helper metadata.
    pub fn metadata_path(&self) -> Option<&Path> {
        self.metadata_path.as_deref()
    }
}
