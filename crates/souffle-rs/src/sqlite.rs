use std::{
    collections::BTreeSet,
    fmt, fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    Backend, BuildInfo, Program, ProgramBuilder, ProgramConfig, RelationBundle, RelationIterator,
    RelationOutput, RelationSchema, Row, RunOptions, SouffleError,
    program::{RelationIteratorSource, validate_row},
};

const FORMAT_VERSION: u32 = 1;

/// SQLite-backed relation store used for explicit export, debugging,
/// interoperability, and backend parity.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
///     RelationSchema, Row, SqliteRelationStore, TypeRef, Value,
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
///
/// let tempdir = tempfile::tempdir().expect("temporary sqlite export");
/// let store = SqliteRelationStore::new(tempdir.path().join("relations.db"));
/// let artifacts = store.export_outputs(&program, ["Output"])?;
///
/// assert_eq!(artifacts[0].relation, "Output");
/// assert_eq!(store.load_outputs()?[0].rows()[0].values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteRelationStore {
    path: PathBuf,
}

impl SqliteRelationStore {
    /// Create a SQLite relation store at a deterministic database path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// SQLite database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Export selected printable relations from a program into SQLite.
    pub fn export_outputs<P, S>(
        &self,
        program: &P,
        relations: impl IntoIterator<Item = S>,
    ) -> Result<Vec<SqliteRelationArtifact>, SouffleError>
    where
        P: Program,
        S: AsRef<str>,
    {
        let mut connection = self.open()?;
        initialize_schema(&connection, self.path())?;

        let transaction = connection
            .transaction()
            .map_err(|source| sqlite_error("begin transaction", self.path(), source))?;
        write_program_schema(&transaction, self.path(), program.schema_bundle())?;
        clear_exported_relations(&transaction, self.path())?;

        let mut artifacts = Vec::new();
        for relation in relations {
            let schema = program.relation_schema(relation.as_ref())?.clone();
            let mut rows = program.iter_relation(schema.name())?;
            artifacts.push(write_streamed_relation(
                &transaction,
                self.path(),
                &schema,
                &mut rows,
            )?);
        }

        transaction
            .commit()
            .map_err(|source| sqlite_error("commit transaction", self.path(), source))?;
        Ok(artifacts)
    }

