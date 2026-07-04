use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::NativeLinkMode;

/// Planned build steps and Cargo rebuild directives.
///
/// A plan is side-effect free: it validates configuration and records the
/// Souffle commands and Cargo directives that [`crate::Build::compile`] would
/// use, without spawning Souffle or writing artifacts.
///
/// # Example
///
/// ```
/// use souffle_rs_build::Build;
///
/// let plan = Build::new()
///     .program("analysis", "logic/main.dl")
///     .include_dir("logic/include")
///     .plan()
///     .unwrap();
///
/// assert_eq!(plan.souffle_commands()[0].program(), "analysis");
/// assert!(plan
///     .cargo_directives()
///     .iter()
///     .any(|directive| directive.render() == "cargo:rerun-if-changed=logic/main.dl"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
    souffle_commands: Vec<SouffleCommand>,
    cargo_directives: Vec<CargoDirective>,
}

impl BuildPlan {
    pub(crate) fn new(
        souffle_commands: Vec<SouffleCommand>,
        cargo_directives: Vec<CargoDirective>,
    ) -> Self {
        Self {
            souffle_commands,
            cargo_directives,
        }
    }

    /// Souffle generation commands in execution order.
    pub fn souffle_commands(&self) -> &[SouffleCommand] {
        &self.souffle_commands
    }

    /// Cargo directives that should be printed by `build.rs`.
    pub fn cargo_directives(&self) -> &[CargoDirective] {
        &self.cargo_directives
    }
}

/// One planned `souffle` invocation.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, GeneratedMode};
///
/// let plan = Build::new()
///     .program("analysis", "logic/main.dl")
///     .generated_mode(GeneratedMode::SingleFile)
///     .plan()
///     .unwrap();
/// let command = &plan.souffle_commands()[0];
///
/// assert_eq!(command.program(), "analysis");
/// assert_eq!(command.executable().to_string_lossy(), "souffle");
/// assert!(command.command_line().contains("logic/main.dl"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SouffleCommand {
    program: String,
    executable: PathBuf,
    working_dir: PathBuf,
    args: Vec<OsString>,
}

impl SouffleCommand {
    pub(crate) fn new(
        program: impl Into<String>,
        executable: PathBuf,
        working_dir: PathBuf,
        args: Vec<OsString>,
    ) -> Self {
        Self {
            program: program.into(),
            executable,
            working_dir,
            args,
        }
    }

    /// Program name this command generates.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Souffle binary path.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Working directory for diagnostics and reproducibility.
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    /// Command arguments.
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Render the command for diagnostics.
    pub fn command_line(&self) -> String {
        std::iter::once(self.executable.as_os_str())
            .chain(self.args.iter().map(OsString::as_os_str))
            .map(|part| part.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Cargo directive emitted by the build helper.
///
/// Use [`CargoDirective::render`] when writing a custom build script that wants
/// to inspect or filter directives before printing them.
///
/// # Example
///
/// ```
/// use std::path::PathBuf;
///
/// use souffle_rs_build::{CargoDirective, NativeLinkMode};
///
/// let directives = [
///     CargoDirective::RerunIfChanged(PathBuf::from("logic/main.dl")),
///     CargoDirective::RustcLinkLib {
///         mode: NativeLinkMode::Dynamic,
///         library: "z3".into(),
///     },
/// ];
/// let rendered = directives
///     .iter()
///     .map(CargoDirective::render)
///     .collect::<Vec<_>>();
///
/// assert_eq!(rendered[0], "cargo:rerun-if-changed=logic/main.dl");
/// assert_eq!(rendered[1], "cargo:rustc-link-lib=dylib=z3");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CargoDirective {
    /// Re-run the build script when a path changes.
    RerunIfChanged(PathBuf),
    /// Re-run the build script when an environment variable changes.
    RerunIfEnvChanged(String),
    /// Add a native library search path for Rust linking.
    RustcLinkSearch(PathBuf),
    /// Link a native library with an explicit static or dynamic mode.
    RustcLinkLib {
        /// Static or dynamic native link mode.
        mode: NativeLinkMode,
        /// Library name to pass to Cargo.
        library: String,
    },
    /// Add a target-specific native linker argument.
    RustcLinkArg(String),
    /// Set a compile-time environment variable for dependent Rust code.
    RustcEnv {
        /// Environment variable name.
        key: String,
        /// Environment variable value.
        value: String,
    },
}

impl CargoDirective {
    /// Render as the `cargo:` line expected by build scripts.
    pub fn render(&self) -> String {
        match self {
            Self::RerunIfChanged(path) => {
                format!("cargo:rerun-if-changed={}", path.display())
            }
            Self::RerunIfEnvChanged(name) => {
                format!("cargo:rerun-if-env-changed={name}")
            }
            Self::RustcLinkSearch(path) => {
                format!("cargo:rustc-link-search={}", path.display())
            }
            Self::RustcLinkLib { mode, library } => {
                format!("cargo:rustc-link-lib={}={library}", link_mode_kind(*mode))
            }
            Self::RustcLinkArg(argument) => {
                format!("cargo:rustc-link-arg={argument}")
            }
            Self::RustcEnv { key, value } => {
                format!("cargo:rustc-env={key}={value}")
            }
        }
    }
}

fn link_mode_kind(mode: NativeLinkMode) -> &'static str {
    mode.into()
}
