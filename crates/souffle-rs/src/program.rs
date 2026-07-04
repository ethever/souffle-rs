use std::{collections::BTreeMap, fmt, num::NonZeroUsize};

use serde::Serialize;

use crate::{
    Backend, BuildInfo, CpuBudget, EmbeddedProgram, FileProgram, FileRelationStore, ProcessConfig,
    ProcessProgram, ProgramConfig, RelationBundle, RelationHandle, RelationSchema, Row, RunOptions,
    SouffleError, SqliteProgram, SqliteRelationStore, embedded::EmbeddedRelationIterator,
    schema::TypeCheck,
};

/// Common dynamic relation operations exposed by every backend.
///
/// A dynamic program facade follows the same lifecycle for all backends:
/// resolve schema, insert rows into loadable relations, run the program, and
/// stream or materialize printable relations. Generated typed APIs build on
/// this trait, but the dynamic API is useful for schema-driven tools, tests,
/// parity checks, and integrations that discover relations at runtime.
///
/// **Use streaming APIs for large relations:** prefer [`Program::iter_relation`],
/// [`Program::iter_relation_by_handle`], [`RelationIterator::next_row`], and
/// [`RelationIterator::next_chunk`] when a relation may be large. The
/// `read_relation*` helpers intentionally materialize complete relations into
/// Rust-owned vectors.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
///     RelationSchema, Row, TypeRef, Value,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema: RelationBundle = [
///     RelationSchema::input(
///         RelationId::new(0),
///         "Input",
///         [AttributeSchema::new("id", TypeRef::Number)],
///     ),
///     RelationSchema::output(
///         RelationId::new(1),
///         "Output",
///         [AttributeSchema::new("id", TypeRef::Number)],
///     ),
/// ]
/// .into_iter()
/// .collect();
/// let mut program = InMemoryProgram::builder("analysis")
///     .schema(schema)
///     .build_memory();
///
/// program.insert_row("Input", [Value::Number(7)])?;
/// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
/// program.run()?;
///
/// let mut output = program.iter_relation("Output")?;
/// assert_eq!(output.next_row()?.unwrap().values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
pub trait Program {
    /// Generated program name.
    fn name(&self) -> &str;

    /// Selected runtime backend.
    fn backend(&self) -> Backend;

    /// Complete schema bundle for this program.
    fn schema_bundle(&self) -> &RelationBundle;

    /// C ABI version used by this program facade.
    fn abi_version(&self) -> Result<u32, SouffleError>;

    /// Runtime-visible build and ABI metadata.
    fn build_info(&self) -> Result<BuildInfo, SouffleError> {
        Ok(BuildInfo::new(
            self.name(),
            self.backend(),
            self.abi_version()?,
            self.schema_bundle().clone(),
        ))
    }

    /// Schema for one relation.
    fn relation_schema(&self, relation: &str) -> Result<&RelationSchema, SouffleError> {
        self.schema_bundle()
            .get(relation)
            .ok_or_else(|| SouffleError::RelationNotFound {
                relation: relation.to_owned(),
            })
    }

    /// Stable handle for one relation.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, TypeRef,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::input(
    ///     RelationId::new(0),
    ///     "Input",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    ///
    /// let input = program.relation_handle("Input")?;
    /// assert_eq!(input.name(), "Input");
    /// assert!(input.is_loadable());
    /// # Ok(())
    /// # }
    /// ```
    fn relation_handle(&self, relation: &str) -> Result<RelationHandle, SouffleError> {
        Ok(self.relation_schema(relation)?.handle())
    }

    /// Schema for one relation addressed by a previously resolved handle.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, TypeRef,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::output(
    ///     RelationId::new(0),
    ///     "Output",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    /// let output = program.relation_handle("Output")?;
    ///
    /// assert_eq!(program.relation_schema_by_handle(&output)?.arity(), 1);
    /// # Ok(())
    /// # }
    /// ```
    fn relation_schema_by_handle(
        &self,
        handle: &RelationHandle,
    ) -> Result<&RelationSchema, SouffleError> {
        let schema = self.relation_schema(handle.name())?;
        if schema.id() != handle.id() {
            return Err(SouffleError::RelationHandleMismatch {
                relation: handle.name().to_owned(),
                expected: handle.id(),
                actual: schema.id(),
            });
        }
        Ok(schema)
    }

