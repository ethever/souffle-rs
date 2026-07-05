use std::{
    fs,
    num::NonZeroUsize,
    path::Path,
    time::{Duration, Instant},
};

use serde::Serialize;

use crate::{Backend, CpuBudget, RunOptions, SouffleError};

/// Machine-readable performance evidence for one backend run.
///
/// `file_count` and `bytes_written` describe relation-exchange artifacts such
/// as fact/output files, manifests, or durable backend rows that were written
/// for the measured run. Embedded and in-memory relation exchange should leave
/// these counters at zero; explicit file or SQLite export paths should record
/// their durable writes. `peak_rss_bytes` and `cpu_utilization` are filled from
/// explicit recorder setters or from host resource sampling when the platform
/// exposes that data.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PerformanceMetrics {
    backend: Backend,
    total_time: Duration,
    souffle_run_time: Duration,
    relation_insertion_time: Duration,
    relation_output_decode_time: Duration,
    file_count: u64,
    bytes_written: u64,
    metadata_operations: u64,
    peak_rss_bytes: Option<u64>,
    cpu_utilization: Option<f64>,
    openmp_threads: usize,
    rust_worker_count: usize,
}

impl PerformanceMetrics {
    /// Runtime backend used by the measured program.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Wall-clock duration from recorder creation to finish.
    pub fn total_time(&self) -> Duration {
        self.total_time
    }

    /// Time spent in the Souffle program run phase.
    pub fn souffle_run_time(&self) -> Duration {
        self.souffle_run_time
    }

    /// Time spent inserting input relation rows.
    pub fn relation_insertion_time(&self) -> Duration {
        self.relation_insertion_time
    }

    /// Time spent decoding or streaming output relation rows.
    pub fn relation_output_decode_time(&self) -> Duration {
        self.relation_output_decode_time
    }

    /// Number of relation-exchange files written by the measured run.
    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    /// Bytes written to relation-exchange artifacts.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Count of metadata operations attributed to relation exchange.
    pub fn metadata_operations(&self) -> u64 {
        self.metadata_operations
    }

    /// Peak resident set size in bytes, when supplied or sampled.
    pub fn peak_rss_bytes(&self) -> Option<u64> {
        self.peak_rss_bytes
    }

    /// Core-equivalent CPU utilization, when supplied or sampled.
    ///
    /// A value near `1.0` means the measured process consumed roughly one full
    /// CPU core for the recorder's wall-clock lifetime; multi-threaded runs may
    /// exceed `1.0`.
    pub fn cpu_utilization(&self) -> Option<f64> {
        self.cpu_utilization
    }

    /// Souffle/OpenMP thread count used for the measured run.
    pub fn openmp_threads(&self) -> usize {
        self.openmp_threads
    }

    /// Rust worker count around the measured run.
    pub fn rust_worker_count(&self) -> usize {
        self.rust_worker_count
    }

    /// Whether relation exchange avoided fact/output and durable backend files.
    pub fn relation_exchange_is_file_free(&self) -> bool {
        self.file_count == 0 && self.bytes_written == 0 && self.metadata_operations == 0
    }
}

