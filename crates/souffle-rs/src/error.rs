use thiserror::Error;

/// Top-level error type for safe `souffle-rs` APIs.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SouffleError {
    /// Build-time Souffle or C++ generation failed.
    #[error(transparent)]
    Build(#[from] BuildError),
    /// Native or Rust artifact linking failed.
    #[error(transparent)]
    Link(#[from] LinkError),
    /// C ABI contract or version check failed.
    #[error(transparent)]
    Abi(#[from] AbiError),
    /// Requested generated program was not registered.
    #[error("program `{program}` was not found")]
    ProgramNotFound {
        /// Requested program name.
        program: String,
    },
    /// Requested relation does not exist in the schema bundle.
    #[error("relation `{relation}` was not found")]
    RelationNotFound {
        /// Requested relation name.
        relation: String,
    },
    /// A relation handle no longer matches the schema bundle relation id.
    #[error("relation handle `{relation}` expected id {expected} but schema has id {actual}")]
    RelationHandleMismatch {
        /// Relation name carried by the stale or mismatched handle.
        relation: String,
        /// Relation id carried by the handle.
        expected: crate::RelationId,
        /// Relation id found in the current schema bundle.
        actual: crate::RelationId,
    },
    /// No reliable schema is available for the relation.
    #[error("schema for relation `{relation}` is unavailable")]
    SchemaUnavailable {
        /// Relation whose schema was requested.
        relation: String,
    },
    /// Row arity does not match relation schema.
    #[error("relation `{relation}` expects {expected} values but received {actual}")]
    ArityMismatch {
        /// Relation receiving or returning the row.
        relation: String,
        /// Number of columns declared by the schema.
        expected: usize,
        /// Number of values supplied or decoded.
        actual: usize,
    },
    /// A value does not match its declared Souffle type.
    #[error("relation `{relation}` column `{column}` expects `{expected}` but received `{actual}`")]
    TypeMismatch {
        /// Relation whose value failed type checking.
        relation: String,
        /// Column whose declared type was not satisfied.
        column: String,
        /// Expected declared or runtime type name.
        expected: String,
        /// Actual runtime value type name.
        actual: String,
    },
    /// A relation schema is internally inconsistent before any row values are used.
    #[error("schema for relation `{relation}` is invalid at `{path}`: {message}")]
    SchemaValidation {
        /// Relation whose schema failed validation.
        relation: String,
        /// Attribute or type path within the relation schema.
        path: String,
        /// Validation failure message.
        message: String,
    },
    /// An ADT value used a variant outside the declared type.
    #[error("relation `{relation}` column `{column}` has unsupported ADT variant `{variant}`")]
    AdtVariantMismatch {
        /// Relation whose ADT value failed validation.
        relation: String,
        /// Column whose ADT variant was invalid.
        column: String,
        /// Variant constructor that is not declared for the type.
        variant: String,
    },
    /// The relation cannot be inserted into for the selected backend.
    #[error("relation `{relation}` cannot be used as input")]
    RelationNotInput {
        /// Relation that was used for insertion.
        relation: String,
    },
    /// The relation cannot be read for the selected backend.
    #[error("relation `{relation}` cannot be read as output")]
    RelationNotOutput {
        /// Relation that was used for output reading.
        relation: String,
    },
    /// Process backend configuration is missing or invalid.
    #[error("process backend configuration `{field}` is invalid: {message}")]
    ProcessConfiguration {
        /// Invalid process backend configuration field.
        field: String,
        /// Validation failure message.
        message: String,
    },
    /// Backend configuration is missing or invalid.
    #[error("backend `{backend:?}` configuration `{field}` is invalid: {message}")]
    BackendConfiguration {
        /// Backend whose configuration failed validation.
        backend: crate::Backend,
        /// Invalid backend configuration field.
        field: String,
        /// Validation failure message.
        message: String,
    },
    /// Configured Rust workers and Souffle threads can exceed host parallelism.
    #[error(
        "CPU budget oversubscribes host parallelism: {rust_workers} Rust workers * {souffle_threads} Souffle threads = {requested_threads} requested threads, but only {available_threads} are available"
    )]
    ThreadOversubscription {
        /// Number of Rust workers requested by the caller.
        rust_workers: usize,
        /// Number of Souffle/OpenMP threads per worker.
        souffle_threads: usize,
        /// Product of Rust workers and Souffle threads.
        requested_threads: usize,
        /// Host parallelism used for validation.
        available_threads: usize,
    },
    /// A backend cannot represent a schema-visible relation type.
    #[error(
        "backend `{backend:?}` does not support relation `{relation}` column `{column}` type `{declared_type}`: {message}"
    )]
    UnsupportedType {
        /// Backend that cannot represent the type.
        backend: crate::Backend,
        /// Relation containing the unsupported type.
        relation: String,
        /// Column containing the unsupported type.
        column: String,
        /// Declared Souffle type name.
        declared_type: String,
        /// Backend-specific explanation.
        message: String,
    },
    /// Generated program execution failed.
    #[error("program `{program}` failed: {message}")]
    RunFailed {
        /// Program whose execution failed.
        program: String,
        /// Backend or generated-program failure message.
        message: String,
    },
    /// A C++ exception was caught and converted at the wrapper boundary.
    #[error("C++ exception: {message}")]
    CxxException {
        /// Message captured from `std::exception::what()` or the wrapper fallback.
        message: String,
    },
    /// Safe Rust decoding of a relation value failed.
    #[error("failed to decode relation `{relation}` column `{column}`: {message}")]
    DecodeFailed {
        /// Relation being decoded.
        relation: String,
        /// Column being decoded.
        column: String,
        /// Decode failure message.
        message: String,
    },
    /// Backend outputs differed during parity comparison.
    #[error("backend parity mismatch for relation `{relation}`: {message}")]
    BackendParityMismatch {
        /// Relation whose normalized rows differed.
        relation: String,
        /// Comparison failure message.
        message: String,
    },
    /// Filesystem operation failed for an explicit file backend/export path.
    #[error("failed to {operation} `{path}`: {message}")]
    FileIo {
        /// Filesystem operation being attempted.
        operation: String,
        /// Path involved in the failure.
        path: String,
        /// Underlying I/O error message.
        message: String,
    },
    /// SQLite backend operation failed.
    #[error("SQLite operation `{operation}` failed for `{database}`: {message}")]
    Sqlite {
        /// SQLite operation being attempted.
        operation: String,
        /// Database path involved in the failure.
        database: String,
        /// Underlying SQLite error message.
        message: String,
    },
    /// Writing a machine-readable artifact failed.
    #[error("failed to encode artifact `{artifact}`: {message}")]
    EncodeFailed {
        /// Artifact path or logical artifact name.
        artifact: String,
        /// Serialization failure message.
        message: String,
    },
    /// Reading a machine-readable artifact failed.
    #[error("failed to decode artifact `{artifact}`: {message}")]
    ArtifactDecodeFailed {
        /// Artifact path or logical artifact name.
        artifact: String,
        /// Deserialization failure message.
        message: String,
    },
}