    /// Insert one row into a loadable relation.
    ///
    /// **Performance note:** this is a convenience API, not a bulk ingestion
    /// API. File and SQLite backends read and rewrite relation storage for each
    /// inserted row, embedded backends cross the FFI boundary once per row, and
    /// process backends buffer rows before writing `.facts` files at run time.
    /// Prefer backend-specific bulk/export paths when loading large inputs.
    fn insert_row(&mut self, relation: &str, row: impl Into<Row>) -> Result<(), SouffleError>;

    /// Insert one row into a loadable relation addressed by handle.
    ///
    /// **Performance note:** this calls [`Program::insert_row`], so the same
    /// per-row backend costs apply. Prefer bulk ingestion or export-oriented
    /// APIs when loading large relations.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, TypeRef, Value,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::input(
    ///     RelationId::new(0),
    ///     "Input",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let mut program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    /// let input = program.relation_handle("Input")?;
    ///
    /// program.insert_row_by_handle(&input, [Value::Number(7)])?;
    /// # Ok(())
    /// # }
    /// ```
    fn insert_row_by_handle(
        &mut self,
        handle: &RelationHandle,
        row: impl Into<Row>,
    ) -> Result<(), SouffleError> {
        self.relation_schema_by_handle(handle)?;
        self.insert_row(handle.name(), row)
    }

    /// Run the generated program with explicit options.
    fn run_with_options(&mut self, options: RunOptions) -> Result<(), SouffleError>;

    /// Default run options derived from the program configuration.
    fn default_run_options(&self) -> RunOptions {
        RunOptions::default()
    }

    /// Run the generated program with its configured default thread count.
    fn run(&mut self) -> Result<(), SouffleError> {
        self.run_with_options(self.default_run_options())
    }

    /// Iterate one printable relation.
    ///
    /// **Recommended for large relations:** this returns a streaming
    /// [`RelationIterator`] instead of materializing the whole relation.
    fn iter_relation<'program>(
        &'program self,
        relation: &str,
    ) -> Result<RelationIterator<'program>, SouffleError>;

    /// Iterate one printable relation addressed by handle.
    ///
    /// **Recommended for large relations:** this returns a streaming
    /// [`RelationIterator`] instead of materializing the whole relation.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, Row, TypeRef, Value,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::output(
    ///     RelationId::new(0),
    ///     "Output",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let mut program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    /// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
    /// let output = program.relation_handle("Output")?;
    ///
    /// let mut rows = program.iter_relation_by_handle(&output)?;
    /// assert_eq!(rows.next_row()?.unwrap().values(), &[Value::Number(7)]);
    /// # Ok(())
    /// # }
    /// ```
    fn iter_relation_by_handle<'program>(
        &'program self,
        handle: &RelationHandle,
    ) -> Result<RelationIterator<'program>, SouffleError> {
        self.relation_schema_by_handle(handle)?;
        self.iter_relation(handle.name())
    }

    /// Materialize one printable relation using the backend's streaming path.
    ///
    /// **Performance note:** this collects the entire relation into a
    /// [`RelationOutput`] backed by `Vec<Row>`. Use [`Program::iter_relation`],
    /// [`RelationIterator::next_row`], or [`RelationIterator::next_chunk`] for
    /// large outputs.
    fn read_relation(&self, relation: &str) -> Result<RelationOutput, SouffleError> {
        let mut iterator = self.iter_relation(relation)?;
        let schema = iterator.schema().clone();
        let mut rows = Vec::new();
        while let Some(row) = iterator.next_row()? {
            rows.push(row);
        }
        RelationOutput::new(schema, rows)
    }

    /// Materialize one printable relation addressed by handle.
    ///
    /// **Performance note:** this collects the entire relation into a
    /// [`RelationOutput`] backed by `Vec<Row>`. Use
    /// [`Program::iter_relation_by_handle`], [`RelationIterator::next_row`], or
    /// [`RelationIterator::next_chunk`] for large outputs.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, Row, TypeRef, Value,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::output(
    ///     RelationId::new(0),
    ///     "Output",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let mut program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    /// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
    /// let output = program.relation_handle("Output")?;
    ///
    /// let rows = program.read_relation_by_handle(&output)?;
    /// assert_eq!(rows.rows()[0].values(), &[Value::Number(7)]);
    /// # Ok(())
    /// # }
    /// ```
    fn read_relation_by_handle(
        &self,
        handle: &RelationHandle,
    ) -> Result<RelationOutput, SouffleError> {
        self.relation_schema_by_handle(handle)?;
        self.read_relation(handle.name())
    }
}