    /// Load all relation artifacts from the store.
    pub fn load_artifacts(&self) -> Result<Vec<SqliteRelationArtifact>, SouffleError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT relation, relation_id, schema_json, row_count \
                 FROM relation_manifest ORDER BY relation_id, relation",
            )
            .map_err(|source| sqlite_error("prepare manifest query", self.path(), source))?;
        let rows = statement
            .query_map([], |row| {
                let schema_json: String = row.get(2)?;
                let schema =
                    serde_json::from_str::<RelationSchema>(&schema_json).map_err(|source| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(source),
                        )
                    })?;
                Ok(SqliteRelationArtifact {
                    relation: row.get(0)?,
                    relation_id: decode_u32(row.get(1)?, 1)?,
                    schema,
                    row_count: decode_usize(row.get(3)?, 3)?,
                })
            })
            .map_err(|source| sqlite_error("query manifest", self.path(), source))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|source| sqlite_error("decode manifest", self.path(), source))
    }

    /// Load all relation outputs from the store.
    pub fn load_outputs(&self) -> Result<Vec<RelationOutput>, SouffleError> {
        self.load_artifacts()?
            .iter()
            .map(|artifact| self.load_relation(artifact))
            .collect()
    }

    pub(crate) fn initialize_runtime(&self, schema: &RelationBundle) -> Result<(), SouffleError> {
        let connection = self.open()?;
        initialize_schema(&connection, self.path())?;
        write_program_schema(&connection, self.path(), schema)?;
        reconcile_runtime_schema(&connection, self.path(), schema)
    }

    pub(crate) fn replace_relation_rows(
        &self,
        schema: RelationSchema,
        rows: Vec<Row>,
    ) -> Result<SqliteRelationArtifact, SouffleError> {
        let output = RelationOutput::new(schema, rows)?;
        let mut connection = self.open()?;
        initialize_schema(&connection, self.path())?;
        let transaction = connection
            .transaction()
            .map_err(|source| sqlite_error("begin transaction", self.path(), source))?;
        let artifact = write_relation(&transaction, self.path(), &output)?;
        transaction
            .commit()
            .map_err(|source| sqlite_error("commit transaction", self.path(), source))?;
        Ok(artifact)
    }

    pub(crate) fn load_relation_output(
        &self,
        schema: &RelationSchema,
    ) -> Result<RelationOutput, SouffleError> {
        let artifacts = self.load_artifacts()?;
        let Some(artifact) = artifacts
            .iter()
            .find(|artifact| artifact.relation == schema.name())
        else {
            return RelationOutput::new(schema.clone(), Vec::new());
        };
        self.load_relation(artifact)
    }

    pub(crate) fn stream_relation_output(
        &self,
        schema: &RelationSchema,
    ) -> Result<Option<SqliteRelationRows>, SouffleError> {
        let artifacts = self.load_artifacts()?;
        let Some(artifact) = artifacts
            .iter()
            .find(|artifact| artifact.relation == schema.name())
        else {
            return Ok(None);
        };
        let connection = self.open()?;
        let actual_count = relation_row_count(&connection, self.path(), &artifact.relation)?;
        if actual_count != artifact.row_count {
            return Err(SouffleError::ArtifactDecodeFailed {
                artifact: self.path().display().to_string(),
                message: format!(
                    "relation `{}` expected {} rows from manifest but decoded {}",
                    artifact.relation, artifact.row_count, actual_count
                ),
            });
        }
        Ok(Some(SqliteRelationRows {
            path: self.path.clone(),
            connection,
            relation: artifact.relation.clone(),
            expected_row_count: artifact.row_count,
            next_index: 0,
        }))
    }

    fn load_relation(
        &self,
        artifact: &SqliteRelationArtifact,
    ) -> Result<RelationOutput, SouffleError> {
        let connection = self.open()?;
        let mut statement = connection
            .prepare(
                "SELECT row_json FROM relation_rows \
                 WHERE relation = ?1 ORDER BY row_index",
            )
            .map_err(|source| sqlite_error("prepare relation query", self.path(), source))?;
        let rows = statement
            .query_map(params![artifact.relation], |row| {
                let row_json: String = row.get(0)?;
                serde_json::from_str::<Row>(&row_json).map_err(|source| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(source),
                    )
                })
            })
            .map_err(|source| sqlite_error("query relation rows", self.path(), source))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| sqlite_error("decode relation rows", self.path(), source))?;

        if rows.len() != artifact.row_count {
            return Err(SouffleError::ArtifactDecodeFailed {
                artifact: self.path().display().to_string(),
                message: format!(
                    "relation `{}` expected {} rows from manifest but decoded {}",
                    artifact.relation,
                    artifact.row_count,
                    rows.len()
                ),
            });
        }

        RelationOutput::new(artifact.schema.clone(), rows)
    }

    fn open(&self) -> Result<Connection, SouffleError> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| SouffleError::FileIo {
                operation: "create directory".to_owned(),
                path: parent.display().to_string(),
                message: source.to_string(),
            })?;
        }
        Connection::open(&self.path)
            .map_err(|source| sqlite_error("open database", self.path(), source))
    }
}

/// SQLite-backed dynamic program facade.
///
/// This backend uses the same schema/value contract as embedded and process
/// facades while storing relation rows in an explicit SQLite database. It is
/// useful for parity, large intermediate relation inspection, and workflows
/// that need durable relation artifacts without Souffle fact/output files.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, Program, RelationBundle, RelationId, RelationSchema,
///     Row, SqliteProgram, SqliteRelationStore, TypeRef, Value,
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
/// let tempdir = tempfile::tempdir().expect("temporary sqlite backend");
/// let store = SqliteRelationStore::new(tempdir.path().join("relations.db"));
/// let mut program = SqliteProgram::builder("analysis")
///     .schema(schema)
///     .sqlite_store(store)
///     .build_sqlite()?;
///
/// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
/// let mut rows = program.iter_relation("Output")?;
/// assert_eq!(rows.next_row()?.unwrap().values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct SqliteProgram {
    config: ProgramConfig,
    schema: RelationBundle,
    store: SqliteRelationStore,
    last_run_options: Option<RunOptions>,
}

impl SqliteProgram {
    /// Start building a SQLite-backed program facade.
    pub fn builder(name: impl Into<String>) -> ProgramBuilder {
        ProgramBuilder::new(name).backend(Backend::Sqlite)
    }

    pub(crate) fn from_config(
        config: ProgramConfig,
        schema: RelationBundle,
        store: SqliteRelationStore,
    ) -> Result<Self, SouffleError> {
        schema.validate()?;
        store.initialize_runtime(&schema)?;
        Ok(Self {
            config: config.with_backend(Backend::Sqlite),
            schema,
            store,
            last_run_options: None,
        })
    }

