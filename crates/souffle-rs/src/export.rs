use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    Backend, BuildInfo, Program, ProgramBuilder, ProgramConfig, RelationBundle, RelationIterator,
    RelationOutput, RelationSchema, Row, RunOptions, SouffleError,
    program::{RelationIteratorSource, validate_row},
};

const MANIFEST_FILE: &str = "manifest.json";
const SCHEMA_FILE: &str = "schema.json";
const RELATIONS_DIR: &str = "relations";
const FORMAT_VERSION: u32 = 1;

/// File-backed relation store used for explicit export, debugging, and backend
/// parity. It writes one schema artifact, one manifest, and one JSONL row file
/// per exported relation.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, FileRelationStore, InMemoryProgram, Program, RelationBundle,
///     RelationId, RelationSchema, Row, TypeRef, Value,
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
/// let tempdir = tempfile::tempdir().expect("temporary export root");
/// let store = FileRelationStore::new(tempdir.path());
/// let manifest = store.export_outputs(&program, ["Output"])?;
///
/// assert_eq!(manifest.relations[0].relation, "Output");
/// assert_eq!(manifest.relations[0].row_count, 1);
/// assert_eq!(store.load_outputs()?[0].rows()[0].values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRelationStore {
    root: PathBuf,
}

impl FileRelationStore {
    /// Create a relation store rooted at a deterministic directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Root directory for this store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Export selected printable relations from a program.
    pub fn export_outputs<P, S>(
        &self,
        program: &P,
        relations: impl IntoIterator<Item = S>,
    ) -> Result<FileExportManifest, SouffleError>
    where
        P: Program,
        S: AsRef<str>,
    {
        create_dir_all(&self.root)?;
        create_dir_all(&self.root.join(RELATIONS_DIR))?;

        let schema_path = PathBuf::from(SCHEMA_FILE);
        write_json(&self.root.join(&schema_path), program.schema_bundle())?;

        let mut artifacts = Vec::new();
        for relation in relations {
            let schema = program.relation_schema(relation.as_ref())?.clone();
            let mut rows = program.iter_relation(schema.name())?;
            artifacts.push(self.write_streamed_relation(&schema, &mut rows)?);
        }

        let manifest = FileExportManifest {
            format_version: FORMAT_VERSION,
            schema_path,
            relations: artifacts,
        };
        write_json(&self.root.join(MANIFEST_FILE), &manifest)?;
        Ok(manifest)
    }

    /// Load the export manifest.
    pub fn load_manifest(&self) -> Result<FileExportManifest, SouffleError> {
        read_json(&self.root.join(MANIFEST_FILE))
    }

    /// Load all relation outputs referenced by the manifest.
    pub fn load_outputs(&self) -> Result<Vec<RelationOutput>, SouffleError> {
        let manifest = self.load_manifest()?;
        manifest
            .relations
            .iter()
            .map(|artifact| self.load_relation(artifact))
            .collect()
    }