/// Builder for the first safe Rust dynamic program facade.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, Backend, Program, ProgramBuilder, RelationBundle,
///     RelationId, RelationSchema, TypeRef,
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
/// let program = ProgramBuilder::new("analysis")
///     .backend(Backend::Memory)
///     .schema(schema)
///     .build_memory();
///
/// assert_eq!(program.name(), "analysis");
/// assert_eq!(program.backend(), Backend::Memory);
/// assert!(program.relation_schema("Output").is_ok());
/// ```
#[derive(Debug, Clone)]
pub struct ProgramBuilder {
    config: ProgramConfig,
    schema: RelationBundle,
    process_config: Option<ProcessConfig>,
    file_store: Option<FileRelationStore>,
    sqlite_store: Option<SqliteRelationStore>,
}

impl ProgramBuilder {
    /// Start building a named program facade.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            config: ProgramConfig::new(name),
            schema: RelationBundle::new(),
            process_config: None,
            file_store: None,
            sqlite_store: None,
        }
    }

    /// Select the backend.
    pub fn backend(mut self, backend: Backend) -> Self {
        self.config = self.config.with_backend(backend);
        self
    }

    /// Set the default Souffle thread count used by [`Program::run`].
    pub fn threads(mut self, threads: NonZeroUsize) -> Self {
        let cpu_budget = self
            .config
            .cpu_budget()
            .clone()
            .with_souffle_threads(threads);
        self.config = self.config.with_cpu_budget(cpu_budget);
        self
    }

    /// Set the default Rust worker count used for CPU budget diagnostics.
    pub fn rust_workers(mut self, rust_workers: NonZeroUsize) -> Self {
        let cpu_budget = self
            .config
            .cpu_budget()
            .clone()
            .with_rust_workers(rust_workers);
        self.config = self.config.with_cpu_budget(cpu_budget);
        self
    }

    /// Set the full CPU budget for Rust orchestration and Souffle execution.
    ///
    /// # Example
    ///
    /// ```
    /// use std::num::NonZeroUsize;
    ///
    /// use souffle_rs::{
    ///     AttributeSchema, CpuBudget, InMemoryProgram, Program, RelationBundle,
    ///     RelationId, RelationSchema, TypeRef,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::input(
    ///     RelationId::new(0),
    ///     "Input",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    /// let mut program = InMemoryProgram::builder("analysis")
    ///     .cpu_budget(CpuBudget::new(
    ///         NonZeroUsize::new(2).unwrap(),
    ///         NonZeroUsize::new(4).unwrap(),
    ///     ))
    ///     .schema(schema)
    ///     .build_memory();
    ///
    /// program.run()?;
    /// assert_eq!(program.last_run_options().unwrap().threads().get(), 4);
    /// # Ok(())
    /// # }
    /// ```
    pub fn cpu_budget(mut self, cpu_budget: CpuBudget) -> Self {
        self.config = self.config.with_cpu_budget(cpu_budget);
        self
    }

    /// Attach generated or extracted schema metadata.
    pub fn schema(mut self, schema: RelationBundle) -> Self {
        self.schema = schema;
        self
    }

    /// Attach isolated process backend configuration.
    pub fn process_config(mut self, process_config: ProcessConfig) -> Self {
        self.process_config = Some(process_config);
        self
    }

    /// Attach file backend storage.
    pub fn file_store(mut self, store: FileRelationStore) -> Self {
        self.file_store = Some(store);
        self
    }

    /// Attach SQLite backend storage.
    pub fn sqlite_store(mut self, store: SqliteRelationStore) -> Self {
        self.sqlite_store = Some(store);
        self
    }

    /// Build an in-memory relation facade.
    pub fn build_memory(self) -> InMemoryProgram {
        self.try_build_memory()
            .expect("invalid schema passed to ProgramBuilder::build_memory")
    }

    /// Build an in-memory relation facade after schema validation.
    pub fn try_build_memory(self) -> Result<InMemoryProgram, SouffleError> {
        self.schema.validate()?;
        Ok(InMemoryProgram::new(
            self.config.with_backend(Backend::Memory),
            self.schema,
        ))
    }

    /// Build an embedded generated-program facade.
    pub fn build_embedded(self) -> Result<EmbeddedProgram, SouffleError> {
        EmbeddedProgram::from_config(self.config.with_backend(Backend::Embedded), self.schema)
    }

    /// Build an isolated process facade.
    pub fn build_process(self) -> Result<ProcessProgram, SouffleError> {
        let Some(process_config) = self.process_config else {
            return Err(SouffleError::ProcessConfiguration {
                field: "process_config".to_owned(),
                message: "missing ProcessConfig".to_owned(),
            });
        };
        ProcessProgram::from_config(
            self.config.with_backend(Backend::Process),
            self.schema,
            process_config,
        )
    }

    /// Build a file-backed relation facade.
    pub fn build_file(self) -> Result<FileProgram, SouffleError> {
        let Some(store) = self.file_store else {
            return Err(SouffleError::BackendConfiguration {
                backend: Backend::File,
                field: "file_store".to_owned(),
                message: "missing FileRelationStore".to_owned(),
            });
        };
        FileProgram::from_config(self.config.with_backend(Backend::File), self.schema, store)
    }

    /// Build a SQLite-backed relation facade.
    pub fn build_sqlite(self) -> Result<SqliteProgram, SouffleError> {
        let Some(store) = self.sqlite_store else {
            return Err(SouffleError::BackendConfiguration {
                backend: Backend::Sqlite,
                field: "sqlite_store".to_owned(),
                message: "missing SqliteRelationStore".to_owned(),
            });
        };
        SqliteProgram::from_config(
            self.config.with_backend(Backend::Sqlite),
            self.schema,
            store,
        )
    }
}

