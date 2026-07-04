mod facts;

use std::{
    collections::BTreeMap,
    io::Read,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use crate::{
    Backend, BuildInfo, ProcessConfig, Program, ProgramBuilder, ProgramConfig, RelationBundle,
    RelationIterator, Row, RunOptions, SouffleError, program::validate_row,
};

use facts::{
    ensure_relation_supported, ensure_row_fact_encodable, prepare_exchange_dir,
    process_failure_message, stream_output_file, write_fact_file,
};

const FACTS_DIR: &str = "facts";
const OUTPUT_DIR: &str = "output";
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Isolated process backend using Souffle fact/output files as the exchange
/// boundary. This backend is intentionally explicit: unlike the embedded
/// backend, it preserves crash isolation and filesystem artifacts for parity and
/// debugging.
///
/// The configured executable must be a compiled Souffle program. At runtime the
/// backend writes loadable relations under `<work_dir>/facts`, runs the program
/// with `-F`, `-D`, and `-j`, then streams printable relations from
/// `<work_dir>/output`.
///
/// **Performance note:** this backend intentionally pays file IO, process
/// startup, and text fact/CSV parsing costs for isolation and inspectability.
/// It is useful for parity and debugging, but embedded or future batch APIs are
/// better fits for high-throughput ingestion. Prefer [`Program::iter_relation`]
/// over [`Program::read_relation`] when reading large process outputs.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
///
/// use souffle_rs::{
///     AttributeSchema, ProcessConfig, ProcessProgram, Program, RelationBundle,
///     RelationId, RelationSchema, TypeRef, Value,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema: RelationBundle = [
///     RelationSchema::input(
///         RelationId::new(0),
///         "Edge",
///         [
///             AttributeSchema::new("src", TypeRef::Symbol),
///             AttributeSchema::new("dst", TypeRef::Symbol),
///         ],
///     ),
///     RelationSchema::output(
///         RelationId::new(1),
///         "Reachable",
///         [AttributeSchema::new("node", TypeRef::Symbol)],
///     ),
/// ]
/// .into_iter()
/// .collect();
/// let work_dir = tempfile::tempdir().expect("temporary process workspace");
/// let mut program = ProcessProgram::builder("reachability")
///     .schema(schema)
///     .process_config(
///         ProcessConfig::new("target/souffle/reachability", work_dir.path())
///             .with_timeout(Duration::from_secs(10)),
///     )
///     .build_process()?;
///
/// program.insert_row("Edge", [Value::Symbol("a".into()), Value::Symbol("b".into())])?;
/// program.run()?;
/// let mut reachable = program.iter_relation("Reachable")?;
/// let _first = reachable.next_row()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ProcessProgram {
    config: ProgramConfig,
    schema: RelationBundle,
    process: ProcessConfig,
    input_rows: BTreeMap<String, Vec<Row>>,
    last_run_options: Option<RunOptions>,
}

impl ProcessProgram {
    /// Start building an isolated process facade.
    pub fn builder(name: impl Into<String>) -> ProgramBuilder {
        ProgramBuilder::new(name).backend(Backend::Process)
    }

    pub(crate) fn from_config(
        config: ProgramConfig,
        schema: RelationBundle,
        process: ProcessConfig,
    ) -> Result<Self, SouffleError> {
        schema.validate()?;
        validate_process_config(&process)?;
        let input_rows = schema
            .iter()
            .filter(|relation| relation.is_loadable())
            .map(|relation| (relation.name().to_owned(), Vec::new()))
            .collect();
        Ok(Self {
            config: config.with_backend(Backend::Process),
            schema,
            process,
            input_rows,
            last_run_options: None,
        })
    }

    /// Process backend configuration.
    pub fn process_config(&self) -> &ProcessConfig {
        &self.process
    }

    /// Default options used by the latest `run` call, if any.
    pub fn last_run_options(&self) -> Option<&RunOptions> {
        self.last_run_options.as_ref()
    }

    fn facts_dir(&self) -> PathBuf {
        self.process.work_dir().join(FACTS_DIR)
    }

    fn output_dir(&self) -> PathBuf {
        self.process.work_dir().join(OUTPUT_DIR)
    }

    fn write_input_facts(&self) -> Result<(), SouffleError> {
        prepare_exchange_dir(&self.facts_dir())?;
        for relation in self.schema.iter().filter(|relation| relation.is_loadable()) {
            ensure_relation_supported(Backend::Process, relation)?;
            let rows = self
                .input_rows
                .get(relation.name())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            write_fact_file(&self.facts_dir(), relation, rows)?;
        }
        Ok(())
    }
}

impl Program for ProcessProgram {
    fn name(&self) -> &str {
        self.config.name()
    }

