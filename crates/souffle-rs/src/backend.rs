use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::SouffleError;

/// Execution backend used by a Souffle program facade.
///
/// The safe [`crate::Program`] trait is intentionally shared by every backend:
/// callers insert [`crate::Row`] values, run the program, and stream printable
/// relations the same way whether the rows are exchanged through embedded C++,
/// an isolated process, JSONL files, SQLite, or Rust-owned memory.
///
/// # Example
///
/// ```
/// use souffle_rs::{Backend, ProgramConfig};
///
/// let config = ProgramConfig::new("analysis").with_backend(Backend::Sqlite);
///
/// assert_eq!(config.backend(), Backend::Sqlite);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    /// In-process generated C++ through the `souffle-rs` C ABI.
    Embedded,
    /// Isolated Souffle process using explicit relation import/export.
    Process,
    /// File-backed relation exchange for debugging and parity checks.
    File,
    /// SQLite-backed relation exchange for interoperability and large exports.
    Sqlite,
    /// Rust-owned relation storage used by adapters, tests, and parity tooling.
    Memory,
}

/// Explicit CPU budget for Rust orchestration and Souffle execution.
///
/// `rust_workers` describes how many Rust tasks may run generated programs
/// concurrently. `souffle_threads` is the OpenMP thread count passed into each
/// generated Souffle program. The worst-case concurrent native thread budget is
/// therefore `rust_workers * souffle_threads`.
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use souffle_rs::CpuBudget;
///
/// let budget = CpuBudget::new(
///     NonZeroUsize::new(2).unwrap(),
///     NonZeroUsize::new(4).unwrap(),
/// );
///
/// assert_eq!(budget.rust_workers().get(), 2);
/// assert_eq!(budget.souffle_threads().get(), 4);
/// assert_eq!(budget.max_concurrent_threads(), 8);
/// assert!(budget
///     .validate_against_available_parallelism(NonZeroUsize::new(8).unwrap())
///     .is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuBudget {
    rust_workers: NonZeroUsize,
    souffle_threads: NonZeroUsize,
}

impl CpuBudget {
    /// Create a CPU budget with independent Rust worker and Souffle thread
    /// counts.
    pub fn new(rust_workers: NonZeroUsize, souffle_threads: NonZeroUsize) -> Self {
        Self {
            rust_workers,
            souffle_threads,
        }
    }

    /// Return a budget with a different Rust worker count.
    pub fn with_rust_workers(self, rust_workers: NonZeroUsize) -> Self {
        Self {
            rust_workers,
            ..self
        }
    }

    /// Return a budget with a different Souffle OpenMP thread count.
    pub fn with_souffle_threads(self, souffle_threads: NonZeroUsize) -> Self {
        Self {
            souffle_threads,
            ..self
        }
    }

    /// Number of Rust worker threads allowed around the Souffle program.
    pub fn rust_workers(&self) -> NonZeroUsize {
        self.rust_workers
    }

    /// Number of threads passed to the Souffle generated program.
    pub fn souffle_threads(&self) -> NonZeroUsize {
        self.souffle_threads
    }

    /// Worst-case concurrent native threads for this budget.
    ///
    /// This uses saturating multiplication so pathological `usize::MAX`
    /// budgets remain diagnostic instead of overflowing.
    pub fn max_concurrent_threads(&self) -> usize {
        self.rust_workers
            .get()
            .saturating_mul(self.souffle_threads.get())
    }

    /// Return a typed diagnostic if this budget can oversubscribe the supplied
    /// host parallelism.
    pub fn validate_against_available_parallelism(
        &self,
        available_threads: NonZeroUsize,
    ) -> Result<(), SouffleError> {
        let requested_threads = self.max_concurrent_threads();
        let available_threads = available_threads.get();
        if requested_threads > available_threads {
            return Err(SouffleError::ThreadOversubscription {
                rust_workers: self.rust_workers.get(),
                souffle_threads: self.souffle_threads.get(),
                requested_threads,
                available_threads,
            });
        }
        Ok(())
    }
}

impl Default for CpuBudget {
    fn default() -> Self {
        let one = NonZeroUsize::new(1).expect("1 is non-zero");
        Self::new(one, one)
    }
}

/// Runtime options for one program execution.
///
/// `RunOptions` are per-run settings. The builder-level [`CpuBudget`] supplies
/// the default thread count used by [`crate::Program::run`], while
/// [`crate::Program::run_with_options`] accepts an explicit `RunOptions` value
/// for one invocation.
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use souffle_rs::RunOptions;
///
/// let options = RunOptions::new(NonZeroUsize::new(8).unwrap());
///
/// assert_eq!(options.threads().get(), 8);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOptions {
    threads: NonZeroUsize,
}

impl RunOptions {
    /// Create run options with an explicit Souffle thread count.
    pub fn new(threads: NonZeroUsize) -> Self {
        Self { threads }
    }