    /// SQLite store backing this program facade.
    pub fn store(&self) -> &SqliteRelationStore {
        &self.store
    }

    /// Options used by the latest `run` call, if any.
    pub fn last_run_options(&self) -> Option<&RunOptions> {
        self.last_run_options.as_ref()
    }

    /// Replace relation rows after validating each row against schema.
    pub fn replace_relation_rows(
        &mut self,
        relation: &str,
        rows: impl IntoIterator<Item = Row>,
    ) -> Result<(), SouffleError> {
        let schema = self.relation_schema(relation)?.clone();
        let rows = rows.into_iter().collect::<Vec<_>>();
        self.store.replace_relation_rows(schema, rows)?;
        Ok(())
    }
}

impl Program for SqliteProgram {
    fn name(&self) -> &str {
        self.config.name()
    }

    fn backend(&self) -> Backend {
        Backend::Sqlite
    }

    fn schema_bundle(&self) -> &RelationBundle {
        &self.schema
    }

    fn abi_version(&self) -> Result<u32, SouffleError> {
        Ok(souffle_rs_sys::SOUFFLE_RS_ABI_VERSION)
    }

    fn build_info(&self) -> Result<BuildInfo, SouffleError> {
        Ok(BuildInfo::new(
            self.name(),
            Backend::Sqlite,
            self.abi_version()?,
            self.schema.clone(),
        ))
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
        let mut rows = self.store.load_relation_output(schema)?.into_rows();
        rows.push(row);
        self.store.replace_relation_rows(schema.clone(), rows)?;
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

        match self.store.stream_relation_output(schema)? {
            Some(source) => Ok(RelationIterator::from_source(schema.clone(), source)),
            None => Ok(RelationIterator::new(schema.clone(), Vec::new())),
        }
    }
}

/// One exported relation recorded in SQLite metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteRelationArtifact {
    /// Relation name.
    pub relation: String,
    /// Stable relation id recorded in the SQLite metadata table.
    pub relation_id: u32,
    /// Schema used to encode rows for this relation.
    pub schema: RelationSchema,
    /// Number of encoded rows in the relation table.
    pub row_count: usize,
}

#[derive(Debug)]
pub(crate) struct SqliteRelationRows {
    path: PathBuf,
    connection: Connection,
    relation: String,
    expected_row_count: usize,
    next_index: usize,
}

impl RelationIteratorSource for SqliteRelationRows {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        if self.next_index >= self.expected_row_count {
            return Ok(None);
        }

        let row_json = self
            .connection
            .query_row(
                "SELECT row_json FROM relation_rows \
                 WHERE relation = ?1 AND row_index = ?2",
                params![&self.relation, self.next_index as i64],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| sqlite_error("read relation row", &self.path, source))?
            .ok_or_else(|| SouffleError::ArtifactDecodeFailed {
                artifact: self.path.display().to_string(),
                message: format!(
                    "relation `{}` expected row index {} from manifest but it was missing",
                    self.relation, self.next_index
                ),
            })?;
        let row = decode_row_json(&self.path, &row_json)?;
        validate_row(schema, &row)?;
        self.next_index += 1;
        Ok(Some(row))
    }
}