    fn backend(&self) -> Backend {
        Backend::Process
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
            Backend::Process,
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
        ensure_relation_supported(Backend::Process, schema)?;

        let row = row.into();
        validate_row(schema, &row)?;
        ensure_row_fact_encodable(schema, &row)?;
        self.input_rows
            .entry(relation.to_owned())
            .or_default()
            .push(row);
        Ok(())
    }

    fn run_with_options(&mut self, options: RunOptions) -> Result<(), SouffleError> {
        self.write_input_facts()?;
        prepare_exchange_dir(&self.output_dir())?;

        let mut command = Command::new(self.process.executable());
        command
            .arg("-F")
            .arg(self.facts_dir())
            .arg("-D")
            .arg(self.output_dir())
            .arg("-j")
            .arg(options.threads().get().to_string())
            .current_dir(self.process.work_dir());

        let output =
            run_command_with_timeout(&mut command, self.process.timeout()).map_err(|message| {
                SouffleError::RunFailed {
                    program: self.name().to_owned(),
                    message,
                }
            })?;

        if !output.status.success() {
            return Err(SouffleError::RunFailed {
                program: self.name().to_owned(),
                message: process_failure_message(&output),
            });
        }

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
        ensure_relation_supported(Backend::Process, schema)?;
        let source = stream_output_file(&self.output_dir(), schema)?;
        Ok(RelationIterator::from_source(schema.clone(), source))
    }
}

fn validate_process_config(config: &ProcessConfig) -> Result<(), SouffleError> {
    if config.executable().as_os_str().is_empty() {
        return Err(SouffleError::ProcessConfiguration {
            field: "executable".to_owned(),
            message: "path is empty".to_owned(),
        });
    }
    if config.work_dir().as_os_str().is_empty() {
        return Err(SouffleError::ProcessConfiguration {
            field: "work_dir".to_owned(),
            message: "path is empty".to_owned(),
        });
    }
    Ok(())
}

fn run_command_with_timeout(
    command: &mut Command,
    timeout: Option<Duration>,
) -> Result<Output, String> {
    let executable = command.get_program().to_string_lossy().into_owned();
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_with_busy_retry(command, &executable)?;

    let Some(stdout) = child.stdout.take() else {
        terminate_child(&mut child);
        return Err("failed to capture process stdout".to_owned());
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_child(&mut child);
        return Err("failed to capture process stderr".to_owned());
    };

    let stdout_reader = spawn_pipe_reader(stdout);
    let stderr_reader = spawn_pipe_reader(stderr);
    let (status, timed_out) = wait_for_child(&mut child, timeout)?;
    let output = Output {
        status,
        stdout: join_pipe_reader(stdout_reader, "stdout")?,
        stderr: join_pipe_reader(stderr_reader, "stderr")?,
    };

    if let Some(timeout) = timed_out {
        return Err(process_timeout_message(timeout, &output));
    }

    Ok(output)
}

fn spawn_with_busy_retry(command: &mut Command, executable: &str) -> Result<Child, String> {
    let mut attempts = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(source) if is_executable_busy(&source) && attempts < 5 => {
                attempts += 1;
                thread::sleep(Duration::from_millis(10));
            }
            Err(source) => return Err(format!("failed to spawn `{executable}`: {source}")),
        }
    }
}

fn is_executable_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}

fn wait_for_child(
    child: &mut Child,
    timeout: Option<Duration>,
) -> Result<(ExitStatus, Option<Duration>), String> {
    let Some(timeout) = timeout else {
        return child
            .wait()
            .map(|status| (status, None))
            .map_err(|source| format!("failed to wait for process: {source}"));
    };

    let started_at = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|source| format!("failed to wait for process: {source}"))?
        {
            return Ok((status, None));
        }

        if started_at.elapsed() >= timeout {
            terminate_child(child);
            let status = child
                .wait()
                .map_err(|source| format!("failed to wait for timed-out process: {source}"))?;
            return Ok((status, Some(timeout)));
        }

        let remaining = timeout.saturating_sub(started_at.elapsed());
        thread::sleep(std::cmp::min(remaining, PROCESS_POLL_INTERVAL));
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
}

fn spawn_pipe_reader<R>(mut pipe: R) -> thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = Vec::new();
        pipe.read_to_end(&mut buffer).map(|_| buffer)
    })
}

fn join_pipe_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, String> {
    reader
        .join()
        .map_err(|_| format!("process {stream} reader panicked"))?
        .map_err(|source| format!("failed to read process {stream}: {source}"))
}

fn process_timeout_message(timeout: Duration, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "process timed out after {timeout:?} and was terminated with status {}; stdout: {}; stderr: {}",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}
