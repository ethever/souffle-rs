use std::fmt;

use thiserror::Error;

/// Full diagnostic for a failed external build command.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandFailure {
    /// Logical Souffle program being generated or compiled.
    pub program: String,
    /// Full command line used for reproduction.
    pub command: String,
    /// Working directory of the failed command.
    pub working_dir: String,
    /// Process exit status text.
    pub status: String,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

impl fmt::Display for CommandFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "command `{}` failed in `{}` with status {}: {}",
            self.command, self.working_dir, self.status, self.stderr
        )
    }
}

/// Full diagnostic for native C++ compilation through the build helper.
#[derive(Debug, PartialEq, Eq)]
pub struct NativeCompileFailure {
    /// Native library being produced.
    pub library: String,
    /// Compiler path, when explicitly configured or discovered.
    pub compiler: Option<String>,
    /// Working directory of the native compiler invocation.
    pub working_dir: String,
    /// Source files passed to the compiler.
    pub sources: Vec<String>,
    /// Include directories passed to the compiler.
    pub include_dirs: Vec<String>,
    /// Library search directories passed to the compiler/linker.
    pub library_dirs: Vec<String>,
    /// Native compiler and linker flags.
    pub flags: Vec<String>,
    /// Human-readable failure message.
    pub message: String,
}

impl fmt::Display for NativeCompileFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "native C++ compilation for `{}` failed in `{}`: {}",
            self.library, self.working_dir, self.message
        )
    }
}

/// Typed diagnostics for build planning and metadata emission.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildError {
    /// At least one program must be configured before compiling.
    #[error("no Souffle programs were configured")]
    NoPrograms,
    /// Program names are used in generated artifact paths and Rust modules.
    #[error("invalid program name `{program}`")]
    InvalidProgramName {
        /// Invalid program name.
        program: String,
    },
    /// Program names must map to distinct generated artifacts.
    #[error("duplicate program name `{program}`")]
    DuplicateProgramName {
        /// Duplicate program name.
        program: String,
    },
    /// A required path setting was empty.
    #[error("empty path configured for `{field}`")]
    EmptyPath {
        /// Configuration field whose path was empty.
        field: &'static str,
    },
    /// A required string setting was empty.
    #[error("empty value configured for `{field}`")]
    EmptyValue {
        /// Configuration field whose value was empty.
        field: &'static str,
    },
    /// A required Cargo build-script environment variable was not set.
    #[error("missing Cargo environment variable `{variable}`")]
    MissingCargoEnv {
        /// Cargo environment variable name.
        variable: &'static str,
    },
    /// A configured identifier-like value was malformed.
    #[error("invalid identifier `{value}` configured for `{field}`")]
    InvalidIdentifierValue {
        /// Configuration field containing the malformed identifier.
        field: &'static str,
        /// Malformed identifier value.
        value: String,
    },
    /// The requested linker/platform capability is unavailable for the target.
    #[error("platform capability `{capability}` is unavailable for target `{target}`")]
    UnsupportedPlatformCapability {
        /// Requested platform capability.
        capability: String,
        /// Target triple that cannot support the capability.
        target: String,
    },
    /// JSON metadata serialization failed.
    #[error("failed to serialize build metadata: {message}")]
    MetadataSerialization {
        /// Serializer error message.
        message: String,
    },
    /// Build artifact serialization failed.
    #[error("failed to serialize artifact `{artifact}`: {message}")]
    ArtifactSerialization {
        /// Artifact path or logical artifact name.
        artifact: String,
        /// Serializer error message.
        message: String,
    },
    /// A requested schema or typed API artifact had no reliable schema source.
    #[error("schema bundle for program `{program}` is unavailable")]
    SchemaUnavailable {
        /// Program whose schema was requested.
        program: String,
    },
    /// A configured or extracted schema was internally inconsistent.
    #[error("schema bundle for program `{program}` is invalid: {message}")]
    SchemaValidation {
        /// Program whose schema failed validation.
        program: String,
        /// Validation failure message.
        message: String,
    },
    /// Build-time schema extraction from Souffle metadata failed.
    #[error("failed to extract schema for program `{program}`: {message}")]
    SchemaExtraction {
        /// Program whose schema extraction failed.
        program: String,
        /// Extraction failure message.
        message: String,
    },
    /// Native C++ compilation was enabled but no generated or wrapper sources were available.
    #[error("native C++ compilation for `{library}` has no source files")]
    NativeSourcesUnavailable {
        /// Native library that had no available sources.
        library: String,
    },
    /// Filesystem operation failed while preparing or writing build artifacts.
    #[error("failed to {operation} `{path}`: {message}")]
    Io {
        /// Filesystem operation being attempted.
        operation: String,
        /// Path involved in the failure.
        path: String,
        /// Underlying I/O error message.
        message: String,
    },
    /// Failed to spawn a configured external command.
    #[error("failed to spawn command `{command}` in `{working_dir}`: {message}")]
    CommandSpawnFailed {
        /// Command line that could not be spawned.
        command: String,
        /// Working directory requested for the command.
        working_dir: String,
        /// Spawn failure message.
        message: String,
    },
    /// A configured external command exited unsuccessfully.
    #[error("{0}")]
    CommandFailed(Box<CommandFailure>),
    /// Native C++ compilation exited unsuccessfully.
    #[error("{0}")]
    NativeCompileFailed(Box<NativeCompileFailure>),
}