/// Rust-owned in-memory backend used as the shared relation contract and parity
/// fixture for generated backends.
///
/// The in-memory backend does not evaluate Datalog rules. It stores validated
/// input and output relation rows, records the latest run options, and provides
/// the same streaming/read APIs as generated backends. This makes it useful for
/// unit tests, schema validation, file/SQLite export tooling, and backend
/// parity checks.
#[derive(Debug, Clone)]
pub struct InMemoryProgram {
    config: ProgramConfig,
    schema: RelationBundle,
    relation_rows: BTreeMap<String, Vec<Row>>,
    last_run_options: Option<RunOptions>,
}

impl InMemoryProgram {
    /// Create an in-memory program from explicit config and schema metadata.
    pub fn new(config: ProgramConfig, schema: RelationBundle) -> Self {
        Self::try_new(config, schema).expect("invalid schema passed to InMemoryProgram::new")
    }

    /// Create an in-memory program from explicit config after schema validation.
    pub fn try_new(config: ProgramConfig, schema: RelationBundle) -> Result<Self, SouffleError> {
        schema.validate()?;
        let relation_rows = schema
            .iter()
            .map(|relation| (relation.name().to_owned(), Vec::new()))
            .collect();
        Ok(Self {
            config: config.with_backend(Backend::Memory),
            schema,
            relation_rows,
            last_run_options: None,
        })
    }

    /// Start building a named in-memory program.
    pub fn builder(name: impl Into<String>) -> ProgramBuilder {
        ProgramBuilder::new(name).backend(Backend::Memory)
    }

    /// Default options used by the latest `run` call, if any.
    pub fn last_run_options(&self) -> Option<&RunOptions> {
        self.last_run_options.as_ref()
    }

    /// Replace relation rows after validating each row against schema.
    ///
    /// Backend adapters use this to materialize output rows without bypassing
    /// schema checks.
    pub fn replace_relation_rows(
        &mut self,
        relation: &str,
        rows: impl IntoIterator<Item = Row>,
    ) -> Result<(), SouffleError> {
        let schema = self.relation_schema(relation)?.clone();
        let rows = rows.into_iter().collect::<Vec<_>>();
        for row in &rows {
            validate_row(&schema, row)?;
        }
        self.relation_rows.insert(relation.to_owned(), rows);
        Ok(())
    }
}

impl Program for InMemoryProgram {
    fn name(&self) -> &str {
        self.config.name()
    }

    fn backend(&self) -> Backend {
        self.config.backend()
    }

    fn schema_bundle(&self) -> &RelationBundle {
        &self.schema
    }

    fn abi_version(&self) -> Result<u32, SouffleError> {
        Ok(souffle_rs_sys::SOUFFLE_RS_ABI_VERSION)
    }

    fn insert_row(&mut self, relation: &str, row: impl Into<Row>) -> Result<(), SouffleError> {
        let schema = self.relation_schema(relation)?;
        if !schema.is_loadable() {
            return Err(SouffleError::RelationNotInput {
                relation: relation.to_owned(),
            });
        }

        let row = row.into();
        validate_row(schema, &row)?;
        self.relation_rows
            .entry(relation.to_owned())
            .or_default()
            .push(row);
        Ok(())
    }

    fn run_with_options(&mut self, options: RunOptions) -> Result<(), SouffleError> {
        self.last_run_options = Some(options);
        Ok(())
    }