fn initialize_schema(connection: &Connection, path: &Path) -> Result<(), SouffleError> {
    connection
        .execute_batch(
            "
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS store_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS program_schema (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                schema_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS relation_manifest (
                relation TEXT PRIMARY KEY,
                relation_id INTEGER NOT NULL,
                schema_json TEXT NOT NULL,
                row_count INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS relation_rows (
                relation TEXT NOT NULL,
                row_index INTEGER NOT NULL,
                row_json TEXT NOT NULL,
                PRIMARY KEY (relation, row_index),
                FOREIGN KEY (relation)
                    REFERENCES relation_manifest(relation)
                    ON DELETE CASCADE
            );
            ",
        )
        .map_err(|source| sqlite_error("initialize schema", path, source))?;
    connection
        .execute(
            "INSERT INTO store_metadata(key, value) VALUES ('format_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![FORMAT_VERSION.to_string()],
        )
        .map_err(|source| sqlite_error("record format version", path, source))?;
    Ok(())
}

fn write_program_schema(
    connection: &Connection,
    path: &Path,
    schema: &RelationBundle,
) -> Result<(), SouffleError> {
    let schema_json =
        serde_json::to_string(schema).map_err(|source| SouffleError::EncodeFailed {
            artifact: path.display().to_string(),
            message: source.to_string(),
        })?;
    connection
        .execute(
            "INSERT INTO program_schema(id, schema_json) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET schema_json = excluded.schema_json",
            params![schema_json],
        )
        .map_err(|source| sqlite_error("write program schema", path, source))?;
    Ok(())
}

fn reconcile_runtime_schema(
    connection: &Connection,
    path: &Path,
    schema: &RelationBundle,
) -> Result<(), SouffleError> {
    let relation_names = schema
        .iter()
        .map(|relation| relation.name().to_owned())
        .collect::<BTreeSet<_>>();

    let mut statement = connection
        .prepare("SELECT relation FROM relation_manifest")
        .map_err(|source| sqlite_error("prepare relation manifest scan", path, source))?;
    let existing_relations = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| sqlite_error("scan relation manifest", path, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| sqlite_error("decode relation manifest", path, source))?;
    for relation in existing_relations {
        if !relation_names.contains(&relation) {
            delete_relation(connection, path, &relation)?;
        }
    }

    for relation in schema.iter() {
        ensure_relation_manifest(connection, path, relation)?;
    }
    Ok(())
}

fn ensure_relation_manifest(
    connection: &Connection,
    path: &Path,
    schema: &RelationSchema,
) -> Result<(), SouffleError> {
    let schema_json =
        serde_json::to_string(schema).map_err(|source| SouffleError::EncodeFailed {
            artifact: path.display().to_string(),
            message: source.to_string(),
        })?;
    let existing_schema = connection
        .query_row(
            "SELECT schema_json FROM relation_manifest WHERE relation = ?1",
            params![schema.name()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| sqlite_error("read relation schema", path, source))?;

    if existing_schema.as_deref() == Some(schema_json.as_str()) {
        return Ok(());
    }

    delete_relation(connection, path, schema.name())?;
    connection
        .execute(
            "INSERT INTO relation_manifest(relation, relation_id, schema_json, row_count)
             VALUES (?1, ?2, ?3, 0)",
            params![schema.name(), i64::from(schema.id().raw()), schema_json],
        )
        .map_err(|source| sqlite_error("write relation manifest", path, source))?;
    Ok(())
}

fn clear_exported_relations(connection: &Connection, path: &Path) -> Result<(), SouffleError> {
    connection
        .execute("DELETE FROM relation_rows", [])
        .map_err(|source| sqlite_error("clear relation rows", path, source))?;
    connection
        .execute("DELETE FROM relation_manifest", [])
        .map_err(|source| sqlite_error("clear relation manifest", path, source))?;
    Ok(())
}

fn write_relation(
    connection: &Connection,
    path: &Path,
    output: &RelationOutput,
) -> Result<SqliteRelationArtifact, SouffleError> {
    write_rows(
        connection,
        path,
        output.schema(),
        output.rows().iter().cloned(),
    )
}

fn write_streamed_relation(
    connection: &Connection,
    path: &Path,
    schema: &RelationSchema,
    rows: &mut RelationIterator<'_>,
) -> Result<SqliteRelationArtifact, SouffleError> {
    prepare_relation_write(connection, path, schema, 0)?;
    let relation = schema.name().to_owned();
    let mut row_count = 0;

    {
        let mut statement = connection
            .prepare(
                "INSERT INTO relation_rows(relation, row_index, row_json)
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(|source| sqlite_error("prepare row insert", path, source))?;
        while let Some(row) = rows.next_row()? {
            insert_row_json(&mut statement, path, schema, &relation, row_count, &row)?;
            row_count += 1;
        }
    }

    update_relation_row_count(connection, path, &relation, row_count)?;
    Ok(SqliteRelationArtifact {
        relation,
        relation_id: schema.id().raw(),
        schema: schema.clone(),
        row_count,
    })
}

fn write_rows(
    connection: &Connection,
    path: &Path,
    schema: &RelationSchema,
    rows: impl IntoIterator<Item = Row>,
) -> Result<SqliteRelationArtifact, SouffleError> {
    let relation = schema.name().to_owned();
    prepare_relation_write(connection, path, schema, 0)?;
    let mut row_count = 0;
    {
        let mut statement = connection
            .prepare(
                "INSERT INTO relation_rows(relation, row_index, row_json)
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(|source| sqlite_error("prepare row insert", path, source))?;
        for row in rows {
            insert_row_json(&mut statement, path, schema, &relation, row_count, &row)?;
            row_count += 1;
        }
    }

    update_relation_row_count(connection, path, &relation, row_count)?;
    Ok(SqliteRelationArtifact {
        relation,
        relation_id: schema.id().raw(),
        schema: schema.clone(),
        row_count,
    })
}

fn prepare_relation_write(
    connection: &Connection,
    path: &Path,
    schema: &RelationSchema,
    row_count: usize,
) -> Result<(), SouffleError> {
    let schema_json =
        serde_json::to_string(schema).map_err(|source| SouffleError::EncodeFailed {
            artifact: path.display().to_string(),
            message: source.to_string(),
        })?;
    connection
        .execute(
            "INSERT INTO relation_manifest(relation, relation_id, schema_json, row_count)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(relation) DO UPDATE SET
                relation_id = excluded.relation_id,
                schema_json = excluded.schema_json,
                row_count = excluded.row_count",
            params![
                schema.name(),
                i64::from(schema.id().raw()),
                schema_json,
                row_count as i64,
            ],
        )
        .map_err(|source| sqlite_error("write relation manifest", path, source))?;
    connection
        .execute(
            "DELETE FROM relation_rows WHERE relation = ?1",
            params![schema.name()],
        )
        .map_err(|source| sqlite_error("clear relation rows", path, source))?;
    Ok(())
}

fn insert_row_json(
    statement: &mut rusqlite::Statement<'_>,
    path: &Path,
    schema: &RelationSchema,
    relation: &str,
    row_index: usize,
    row: &Row,
) -> Result<(), SouffleError> {
    validate_row(schema, row)?;
    let row_json = serde_json::to_string(row).map_err(|source| SouffleError::EncodeFailed {
        artifact: path.display().to_string(),
        message: source.to_string(),
    })?;
    statement
        .execute(params![relation, row_index as i64, row_json])
        .map_err(|source| sqlite_error("insert row", path, source))?;
    Ok(())
}

fn update_relation_row_count(
    connection: &Connection,
    path: &Path,
    relation: &str,
    row_count: usize,
) -> Result<(), SouffleError> {
    connection
        .execute(
            "UPDATE relation_manifest SET row_count = ?2 WHERE relation = ?1",
            params![relation, row_count as i64],
        )
        .map_err(|source| sqlite_error("update relation row count", path, source))?;
    Ok(())
}

fn delete_relation(
    connection: &Connection,
    path: &Path,
    relation: &str,
) -> Result<(), SouffleError> {
    connection
        .execute(
            "DELETE FROM relation_rows WHERE relation = ?1",
            params![relation],
        )
        .map_err(|source| sqlite_error("delete relation rows", path, source))?;
    connection
        .execute(
            "DELETE FROM relation_manifest WHERE relation = ?1",
            params![relation],
        )
        .map_err(|source| sqlite_error("delete relation manifest", path, source))?;
    Ok(())
}

fn sqlite_error(operation: &str, path: &Path, source: rusqlite::Error) -> SouffleError {
    SouffleError::Sqlite {
        operation: operation.to_owned(),
        database: path.display().to_string(),
        message: source.to_string(),
    }
}

fn relation_row_count(
    connection: &Connection,
    path: &Path,
    relation: &str,
) -> Result<usize, SouffleError> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM relation_rows WHERE relation = ?1",
            params![relation],
            |row| decode_usize(row.get(0)?, 0),
        )
        .map_err(|source| sqlite_error("count relation rows", path, source))?;
    Ok(count)
}

fn decode_row_json(path: &Path, row_json: &str) -> Result<Row, SouffleError> {
    serde_json::from_str::<Row>(row_json).map_err(|source| SouffleError::ArtifactDecodeFailed {
        artifact: path.display().to_string(),
        message: source.to_string(),
    })
}

fn decode_u32(value: i64, column: usize) -> Result<u32, rusqlite::Error> {
    u32::try_from(value).map_err(|_| integer_conversion_error(column, value, "u32"))
}

fn decode_usize(value: i64, column: usize) -> Result<usize, rusqlite::Error> {
    usize::try_from(value).map_err(|_| integer_conversion_error(column, value, "usize"))
}

fn integer_conversion_error(column: usize, value: i64, target: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Integer,
        Box::new(SqliteIntegerError { value, target }),
    )
}

#[derive(Debug)]
struct SqliteIntegerError {
    value: i64,
    target: &'static str,
}

impl fmt::Display for SqliteIntegerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "SQLite integer {} cannot be represented as {}",
            self.value, self.target
        )
    }
}

impl std::error::Error for SqliteIntegerError {}