    pub(crate) fn initialize_runtime(&self, schema: &RelationBundle) -> Result<(), SouffleError> {
        self.prepare_dirs()?;

        let schema_path = PathBuf::from(SCHEMA_FILE);
        write_json(&self.root.join(&schema_path), schema)?;

        let existing = self
            .load_manifest_if_exists()?
            .map(|manifest| {
                manifest
                    .relations
                    .into_iter()
                    .map(|artifact| (artifact.relation.clone(), artifact))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let relation_names = schema
            .iter()
            .map(|relation| relation.name().to_owned())
            .collect::<BTreeSet<_>>();

        for (relation, artifact) in &existing {
            if !relation_names.contains(relation) {
                self.remove_relation_file(artifact)?;
            }
        }

        let mut artifacts = Vec::new();
        for relation in schema.iter() {
            if let Some(artifact) = existing.get(relation.name()) {
                if artifact.schema == *relation && self.root.join(&artifact.rows_path).exists() {
                    artifacts.push(artifact.clone());
                    continue;
                }
                self.remove_relation_file(artifact)?;
            }

            let output = RelationOutput::new(relation.clone(), Vec::new())?;
            artifacts.push(self.write_relation(&output)?);
        }

        self.write_manifest(FileExportManifest {
            format_version: FORMAT_VERSION,
            schema_path,
            relations: artifacts,
        })
    }

    pub(crate) fn replace_relation_rows(
        &self,
        schema: RelationSchema,
        rows: Vec<Row>,
    ) -> Result<FileRelationArtifact, SouffleError> {
        self.prepare_dirs()?;
        let output = RelationOutput::new(schema, rows)?;
        let mut manifest = self.runtime_manifest()?;
        let previous = manifest
            .relations
            .iter()
            .find(|artifact| artifact.relation == output.schema().name())
            .cloned();
        let artifact = self.write_relation(&output)?;
        if let Some(previous) = previous.filter(|previous| previous.rows_path != artifact.rows_path)
        {
            self.remove_relation_file(&previous)?;
        }
        manifest
            .relations
            .retain(|existing| existing.relation != artifact.relation);
        manifest.relations.push(artifact.clone());
        sort_artifacts(&mut manifest.relations);
        self.write_manifest(manifest)?;
        Ok(artifact)
    }

    pub(crate) fn load_relation_output(
        &self,
        schema: &RelationSchema,
    ) -> Result<RelationOutput, SouffleError> {
        let Some(manifest) = self.load_manifest_if_exists()? else {
            return RelationOutput::new(schema.clone(), Vec::new());
        };
        let Some(artifact) = manifest
            .relations
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
    ) -> Result<Option<FileRelationRows>, SouffleError> {
        let Some(manifest) = self.load_manifest_if_exists()? else {
            return Ok(None);
        };
        let Some(artifact) = manifest
            .relations
            .iter()
            .find(|artifact| artifact.relation == schema.name())
        else {
            return Ok(None);
        };
        let path = self.root.join(&artifact.rows_path);
        Ok(Some(FileRelationRows {
            reader: Some(BufReader::new(open_file(&path)?)),
            path,
            expected_row_count: artifact.row_count,
            decoded_row_count: 0,
            completed: false,
        }))
    }

    fn write_relation(
        &self,
        output: &RelationOutput,
    ) -> Result<FileRelationArtifact, SouffleError> {
        let rows = output.rows().iter().cloned();
        self.write_rows(output.schema(), rows)
    }

    fn write_streamed_relation(
        &self,
        schema: &RelationSchema,
        rows: &mut RelationIterator<'_>,
    ) -> Result<FileRelationArtifact, SouffleError> {
        let rows_path = PathBuf::from(RELATIONS_DIR).join(relation_file_name(schema));
        let absolute_rows_path = self.root.join(&rows_path);
        let file = create_file(&absolute_rows_path)?;
        let mut writer = BufWriter::new(file);
        let mut row_count = 0;

        while let Some(row) = rows.next_row()? {
            write_row_json(&mut writer, &absolute_rows_path, schema, &row)?;
            row_count += 1;
        }
        writer.flush().map_err(|source| SouffleError::FileIo {
            operation: "flush".to_owned(),
            path: absolute_rows_path.display().to_string(),
            message: source.to_string(),
        })?;

        Ok(FileRelationArtifact {
            relation: schema.name().to_owned(),
            schema: schema.clone(),
            rows_path,
            row_count,
        })
    }

    fn write_rows(
        &self,
        schema: &RelationSchema,
        rows: impl IntoIterator<Item = Row>,
    ) -> Result<FileRelationArtifact, SouffleError> {
        let rows_path = PathBuf::from(RELATIONS_DIR).join(relation_file_name(schema));
        let absolute_rows_path = self.root.join(&rows_path);
        let file = create_file(&absolute_rows_path)?;
        let mut writer = BufWriter::new(file);
        let mut row_count = 0;

        for row in rows {
            write_row_json(&mut writer, &absolute_rows_path, schema, &row)?;
            row_count += 1;
        }
        writer.flush().map_err(|source| SouffleError::FileIo {
            operation: "flush".to_owned(),
            path: absolute_rows_path.display().to_string(),
            message: source.to_string(),
        })?;

        Ok(FileRelationArtifact {
            relation: schema.name().to_owned(),
            schema: schema.clone(),
            rows_path,
            row_count,
        })
    }

    fn load_relation(
        &self,
        artifact: &FileRelationArtifact,
    ) -> Result<RelationOutput, SouffleError> {
        let absolute_rows_path = self.root.join(&artifact.rows_path);
        let file = open_file(&absolute_rows_path)?;
        let reader = BufReader::new(file);
        let mut rows = Vec::with_capacity(artifact.row_count);

        for line in reader.lines() {
            let line = line.map_err(|source| SouffleError::FileIo {
                operation: "read".to_owned(),
                path: absolute_rows_path.display().to_string(),
                message: source.to_string(),
            })?;
            if line.trim().is_empty() {
                continue;
            }
            rows.push(decode_row_json(&absolute_rows_path, &line)?);
        }

        if rows.len() != artifact.row_count {
            return Err(row_count_mismatch(
                &absolute_rows_path,
                artifact.row_count,
                rows.len(),
            ));
        }

        RelationOutput::new(artifact.schema.clone(), rows)
    }

    fn prepare_dirs(&self) -> Result<(), SouffleError> {
        create_dir_all(&self.root)?;
        create_dir_all(&self.root.join(RELATIONS_DIR))
    }

    fn load_manifest_if_exists(&self) -> Result<Option<FileExportManifest>, SouffleError> {
        let path = self.root.join(MANIFEST_FILE);
        if !path.exists() {
            return Ok(None);
        }
        read_json(&path).map(Some)
    }

    fn runtime_manifest(&self) -> Result<FileExportManifest, SouffleError> {
        Ok(self
            .load_manifest_if_exists()?
            .unwrap_or_else(|| FileExportManifest {
                format_version: FORMAT_VERSION,
                schema_path: PathBuf::from(SCHEMA_FILE),
                relations: Vec::new(),
            }))
    }

    fn write_manifest(&self, manifest: FileExportManifest) -> Result<(), SouffleError> {
        write_json(&self.root.join(MANIFEST_FILE), &manifest)
    }

    fn remove_relation_file(&self, artifact: &FileRelationArtifact) -> Result<(), SouffleError> {
        remove_file_if_exists(&self.root.join(&artifact.rows_path))
    }
}

/// File-backed dynamic program facade.
///
/// Rows are stored as JSONL relation artifacts under an explicit
/// [`FileRelationStore`]. This backend is intended for parity, debugging,
/// durable exports, and crash-inspection workflows where users want the same
/// safe [`Program`] API over inspectable files.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, FileProgram, FileRelationStore, Program, RelationBundle,
///     RelationId, RelationSchema, Row, TypeRef, Value,
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
/// let tempdir = tempfile::tempdir().expect("temporary file backend");
/// let store = FileRelationStore::new(tempdir.path());
/// let mut program = FileProgram::builder("analysis")
///     .schema(schema)
///     .file_store(store)
///     .build_file()?;
///
/// program.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
/// let mut rows = program.iter_relation("Output")?;
/// assert_eq!(rows.next_row()?.unwrap().values(), &[Value::Number(7)]);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct FileProgram {
    config: ProgramConfig,
    schema: RelationBundle,
    store: FileRelationStore,
    last_run_options: Option<RunOptions>,
}

impl FileProgram {
    /// Start building a file-backed program facade.
    pub fn builder(name: impl Into<String>) -> ProgramBuilder {
        ProgramBuilder::new(name).backend(Backend::File)
    }