    fn default_run_options(&self) -> RunOptions {
        RunOptions::from_cpu_budget(self.config.cpu_budget())
    }

    fn iter_relation<'program>(
        &'program self,
        relation: &str,
    ) -> Result<RelationIterator<'program>, SouffleError> {
        let schema = self.relation_schema(relation)?;
        if !schema.is_printable() {
            return Err(SouffleError::RelationNotOutput {
                relation: relation.to_owned(),
            });
        }

        let rows = self
            .relation_rows
            .get(relation)
            .cloned()
            .unwrap_or_default()
            .into_iter();
        Ok(RelationIterator::new(schema.clone(), rows.collect()))
    }
}

/// Materialized output relation.
///
/// `RelationOutput` owns a relation schema and all decoded rows. Use
/// [`Program::read_relation`] when the whole relation should be available at
/// once; use [`Program::iter_relation`] for streaming.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, RelationId, RelationOutput, RelationSchema, Row, TypeRef, Value,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema = RelationSchema::output(
///     RelationId::new(0),
///     "Output",
///     [AttributeSchema::new("id", TypeRef::Number)],
/// );
/// let output = RelationOutput::new(schema, vec![Row::new([Value::Number(7)])])?;
///
/// assert_eq!(output.schema().name(), "Output");
/// assert_eq!(output.rows()[0].values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RelationOutput {
    schema: RelationSchema,
    rows: Vec<Row>,
}

impl RelationOutput {
    /// Create output rows after schema validation.
    pub fn new(schema: RelationSchema, rows: Vec<Row>) -> Result<Self, SouffleError> {
        for row in &rows {
            validate_row(&schema, row)?;
        }
        Ok(Self { schema, rows })
    }

    /// Relation schema.
    pub fn schema(&self) -> &RelationSchema {
        &self.schema
    }

    /// Materialized rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Consume into rows.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }
}

/// Chunk-friendly relation iterator.
///
/// In-memory backends stream from Rust-owned row buffers, process backends
/// stream from Souffle output files, file and SQLite backends stream from their
/// durable row stores, and embedded backends pull rows through a wrapper-owned
/// C++ iterator handle. The iterator borrows the owning program, so Rust
/// prevents running, mutating, or dropping the program while rows are still
/// being streamed.
///
/// **Recommended for large relations:** use this iterator instead of
/// [`Program::read_relation`] or generated typed `read()` helpers. Chunked
/// iteration bounds peak Rust memory to the requested chunk size plus backend
/// buffering.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
///     RelationSchema, Row, TypeRef, Value,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema: RelationBundle = [RelationSchema::output(
///     RelationId::new(0),
///     "Output",
///     [AttributeSchema::new("id", TypeRef::Number)],
/// )]
/// .into_iter()
/// .collect();
///
/// let mut program = InMemoryProgram::builder("analysis")
///     .schema(schema)
///     .build_memory();
/// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
///
/// let mut rows = program.iter_relation("Output")?;
/// assert_eq!(rows.schema().name(), "Output");
/// assert_eq!(rows.next_row()?.unwrap().values(), &[Value::Number(7)]);
/// assert!(rows.next_row()?.is_none());
/// # Ok(())
/// # }
/// ```
///
/// The iterator borrow prevents mutating or rerunning the program while rows are
/// still being streamed:
///
/// ```compile_fail
/// use souffle_rs::{
///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
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
/// let mut program = InMemoryProgram::builder("analysis")
///     .schema(schema)
///     .build_memory();
///
/// let rows = program.iter_relation("Output").unwrap();
/// program.run().unwrap();
/// drop(rows);
/// ```
#[derive(Debug)]
pub struct RelationIterator<'program> {
    schema: RelationSchema,
    source: Box<dyn RelationIteratorSource + 'program>,
}

impl<'program> RelationIterator<'program> {
    pub(crate) fn new(schema: RelationSchema, rows: Vec<Row>) -> Self {
        Self {
            schema,
            source: Box::new(BufferedRelationRows { rows, offset: 0 }),
        }
    }

    pub(crate) fn from_embedded(
        schema: RelationSchema,
        iterator: EmbeddedRelationIterator<'program>,
    ) -> Self {
        Self {
            schema,
            source: Box::new(iterator),
        }
    }

    pub(crate) fn from_source<S>(schema: RelationSchema, source: S) -> Self
    where
        S: RelationIteratorSource + 'program,
    {
        Self {
            schema,
            source: Box::new(source),
        }
    }