/// Build-time diagnostic with enough context to reproduce the failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BuildError {
    /// A required tool could not be located.
    #[error("missing build tool `{tool}`")]
    MissingTool {
        /// Required tool name or path.
        tool: String,
    },
    /// A configured input file or directory is missing.
    #[error("missing build input `{path}`")]
    MissingInput {
        /// Missing input path.
        path: String,
    },
    /// A command failed while generating or compiling artifacts.
    #[error("command `{command}` failed in `{working_dir}`: {stderr}")]
    CommandFailed {
        /// Command line that failed.
        command: String,
        /// Working directory of the failed command.
        working_dir: String,
        /// Captured stderr or synthesized failure message.
        stderr: String,
    },
    /// Current platform cannot support the selected build option.
    #[error("platform capability `{capability}` is unavailable")]
    UnsupportedPlatformCapability {
        /// Requested platform capability.
        capability: String,
    },
}

/// Link-time diagnostic for native artifacts and external dependencies.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinkError {
    /// A required library is missing from the configured search paths.
    #[error("missing library `{library}`")]
    MissingLibrary {
        /// Library name that was not found.
        library: String,
    },
    /// A required symbol was not found in a linked artifact.
    #[error("missing symbol `{symbol}` in `{library}`")]
    MissingSymbol {
        /// Library searched for the symbol.
        library: String,
        /// Required symbol name.
        symbol: String,
    },
    /// A linker flag is invalid for the selected compiler/platform.
    #[error("unsupported linker flag `{flag}`")]
    UnsupportedFlag {
        /// Linker flag rejected by the selected compiler/platform.
        flag: String,
    },
    /// Native linker invocation failed.
    #[error("link command `{command}` failed: {stderr}")]
    CommandFailed {
        /// Link command line that failed.
        command: String,
        /// Captured linker stderr.
        stderr: String,
    },
}

/// C ABI version and boundary diagnostics.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AbiError {
    /// Runtime ABI version does not match the generated Rust bindings.
    #[error("ABI mismatch: expected `{expected}`, actual `{actual}`")]
    VersionMismatch {
        /// ABI version required by Rust bindings.
        expected: u32,
        /// ABI version reported by the loaded wrapper.
        actual: u32,
    },
    /// A null pointer crossed a boundary where a valid handle was required.
    #[error("null pointer for `{argument}`")]
    NullPointer {
        /// Pointer argument that was null.
        argument: String,
    },
    /// C ABI returned an unknown error code.
    #[error("unknown ABI error code `{code}`")]
    UnknownErrorCode {
        /// Raw status code returned by the C ABI.
        code: i32,
    },
    /// A C ABI call returned a typed non-success status.
    #[error("ABI call `{function}` returned `{status}`: {message}")]
    CallFailed {
        /// C ABI function that returned an error.
        function: String,
        /// Symbolic status returned by the wrapper.
        status: String,
        /// Wrapper-provided error message.
        message: String,
    },
    /// A string crossing the ABI was not valid UTF-8.
    #[error("invalid UTF-8 for `{argument}`: {message}")]
    InvalidString {
        /// ABI string argument or result that failed UTF-8 validation.
        argument: String,
        /// UTF-8 validation failure message.
        message: String,
    },
    /// FFI callback reported failure or panic containment.
    #[error("callback failed: {message}")]
    CallbackFailed {
        /// Callback failure or panic-containment message.
        message: String,
    },
}