    pub(crate) fn from_config(
        config: ProgramConfig,
        schema: RelationBundle,
        store: FileRelationStore,
    ) -> Result<Self, SouffleError> {
        schema.validate()?;
        store.initialize_runtime(&schema)?;
        Ok(Self {
            config: config.with_backend(Backend::File),
            schema,
            store,
            last_run_options: None,
        })
    }

    /// File store backing this program facade.
    pub fn store(&self) -> &FileRelationStore {
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

impl Program for FileProgram {
    fn name(&self) -> &str {
        self.config.name()
    }

    fn backend(&self) -> Backend {
        Backend::File
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
            Backend::File,
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

/// Manifest for a file relation export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileExportManifest {
    /// Manifest format version for forward-compatible readers.
    pub format_version: u32,
    /// Path to the schema JSON artifact relative to the export root.
    pub schema_path: PathBuf,
    /// Exported relation row artifacts.
    pub relations: Vec<FileRelationArtifact>,
}

/// One relation row file referenced by a file export manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRelationArtifact {
    /// Relation name.
    pub relation: String,
    /// Schema used to encode the row file.
    pub schema: RelationSchema,
    /// Path to newline-delimited row data relative to the export root.
    pub rows_path: PathBuf,
    /// Number of encoded rows in the row file.
    pub row_count: usize,
}

#[derive(Debug)]
pub(crate) struct FileRelationRows {
    path: PathBuf,
    reader: Option<BufReader<File>>,
    expected_row_count: usize,
    decoded_row_count: usize,
    completed: bool,
}

impl RelationIteratorSource for FileRelationRows {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        if self.completed {
            return Ok(None);
        }
        let Some(reader) = &mut self.reader else {
            self.completed = true;
            return Ok(None);
        };

        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|source| SouffleError::FileIo {
                    operation: "read".to_owned(),
                    path: self.path.display().to_string(),
                    message: source.to_string(),
                })?;
            if bytes == 0 {
                self.reader = None;
                self.completed = true;
                if self.decoded_row_count != self.expected_row_count {
                    return Err(row_count_mismatch(
                        &self.path,
                        self.expected_row_count,
                        self.decoded_row_count,
                    ));
                }
                return Ok(None);
            }

            trim_line_ending(&mut line);
            if line.trim().is_empty() {
                continue;
            }
            if self.decoded_row_count >= self.expected_row_count {
                return Err(row_count_mismatch(
                    &self.path,
                    self.expected_row_count,
                    self.decoded_row_count.saturating_add(1),
                ));
            }

            let row = decode_row_json(&self.path, &line)?;
            validate_row(schema, &row)?;
            self.decoded_row_count += 1;
            return Ok(Some(row));
        }
    }
}