    /// Schema attached to the rows being streamed.
    pub fn schema(&self) -> &RelationSchema {
        &self.schema
    }

    /// Return the next decoded row.
    ///
    /// **Recommended for streaming:** this avoids collecting the whole relation
    /// into a `Vec<Row>`.
    pub fn next_row(&mut self) -> Result<Option<Row>, SouffleError> {
        self.source.next_row(&self.schema)
    }

    /// Return up to `max_rows` decoded rows from this iterator.
    ///
    /// A zero-sized chunk request returns an empty vector without querying the
    /// backend. Reaching the end of the relation also returns an empty vector.
    ///
    /// **Performance note:** each returned chunk is still materialized as a
    /// `Vec<Row>`. For embedded backends, the C++ wrapper also materializes up
    /// to `max_rows` rows before Rust decodes them. Choose a chunk size that
    /// balances memory use against backend/FFI call overhead.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
    ///     RelationSchema, Row, TypeRef, Value,
    /// };
    ///
    /// # fn main() -> Result<(), souffle_rs::SouffleError> {
    /// let schema: RelationBundle = [RelationSchema::output(
    ///     RelationId::new(0),
    ///     "Output",
    ///     [AttributeSchema::new("id", TypeRef::Number)],
    /// )]
    /// .into_iter()
    /// .collect();
    ///
    /// let mut program = InMemoryProgram::builder("analysis")
    ///     .schema(schema)
    ///     .build_memory();
    /// program.replace_relation_rows(
    ///     "Output",
    ///     [
    ///         Row::new([Value::Number(7)]),
    ///         Row::new([Value::Number(8)]),
    ///     ],
    /// )?;
    ///
    /// let mut rows = program.iter_relation("Output")?;
    /// assert_eq!(rows.next_chunk(1)?.len(), 1);
    /// assert_eq!(rows.next_chunk(8)?.len(), 1);
    /// assert!(rows.next_chunk(8)?.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn next_chunk(&mut self, max_rows: usize) -> Result<Vec<Row>, SouffleError> {
        if max_rows == 0 {
            return Ok(Vec::new());
        }
        self.source.next_chunk(&self.schema, max_rows)
    }
}

pub(crate) trait RelationIteratorSource: fmt::Debug {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError>;

    fn next_chunk(
        &mut self,
        schema: &RelationSchema,
        max_rows: usize,
    ) -> Result<Vec<Row>, SouffleError> {
        let mut rows = Vec::new();
        for _ in 0..max_rows {
            let Some(row) = self.next_row(schema)? else {
                break;
            };
            rows.push(row);
        }
        Ok(rows)
    }
}

#[derive(Debug)]
struct BufferedRelationRows {
    rows: Vec<Row>,
    offset: usize,
}

impl RelationIteratorSource for BufferedRelationRows {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        let Some(row) = self.rows.get(self.offset).cloned() else {
            return Ok(None);
        };
        validate_row(schema, &row)?;
        self.offset += 1;
        Ok(Some(row))
    }

    fn next_chunk(
        &mut self,
        schema: &RelationSchema,
        max_rows: usize,
    ) -> Result<Vec<Row>, SouffleError> {
        if max_rows == 0 {
            return Ok(Vec::new());
        }

        let end = self.offset.saturating_add(max_rows).min(self.rows.len());
        let rows = self.rows[self.offset..end].to_vec();
        for row in &rows {
            validate_row(schema, row)?;
        }
        self.offset = end;
        Ok(rows)
    }
}

pub(crate) fn validate_row(schema: &RelationSchema, row: &Row) -> Result<(), SouffleError> {
    schema.validate()?;

    if row.len() != schema.arity() {
        return Err(SouffleError::ArityMismatch {
            relation: schema.name().to_owned(),
            expected: schema.arity(),
            actual: row.len(),
        });
    }

    for (attribute, value) in schema.attributes().iter().zip(row.values()) {
        match attribute.declared_type().accepts_value(value) {
            TypeCheck::Ok => {}
            TypeCheck::Mismatch { expected, actual } => {
                return Err(SouffleError::TypeMismatch {
                    relation: schema.name().to_owned(),
                    column: attribute.name().to_owned(),
                    expected,
                    actual,
                });
            }
            TypeCheck::AdtVariantMismatch { variant } => {
                return Err(SouffleError::AdtVariantMismatch {
                    relation: schema.name().to_owned(),
                    column: attribute.name().to_owned(),
                    variant,
                });
            }
        }
    }

    Ok(())
}