    /// Create run options from the Souffle side of a CPU budget.
    ///
    /// # Example
    ///
    /// ```
    /// use std::num::NonZeroUsize;
    ///
    /// use souffle_rs::{CpuBudget, RunOptions};
    ///
    /// let budget = CpuBudget::new(
    ///     NonZeroUsize::new(2).unwrap(),
    ///     NonZeroUsize::new(6).unwrap(),
    /// );
    /// let options = RunOptions::from_cpu_budget(&budget);
    ///
    /// assert_eq!(options.threads().get(), 6);
    /// ```
    pub fn from_cpu_budget(cpu_budget: &CpuBudget) -> Self {
        Self::new(cpu_budget.souffle_threads())
    }

    /// Thread count that must be passed to the generated Souffle program.
    pub fn threads(&self) -> NonZeroUsize {
        self.threads
    }
}

impl Default for RunOptions {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(1).expect("1 is non-zero"))
    }
}

/// Static configuration for creating a program facade.
///
/// This is the backend-neutral part of [`crate::ProgramBuilder`]. Generated
/// program names, backend selection, and CPU budgeting live here so embedded,
/// process, file, SQLite, and in-memory facades can report a uniform
/// [`crate::BuildInfo`].
///
/// # Example
///
/// ```
/// use std::num::NonZeroUsize;
///
/// use souffle_rs::{Backend, CpuBudget, ProgramConfig};
///
/// let config = ProgramConfig::new("analysis")
///     .with_backend(Backend::Process)
///     .with_cpu_budget(CpuBudget::new(
///         NonZeroUsize::new(1).unwrap(),
///         NonZeroUsize::new(4).unwrap(),
///     ));
///
/// assert_eq!(config.name(), "analysis");
/// assert_eq!(config.backend(), Backend::Process);
/// assert_eq!(config.cpu_budget().souffle_threads().get(), 4);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramConfig {
    name: String,
    backend: Backend,
    cpu_budget: CpuBudget,
}

impl ProgramConfig {
    /// Create a configuration for a named generated Souffle program.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backend: Backend::Embedded,
            cpu_budget: CpuBudget::default(),
        }
    }

    /// Select the runtime backend.
    pub fn with_backend(mut self, backend: Backend) -> Self {
        self.backend = backend;
        self
    }

    /// Set explicit CPU budgeting for Rust and Souffle execution.
    pub fn with_cpu_budget(mut self, cpu_budget: CpuBudget) -> Self {
        self.cpu_budget = cpu_budget;
        self
    }

    /// Return a typed diagnostic if the configured CPU budget can
    /// oversubscribe the supplied host parallelism.
    ///
    /// # Example
    ///
    /// ```
    /// use std::num::NonZeroUsize;
    ///
    /// use souffle_rs::{CpuBudget, ProgramConfig};
    ///
    /// let config = ProgramConfig::new("analysis").with_cpu_budget(CpuBudget::new(
    ///     NonZeroUsize::new(2).unwrap(),
    ///     NonZeroUsize::new(4).unwrap(),
    /// ));
    ///
    /// assert!(config
    ///     .validate_cpu_budget(NonZeroUsize::new(8).unwrap())
    ///     .is_ok());
    /// ```
    pub fn validate_cpu_budget(&self, available_threads: NonZeroUsize) -> Result<(), SouffleError> {
        self.cpu_budget
            .validate_against_available_parallelism(available_threads)
    }

    /// Generated program name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Runtime backend.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// CPU budget for orchestration and Souffle execution.
    pub fn cpu_budget(&self) -> &CpuBudget {
        &self.cpu_budget
    }
}

/// Configuration for the isolated process backend.
///
/// The executable must be a compiled Souffle program that accepts Souffle's
/// standard `-F`, `-D`, and `-j` flags. The working directory is owned by the
/// backend and is used for fact and output exchange.
///
/// # Example
///
/// ```
/// use std::time::Duration;
///
/// use souffle_rs::ProcessConfig;
///
/// let config = ProcessConfig::new("target/souffle/analysis", "target/souffle/run")
///     .with_timeout(Duration::from_secs(30));
///
/// assert_eq!(config.executable().to_string_lossy(), "target/souffle/analysis");
/// assert_eq!(config.timeout(), Some(Duration::from_secs(30)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessConfig {
    executable: PathBuf,
    work_dir: PathBuf,
    timeout: Option<Duration>,
}

impl ProcessConfig {
    /// Create process backend configuration from a compiled Souffle executable
    /// and a backend-owned working directory.
    pub fn new(executable: impl Into<PathBuf>, work_dir: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            work_dir: work_dir.into(),
            timeout: None,
        }
    }

    /// Set the maximum wall-clock duration for one isolated process run.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Compiled Souffle executable invoked by the process backend.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Backend-owned directory used for fact and output exchange.
    pub fn work_dir(&self) -> &Path {
        &self.work_dir
    }

    /// Maximum wall-clock duration for one isolated process run.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}