/// Incremental recorder for backend performance evidence.
///
/// The recorder is intentionally explicit. Runtime code or benchmarks decide
/// which operation belongs to insertion, Souffle execution, output decode, file
/// writes, and metadata operations. Host RSS/CPU values are sampled
/// automatically where available and can be overridden by harness-provided
/// measurements. This keeps the safe API backend-neutral while producing one
/// stable metrics shape for benchmarks, smoke tests, and downstream harnesses.
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use souffle_rs::{
///     AttributeSchema, Backend, CpuBudget, InMemoryProgram, PerformanceRecorder,
///     Program, RelationBundle, RelationId, RelationSchema, TypeRef, Value,
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
/// let cpu_budget = CpuBudget::new(
///     NonZeroUsize::new(1).unwrap(),
///     NonZeroUsize::new(2).unwrap(),
/// );
/// let mut program = InMemoryProgram::builder("analysis")
///     .cpu_budget(cpu_budget.clone())
///     .schema(schema)
///     .build_memory()?;
/// let mut recorder = PerformanceRecorder::new(Backend::Memory, &cpu_budget);
///
/// recorder.measure_relation_insertion(|| program.insert_row("Input", [Value::Number(7)]))?;
/// recorder.measure_souffle_run(|| program.run())?;
///
/// let metrics = recorder.finish();
/// assert_eq!(metrics.openmp_threads(), 2);
/// assert!(metrics.relation_exchange_is_file_free());
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct PerformanceRecorder {
    backend: Backend,
    started_at: Instant,
    souffle_run_time: Duration,
    relation_insertion_time: Duration,
    relation_output_decode_time: Duration,
    file_count: u64,
    bytes_written: u64,
    metadata_operations: u64,
    peak_rss_bytes: Option<u64>,
    cpu_utilization: Option<f64>,
    openmp_threads: usize,
    rust_worker_count: usize,
    resource_start: ResourceSample,
}

impl PerformanceRecorder {
    /// Start recording for a backend and CPU budget.
    pub fn new(backend: Backend, cpu_budget: &CpuBudget) -> Self {
        Self::from_counts(
            backend,
            cpu_budget.rust_workers(),
            cpu_budget.souffle_threads(),
        )
    }

    /// Start recording from run options and an explicit Rust worker count.
    pub fn from_run_options(
        backend: Backend,
        run_options: &RunOptions,
        rust_workers: NonZeroUsize,
    ) -> Self {
        Self::from_counts(backend, rust_workers, run_options.threads())
    }

    fn from_counts(
        backend: Backend,
        rust_workers: NonZeroUsize,
        openmp_threads: NonZeroUsize,
    ) -> Self {
        Self {
            backend,
            started_at: Instant::now(),
            souffle_run_time: Duration::ZERO,
            relation_insertion_time: Duration::ZERO,
            relation_output_decode_time: Duration::ZERO,
            file_count: 0,
            bytes_written: 0,
            metadata_operations: 0,
            peak_rss_bytes: None,
            cpu_utilization: None,
            openmp_threads: openmp_threads.get(),
            rust_worker_count: rust_workers.get(),
            resource_start: ResourceSample::current(),
        }
    }

    /// Measure input relation insertion work.
    pub fn measure_relation_insertion<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        let started_at = Instant::now();
        let output = operation();
        self.relation_insertion_time += started_at.elapsed();
        output
    }

    /// Measure Souffle program execution work.
    pub fn measure_souffle_run<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        let started_at = Instant::now();
        let output = operation();
        self.souffle_run_time += started_at.elapsed();
        output
    }

    /// Measure output relation decode or streaming work.
    pub fn measure_output_decode<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        let started_at = Instant::now();
        let output = operation();
        self.relation_output_decode_time += started_at.elapsed();
        output
    }

    /// Record one relation-exchange file write and its byte count.
    pub fn record_file_write(&mut self, bytes_written: u64) {
        self.file_count = self.file_count.saturating_add(1);
        self.bytes_written = self.bytes_written.saturating_add(bytes_written);
    }

    /// Record relation-exchange metadata work such as create, stat, rename, or
    /// delete operations.
    pub fn record_metadata_operation(&mut self) {
        self.metadata_operations = self.metadata_operations.saturating_add(1);
    }

    /// Count relation-exchange artifacts under a file or directory path.
    ///
    /// Files increment `file_count` and add their length to `bytes_written`.
    /// Directories are walked recursively. Each filesystem metadata/read-dir
    /// lookup increments `metadata_operations`, making the value useful as a
    /// stable relative indicator of small-file pressure.
    pub fn record_artifact_path(&mut self, path: impl AsRef<Path>) -> Result<(), SouffleError> {
        self.record_artifact_path_inner(path.as_ref())
    }

    /// Supply peak resident set size measured by an external harness.
    ///
    /// This value takes precedence over automatic host sampling.
    pub fn set_peak_rss_bytes(&mut self, peak_rss_bytes: u64) {
        self.peak_rss_bytes = Some(peak_rss_bytes);
    }

    /// Supply CPU utilization measured by an external harness.
    ///
    /// This value takes precedence over automatic host sampling.
    pub fn set_cpu_utilization(&mut self, cpu_utilization: f64) {
        self.cpu_utilization = Some(cpu_utilization);
    }

    fn record_artifact_path_inner(&mut self, path: &Path) -> Result<(), SouffleError> {
        self.record_metadata_operation();
        let metadata = fs::metadata(path).map_err(|source| SouffleError::FileIo {
            operation: "inspect artifact metadata".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        })?;

        if metadata.is_file() {
            self.record_file_write(metadata.len());
            return Ok(());
        }

        if metadata.is_dir() {
            self.record_metadata_operation();
            let entries = fs::read_dir(path).map_err(|source| SouffleError::FileIo {
                operation: "scan artifact directory".to_owned(),
                path: path.display().to_string(),
                message: source.to_string(),
            })?;
            for entry in entries {
                let entry = entry.map_err(|source| SouffleError::FileIo {
                    operation: "read artifact directory entry".to_owned(),
                    path: path.display().to_string(),
                    message: source.to_string(),
                })?;
                self.record_artifact_path_inner(&entry.path())?;
            }
        }
        Ok(())
    }

    /// Finish recording and return immutable metrics.
    pub fn finish(self) -> PerformanceMetrics {
        let total_time = self.started_at.elapsed();
        let resource_end = ResourceSample::current();
        let peak_rss_bytes = self.peak_rss_bytes.or(resource_end.peak_rss_bytes);
        let cpu_utilization = self.cpu_utilization.or_else(|| {
            let start_cpu_time = self.resource_start.cpu_time?;
            let end_cpu_time = resource_end.cpu_time?;
            let cpu_time = end_cpu_time.checked_sub(start_cpu_time)?;
            let wall_seconds = total_time.as_secs_f64();
            if wall_seconds > 0.0 {
                Some(cpu_time.as_secs_f64() / wall_seconds)
            } else {
                None
            }
        });

        PerformanceMetrics {
            backend: self.backend,
            total_time,
            souffle_run_time: self.souffle_run_time,
            relation_insertion_time: self.relation_insertion_time,
            relation_output_decode_time: self.relation_output_decode_time,
            file_count: self.file_count,
            bytes_written: self.bytes_written,
            metadata_operations: self.metadata_operations,
            peak_rss_bytes,
            cpu_utilization,
            openmp_threads: self.openmp_threads,
            rust_worker_count: self.rust_worker_count,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ResourceSample {
    peak_rss_bytes: Option<u64>,
    cpu_time: Option<Duration>,
}

impl ResourceSample {
    fn current() -> Self {
        Self {
            peak_rss_bytes: current_peak_rss_bytes(),
            cpu_time: current_cpu_time(),
        }
    }
}

#[cfg(target_os = "linux")]
fn current_peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| parse_status_memory_kib(line, "VmHWM:"))
        .or_else(|| {
            status
                .lines()
                .find_map(|line| parse_status_memory_kib(line, "VmRSS:"))
        })
}

#[cfg(not(target_os = "linux"))]
fn current_peak_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn current_cpu_time() -> Option<Duration> {
    let schedstat = fs::read_to_string("/proc/self/schedstat").ok()?;
    let runtime_nanos = schedstat.split_whitespace().next()?.parse::<u64>().ok()?;
    Some(Duration::from_nanos(runtime_nanos))
}

#[cfg(not(target_os = "linux"))]
fn current_cpu_time() -> Option<Duration> {
    None
}

#[cfg(target_os = "linux")]
fn parse_status_memory_kib(line: &str, prefix: &str) -> Option<u64> {
    let value = line.strip_prefix(prefix)?.split_whitespace().next()?;
    value.parse::<u64>().ok()?.checked_mul(1024)
}