fn relation_file_name(schema: &RelationSchema) -> String {
    format!("{:08}_{}.jsonl", schema.id().raw(), sanitize(schema.name()))
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch == '_' || ch == '-' || ch == '.' || ch.is_ascii_alphanumeric() {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn create_dir_all(path: &Path) -> Result<(), SouffleError> {
    fs::create_dir_all(path).map_err(|source| SouffleError::FileIo {
        operation: "create directory".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

fn create_file(path: &Path) -> Result<File, SouffleError> {
    File::create(path).map_err(|source| SouffleError::FileIo {
        operation: "create file".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

fn open_file(path: &Path) -> Result<File, SouffleError> {
    File::open(path).map_err(|source| SouffleError::FileIo {
        operation: "open file".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

fn remove_file_if_exists(path: &Path) -> Result<(), SouffleError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SouffleError::FileIo {
            operation: "remove file".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        }),
    }
}

fn write_row_json(
    writer: &mut impl Write,
    path: &Path,
    schema: &RelationSchema,
    row: &Row,
) -> Result<(), SouffleError> {
    validate_row(schema, row)?;
    serde_json::to_writer(&mut *writer, row).map_err(|source| SouffleError::EncodeFailed {
        artifact: path.display().to_string(),
        message: source.to_string(),
    })?;
    writer
        .write_all(b"\n")
        .map_err(|source| SouffleError::FileIo {
            operation: "write".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        })
}

fn decode_row_json(path: &Path, line: &str) -> Result<Row, SouffleError> {
    serde_json::from_str::<Row>(line).map_err(|source| SouffleError::ArtifactDecodeFailed {
        artifact: path.display().to_string(),
        message: source.to_string(),
    })
}

fn row_count_mismatch(path: &Path, expected: usize, actual: usize) -> SouffleError {
    SouffleError::ArtifactDecodeFailed {
        artifact: path.display().to_string(),
        message: format!("expected {expected} rows from manifest but decoded {actual}"),
    }
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

fn write_json<T>(path: &Path, value: &T) -> Result<(), SouffleError>
where
    T: Serialize + ?Sized,
{
    let file = create_file(path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer(writer, value).map_err(|source| SouffleError::EncodeFailed {
        artifact: path.display().to_string(),
        message: source.to_string(),
    })
}

fn read_json<T>(path: &Path) -> Result<T, SouffleError>
where
    T: for<'de> Deserialize<'de>,
{
    let file = open_file(path)?;
    serde_json::from_reader(BufReader::new(file)).map_err(|source| {
        SouffleError::ArtifactDecodeFailed {
            artifact: path.display().to_string(),
            message: source.to_string(),
        }
    })
}

fn sort_artifacts(artifacts: &mut [FileRelationArtifact]) {
    artifacts.sort_by(|left, right| {
        left.schema
            .id()
            .cmp(&right.schema.id())
            .then_with(|| left.relation.cmp(&right.relation))
    });
}
