use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use souffle_rs::RelationBundle;
use strum::IntoStaticStr;

use crate::{
    BuildError, BuildMetadata, BuildPlan, CargoDirective, ExternalLibraryMetadata,
    NativeBuildMetadata, OpenMpMetadata, ProgramMetadata, SouffleCommand,
    artifacts::{emit_requested_artifacts, validate_schema_bundle},
    execute::{
        compile_native_artifacts, emit_cargo_directives, emit_metadata, run_souffle_generation,
    },
    schema_extract::extract_schema_bundle,
};

const NATIVE_STATIC_LIBRARY: &str = "souffle_rs_generated";
const CARGO_OUT_DIR_SUBDIRECTORY: &str = "souffle-rs";
const NATIVE_COMPILER_ENV_PREFIXES: &[&str] = &["CC", "CFLAGS", "CXX", "CXXFLAGS", "CXXSTDLIB"];
const NATIVE_COMPILER_ENV_BASE: &[&str] = &["HOST", "OPT_LEVEL", "PROFILE", "TARGET"];

/// Souffle generated output mode.
///
/// Directory mode uses Souffle's `-G` output and is best for normal native
/// compilation. Single-file mode uses `-g` and is useful for tooling that wants
/// one generated C++ file.
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
///
/// assert!(plan.souffle_commands()[0].command_line().contains("-g"));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GeneratedMode {
    /// Use `souffle -G` and emit a generated C++ directory.
    Directory,
    /// Use `souffle -g` and emit one generated C++ source file.
    SingleFile,
}

/// Native link mode for one external artifact class.
///
/// This is rendered into Cargo link directives as `dylib` or `static`.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{CargoDirective, NativeLinkMode};
///
/// let directive = CargoDirective::RustcLinkLib {
///     mode: NativeLinkMode::Static,
///     library: "souffle_rs_generated".into(),
/// };
///
/// assert_eq!(
///     directive.render(),
///     "cargo:rustc-link-lib=static=souffle_rs_generated",
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, IntoStaticStr)]
#[serde(rename_all = "kebab-case")]
pub enum NativeLinkMode {
    /// Link the native library dynamically.
    #[strum(serialize = "dylib")]
    Dynamic,
    /// Link the native library statically.
    #[strum(serialize = "static")]
    Static,
}

/// Overall native link mode for generated C++, wrapper, and external libraries.
///
/// Use [`LinkMode::Dynamic`] for the most portable development setup. Static
/// modes are useful when generated C++ should be archived into a Rust crate or
/// when deployment needs fewer runtime library dependencies.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, LinkMode};
///
/// let metadata = Build::new()
///     .program("analysis", "logic/main.dl")
///     .link_mode(LinkMode::StaticGeneratedDynamicExternal)
///     .metadata()
///     .unwrap();
///
/// assert_eq!(metadata.link_mode, LinkMode::StaticGeneratedDynamicExternal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkMode {
    /// Preserve dynamic linking for generated and external native artifacts.
    Dynamic,
    /// Link generated artifacts statically and preserve configured external modes.
    StaticGeneratedAndConfiguredExternal,
    /// Link generated artifacts statically and external artifacts dynamically.
    StaticGeneratedDynamicExternal,
    /// Link generated and configured external native artifacts statically.
    StaticAll,
}

/// C++ standard used for generated C++ and wrapper compilation.
///
/// # Example
///
/// ```
/// use souffle_rs_build::CppStandard;
///
/// assert_eq!(CppStandard::Cxx17.flag(), "-std=c++17");
/// assert_eq!(CppStandard::Cxx20.flag(), "-std=c++20");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, IntoStaticStr)]
#[serde(rename_all = "kebab-case")]
pub enum CppStandard {
    /// Compile generated C++ and wrapper sources as C++17.
    #[strum(serialize = "-std=c++17")]
    Cxx17,
    /// Compile generated C++ and wrapper sources as C++20.
    #[strum(serialize = "-std=c++20")]
    Cxx20,
}

impl CppStandard {
    /// Compiler flag for this standard.
    pub fn flag(self) -> &'static str {
        self.into()
    }
}

/// Role of a configured external native library.
///
/// The role is recorded in [`crate::BuildMetadata`] so downstream tooling can
/// distinguish a solver dependency from a Souffle addon or custom functor
/// library even when they are all rendered as native link directives.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, ExternalLibrary, ExternalLibraryKind};
///
/// let metadata = Build::new()
///     .program("analysis", "logic/main.dl")
///     .external_library(ExternalLibrary::sqlite("sqlite3"))
///     .metadata()
///     .unwrap();
///
/// assert_eq!(metadata.libraries[0].kind, ExternalLibraryKind::Sqlite);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalLibraryKind {
    /// Souffle custom functor library.
    CustomFunctor,
    /// Souffle addon library loaded by generated code.
    Addon,
    /// Generic generated-code dependency.
    Dependency,
    /// Z3 solver library dependency.
    Z3,
    /// zlib compression library dependency.
    Zlib,
    /// SQLite library dependency.
    Sqlite,
    /// C++ standard runtime library.
    CxxRuntime,
}

/// OpenMP build configuration.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{NativeLinkMode, OpenMpConfig};
///
/// let openmp = OpenMpConfig::enabled("gomp")
///     .link_mode(NativeLinkMode::Static)
///     .flag("-pthread");
///
/// let build = souffle_rs_build::Build::new()
///     .program("analysis", "logic/main.dl")
///     .openmp(openmp);
///
/// assert!(build.plan().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenMpConfig {
    enabled: bool,
    runtime: Option<String>,
    link_mode: NativeLinkMode,
    flags: Vec<String>,
}

impl OpenMpConfig {
    /// Disable OpenMP explicitly.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            runtime: None,
            link_mode: NativeLinkMode::Dynamic,
            flags: Vec::new(),
        }
    }

    /// Enable OpenMP with a named runtime library such as `gomp` or `omp`.
    pub fn enabled(runtime: impl Into<String>) -> Self {
        Self {
            enabled: true,
            runtime: Some(runtime.into()),
            link_mode: NativeLinkMode::Dynamic,
            flags: vec!["-fopenmp".to_owned()],
        }
    }

    /// Select static or dynamic OpenMP runtime linking.
    pub fn link_mode(mut self, link_mode: NativeLinkMode) -> Self {
        self.link_mode = link_mode;
        self
    }

    /// Add a compiler flag required for OpenMP.
    pub fn flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.push(flag.into());
        self
    }

    pub(crate) fn metadata(&self) -> OpenMpMetadata {
        OpenMpMetadata {
            enabled: self.enabled,
            runtime: self.runtime.clone(),
            link_mode: self.link_mode,
            flags: self.flags.clone(),
        }
    }
}

/// Custom functor or addon library configuration.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, FunctorLibrary, NativeLinkMode};
///
/// let metadata = Build::new()
///     .program("analysis", "logic/main.dl")
///     .library_dir("native/lib")
///     .functor_library(
///         FunctorLibrary::new("functors")
///             .search_path("native/functors")
///             .link_library("z3")
///             .link_mode(NativeLinkMode::Dynamic),
///     )
///     .metadata()
///     .unwrap();
///
/// assert_eq!(metadata.libraries[0].name, "functors");
/// assert_eq!(metadata.libraries[0].link_libraries, ["z3"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctorLibrary {
    name: String,
    search_paths: Vec<PathBuf>,
    link_libraries: Vec<String>,
    link_mode: NativeLinkMode,
}

impl FunctorLibrary {
    /// Configure a functor library by logical library name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            search_paths: Vec::new(),
            link_libraries: Vec::new(),
            link_mode: NativeLinkMode::Dynamic,
        }
    }

    /// Add a native library search path for Souffle `-L` and Rust linking.
    pub fn search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.search_paths.push(path.into());
        self
    }

    /// Link an additional dependent library.
    pub fn link_library(mut self, library: impl Into<String>) -> Self {
        self.link_libraries.push(library.into());
        self
    }

    /// Select static or dynamic linking for this functor library.
    pub fn link_mode(mut self, link_mode: NativeLinkMode) -> Self {
        self.link_mode = link_mode;
        self
    }

    pub(crate) fn metadata(&self) -> ExternalLibraryMetadata {
        ExternalLibraryMetadata {
            name: self.name.clone(),
            kind: ExternalLibraryKind::CustomFunctor,
            search_paths: self.search_paths.clone(),
            link_libraries: self.link_libraries.clone(),
            link_mode: self.link_mode,
        }
    }
}

/// Explicit external library linked by generated Souffle artifacts.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, ExternalLibrary, ExternalLibraryKind, NativeLinkMode};
///
/// let metadata = Build::new()
///     .program("analysis", "logic/main.dl")
///     .external_library(
///         ExternalLibrary::z3("z3")
///             .search_path("/opt/z3/lib")
///             .link_mode(NativeLinkMode::Static),
///     )
///     .metadata()
///     .unwrap();
///
/// assert_eq!(metadata.libraries[0].kind, ExternalLibraryKind::Z3);
/// assert_eq!(metadata.libraries[0].link_mode, NativeLinkMode::Static);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalLibrary {
    name: String,
    kind: ExternalLibraryKind,
    search_paths: Vec<PathBuf>,
    link_mode: NativeLinkMode,
}

impl ExternalLibrary {
    /// Configure a generic external dependency by linker library name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: ExternalLibraryKind::Dependency,
            search_paths: Vec::new(),
            link_mode: NativeLinkMode::Dynamic,
        }
    }

    /// Configure a Souffle addon library.
    pub fn addon(name: impl Into<String>) -> Self {
        Self::new(name).kind(ExternalLibraryKind::Addon)
    }

    /// Configure a Z3 dependency library.
    pub fn z3(name: impl Into<String>) -> Self {
        Self::new(name).kind(ExternalLibraryKind::Z3)
    }

    /// Configure a zlib dependency library.
    pub fn zlib(name: impl Into<String>) -> Self {
        Self::new(name).kind(ExternalLibraryKind::Zlib)
    }

    /// Configure a SQLite dependency library.
    pub fn sqlite(name: impl Into<String>) -> Self {
        Self::new(name).kind(ExternalLibraryKind::Sqlite)
    }

    /// Configure the C++ standard runtime library.
    pub fn cxx_runtime(name: impl Into<String>) -> Self {
        Self::new(name).kind(ExternalLibraryKind::CxxRuntime)
    }

    /// Set the semantic library role recorded in build metadata.
    pub fn kind(mut self, kind: ExternalLibraryKind) -> Self {
        self.kind = kind;
        self
    }

    /// Add a native library search path for Rust linking.
    pub fn search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.search_paths.push(path.into());
        self
    }

    /// Select static or dynamic linking for this library when the overall
    /// link mode preserves configured external modes.
    pub fn link_mode(mut self, link_mode: NativeLinkMode) -> Self {
        self.link_mode = link_mode;
        self
    }

    pub(crate) fn metadata(&self) -> ExternalLibraryMetadata {
        ExternalLibraryMetadata {
            name: self.name.clone(),
            kind: self.kind,
            search_paths: self.search_paths.clone(),
            link_libraries: Vec::new(),
            link_mode: self.link_mode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProgramConfig {
    name: String,
    entrypoint: PathBuf,
    generated_namespace: Option<String>,
}

/// Builder for Souffle generation, compilation, linking, and metadata.
///
/// `Build` is designed for `build.rs`: first configure deterministic inputs,
/// inspect [`Build::plan`] when a tool wants the commands without side effects,
/// or call [`Build::compile`] to generate artifacts, emit Cargo directives, and
/// write machine-readable metadata.
///
/// # Example
///
/// ```
/// use souffle_rs_build::{Build, CppStandard, GeneratedMode};
///
/// let metadata = Build::new()
///     .program("analysis", "logic/main.dl")
///     .generated_namespace("analysis")
///     .generated_mode(GeneratedMode::Directory)
///     .cpp_standard(CppStandard::Cxx20)
///     .emit_schema(true)
///     .emit_typed_api(true)
///     .out_dir("target/souffle-rs")
///     .metadata()
///     .unwrap();
///
/// assert_eq!(metadata.programs[0].program, "analysis");
/// assert_eq!(metadata.native.cpp_standard, CppStandard::Cxx20);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    programs: Vec<ProgramConfig>,
    souffle_bin: PathBuf,
    souffle_version: Option<String>,
    souffle_include: Option<PathBuf>,
    include_dirs: Vec<PathBuf>,
    library_dirs: Vec<PathBuf>,
    macros: BTreeMap<String, String>,
    generated_namespace: Option<String>,
    generated_mode: GeneratedMode,
    wrapper_source: Option<PathBuf>,
    functor_libraries: Vec<FunctorLibrary>,
    external_libraries: Vec<ExternalLibrary>,
    cpp_standard: CppStandard,
    target_triple: Option<String>,
    compiler: Option<PathBuf>,
    openmp: OpenMpConfig,
    link_mode: LinkMode,
    rpaths: Vec<PathBuf>,
    install_name: Option<String>,
    compile_native: bool,
    emit_c_header: bool,
    emit_cxx_wrapper: bool,
    emit_schema: bool,
    emit_typed_api: bool,
    emit_typed_api_module: bool,
    schema_bundles: BTreeMap<String, RelationBundle>,
    out_dir: PathBuf,
}

impl Build {
    /// Create a default build configuration.
    pub fn new() -> Self {
        Self {
            programs: Vec::new(),
            souffle_bin: PathBuf::from("souffle"),
            souffle_version: None,
            souffle_include: None,
            include_dirs: Vec::new(),
            library_dirs: Vec::new(),
            macros: BTreeMap::new(),
            generated_namespace: None,
            generated_mode: GeneratedMode::Directory,
            wrapper_source: None,
            functor_libraries: Vec::new(),
            external_libraries: Vec::new(),
            cpp_standard: CppStandard::Cxx17,
            target_triple: None,
            compiler: None,
            openmp: OpenMpConfig::disabled(),
            link_mode: LinkMode::Dynamic,
            rpaths: Vec::new(),
            install_name: None,
            compile_native: false,
            emit_c_header: false,
            emit_cxx_wrapper: false,
            emit_schema: false,
            emit_typed_api: false,
            emit_typed_api_module: false,
            schema_bundles: BTreeMap::new(),
            out_dir: PathBuf::from("target/souffle-rs"),
        }
    }

    /// Add one Souffle program entrypoint.
    pub fn program(mut self, name: impl Into<String>, entrypoint: impl Into<PathBuf>) -> Self {
        self.programs.push(ProgramConfig {
            name: name.into(),
            entrypoint: entrypoint.into(),
            generated_namespace: None,
        });
        self
    }

    /// Add one Souffle program entrypoint with its own generated namespace.
    ///
    /// The per-program namespace overrides [`Build::generated_namespace`] for
    /// this entrypoint, which allows one `build.rs` to generate multiple
    /// Souffle programs without C++ namespace collisions.
    pub fn program_with_namespace(
        mut self,
        name: impl Into<String>,
        entrypoint: impl Into<PathBuf>,
        namespace: impl Into<String>,
    ) -> Self {
        self.programs.push(ProgramConfig {
            name: name.into(),
            entrypoint: entrypoint.into(),
            generated_namespace: Some(namespace.into()),
        });
        self
    }

    /// Set the Souffle binary path.
    pub fn souffle_bin(mut self, path: impl Into<PathBuf>) -> Self {
        self.souffle_bin = path.into();
        self
    }

    /// Record the Souffle version used by build metadata.
    pub fn souffle_version(mut self, version: impl Into<String>) -> Self {
        self.souffle_version = Some(version.into());
        self
    }

    /// Add the Souffle include root used by generated C++ and wrapper code.
    pub fn souffle_include(mut self, path: impl Into<PathBuf>) -> Self {
        self.souffle_include = Some(path.into());
        self
    }

    /// Set the default generated namespace.
    pub fn generated_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.generated_namespace = Some(namespace.into());
        self
    }

    /// Select `souffle -G` directory output or `souffle -g` single-file output.
    pub fn generated_mode(mut self, mode: GeneratedMode) -> Self {
        self.generated_mode = mode;
        self
    }

    /// Define a Souffle macro.
    pub fn define(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.macros.insert(name.into(), value.into());
        self
    }

    /// Add a Souffle include directory.
    pub fn include_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.include_dirs.push(path.into());
        self
    }

    /// Add a global native library search path for Souffle `-L` and Rust linking.
    pub fn library_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.library_dirs.push(path.into());
        self
    }

    /// Set the C++ wrapper source path.
    pub fn wrapper_source(mut self, path: impl Into<PathBuf>) -> Self {
        self.wrapper_source = Some(path.into());
        self
    }

    /// Configure a custom functor or addon library.
    pub fn functor_library(mut self, library: FunctorLibrary) -> Self {
        self.functor_libraries.push(library);
        self
    }

    /// Configure an explicit external dependency such as Z3, zlib, SQLite, an
    /// addon, or the C++ runtime.
    pub fn external_library(mut self, library: ExternalLibrary) -> Self {
        self.external_libraries.push(library);
        self
    }

    /// Select the C++ standard for generated C++ and wrapper compilation.
    pub fn cpp_standard(mut self, standard: CppStandard) -> Self {
        self.cpp_standard = standard;
        self
    }

    /// Override the Cargo target triple used for platform-specific linker
    /// directives and native build metadata.
    pub fn target_triple(mut self, target: impl Into<String>) -> Self {
        self.target_triple = Some(target.into());
        self
    }

    /// Set the C++ compiler path.
    pub fn compiler(mut self, path: impl Into<PathBuf>) -> Self {
        self.compiler = Some(path.into());
        self
    }

    /// Configure OpenMP flags and runtime linking.
    pub fn openmp(mut self, openmp: OpenMpConfig) -> Self {
        self.openmp = openmp;
        self
    }

    /// Select overall link mode.
    pub fn link_mode(mut self, link_mode: LinkMode) -> Self {
        self.link_mode = link_mode;
        self
    }

    /// Add a runtime library search path.
    pub fn rpath(mut self, path: impl Into<PathBuf>) -> Self {
        self.rpaths.push(path.into());
        self
    }

    /// Set the install-name recorded in generated dynamic library links.
    pub fn install_name(mut self, name: impl Into<String>) -> Self {
        self.install_name = Some(name.into());
        self
    }

    /// Enable or disable native C++ compilation for generated and wrapper
    /// sources.
    pub fn compile_native(mut self, compile: bool) -> Self {
        self.compile_native = compile;
        self
    }

    /// Configure C ABI header emission.
    pub fn emit_c_header(mut self, emit: bool) -> Self {
        self.emit_c_header = emit;
        self
    }

    /// Configure generated C++ C ABI wrapper source emission.
    pub fn emit_cxx_wrapper(mut self, emit: bool) -> Self {
        self.emit_cxx_wrapper = emit;
        self
    }

    /// Configure schema artifact emission.
    pub fn emit_schema(mut self, emit: bool) -> Self {
        self.emit_schema = emit;
        self
    }

    /// Configure generated typed Rust API emission.
    pub fn emit_typed_api(mut self, emit: bool) -> Self {
        self.emit_typed_api = emit;
        if !emit {
            self.emit_typed_api_module = false;
        }
        self
    }

    /// Configure emission of a generated Rust module index for all typed API
    /// artifacts.
    ///
    /// The module index is emitted as `rust/mod.rs` under the output root.
    /// During [`Build::compile`], its path is also exposed through Cargo's
    /// `SOUFFLE_RS_TYPED_API_MODULE` compile-time environment variable so the
    /// runtime crate's `souffle_rs::include_generated_programs!()` macro can
    /// load it without application code naming generated files. Enabling this
    /// also enables per-program typed API emission.
    pub fn emit_typed_api_module(mut self, emit: bool) -> Self {
        self.emit_typed_api_module = emit;
        if emit {
            self.emit_typed_api = true;
        }
        self
    }

    /// Provide reliable schema metadata for schema artifacts and typed API
    /// generation.
    pub fn schema_bundle(mut self, program: impl Into<String>, schema: RelationBundle) -> Self {
        self.schema_bundles.insert(program.into(), schema);
        self
    }

    /// Override the deterministic output root.
    ///
    /// The [`Build::new`] default is `target/souffle-rs`, which is useful for
    /// standalone tooling and side-effect-free planning. Cargo `build.rs`
    /// integrations should usually prefer [`Build::out_dir_from_cargo_env`] so
    /// generated files are scoped to Cargo's per-package build output.
    pub fn out_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.out_dir = path.into();
        self
    }

    /// Use Cargo's build-script output directory for generated artifacts.
    ///
    /// This is the recommended output configuration for `build.rs`
    /// integrations. It replaces the [`Build::new`] default output root with a
    /// crate-owned location under Cargo's build-script output root. The exact
    /// generated directory layout is intentionally private; application code
    /// should use returned [`BuildMetadata`] paths and
    /// `souffle_rs::include_generated_programs!()` rather than naming generated
    /// files directly.
    pub fn out_dir_from_cargo_env(mut self) -> Result<Self, BuildError> {
        self.out_dir = cargo_build_output_root()?;
        Ok(self)
    }

    /// Validate and produce deterministic commands/directives without running
    /// external tools.
    pub fn plan(&self) -> Result<BuildPlan, BuildError> {
        self.validate()?;

        let commands = self
            .programs
            .iter()
            .map(|program| self.souffle_command(program))
            .collect::<Vec<_>>();
        let directives = self.cargo_directives()?;

        Ok(BuildPlan::new(commands, directives))
    }

    /// Build metadata matching the validated configuration.
    pub fn metadata(&self) -> Result<BuildMetadata, BuildError> {
        self.validate()?;

        let mut include_dirs = self.include_dirs.clone();
        if let Some(souffle_include) = &self.souffle_include {
            include_dirs.push(souffle_include.clone());
        }

        let mut metadata = BuildMetadata {
            metadata_path: self.metadata_path(),
            out_dir: self.out_dir.clone(),
            c_header_artifact: (self.emit_c_header || self.emit_cxx_wrapper)
                .then(|| self.c_header_artifact()),
            cxx_wrapper_artifact: self.emit_cxx_wrapper.then(|| self.cxx_wrapper_artifact()),
            typed_api_module_artifact: self
                .emit_typed_api_module
                .then(|| self.typed_api_module_artifact()),
            generated_files: Vec::new(),
            programs: self
                .programs
                .iter()
                .map(|program| self.program_metadata(program))
                .collect(),
            souffle_bin: self.souffle_bin.clone(),
            souffle_include: self.souffle_include.clone(),
            souffle_version: self.souffle_version.clone(),
            include_dirs,
            library_dirs: self.effective_library_dirs(),
            macros: self.macros.clone(),
            generated_mode: self.generated_mode,
            wrapper_source: self.wrapper_source.clone(),
            link_mode: self.link_mode,
            openmp: self.openmp.metadata(),
            libraries: self
                .functor_libraries
                .iter()
                .map(FunctorLibrary::metadata)
                .chain(
                    self.external_libraries
                        .iter()
                        .map(ExternalLibrary::metadata),
                )
                .collect(),
            native: self.native_metadata(),
            abi_version: souffle_rs_sys::SOUFFLE_RS_ABI_VERSION,
        };
        refresh_generated_file_inventory(&mut metadata);
        Ok(metadata)
    }

    /// Validate, produce a build plan, and return metadata.
    ///
    /// This executes Souffle generation commands, prepares deterministic output
    /// paths, optionally compiles generated C++ and wrapper sources, and writes
    /// machine-readable build metadata.
    pub fn compile(&self) -> Result<BuildMetadata, BuildError> {
        let plan = self.plan()?;
        let mut metadata = self.metadata()?;
        let schema_bundles = self.resolve_schema_bundles(&metadata)?;
        validate_schema_bundles(&metadata, &schema_bundles)?;
        emit_cargo_directives(&plan);
        run_souffle_generation(self, &plan, &metadata)?;
        self.refresh_generated_sources(&mut metadata)?;
        emit_requested_artifacts(&metadata, &schema_bundles)?;
        compile_native_artifacts(self, &metadata)?;
        emit_metadata(&metadata)?;
        Ok(metadata)
    }

    fn validate(&self) -> Result<(), BuildError> {
        if self.programs.is_empty() {
            return Err(BuildError::NoPrograms);
        }

        require_non_empty_path(&self.souffle_bin, "souffle_bin")?;
        require_non_empty_path(&self.out_dir, "out_dir")?;
        if let Some(path) = &self.souffle_include {
            require_non_empty_path(path, "souffle_include")?;
        }
        if let Some(path) = &self.wrapper_source {
            require_non_empty_path(path, "wrapper_source")?;
        }
        if let Some(path) = &self.compiler {
            require_non_empty_path(path, "compiler")?;
        }
        if let Some(version) = &self.souffle_version {
            require_non_empty_value(version, "souffle_version")?;
        }
        for (name, value) in &self.macros {
            require_non_empty_value(name, "macro.name")?;
            require_identifier_value(name, "macro.name")?;
            require_non_empty_value(value, "macro.value")?;
        }
        for include_dir in &self.include_dirs {
            require_non_empty_path(include_dir, "include_dir")?;
        }
        for library_dir in &self.library_dirs {
            require_non_empty_path(library_dir, "library_dir")?;
        }
        if let Some(namespace) = &self.generated_namespace {
            require_non_empty_value(namespace, "generated_namespace")?;
            require_namespace_value(namespace, "generated_namespace")?;
        }
        if let Some(install_name) = &self.install_name {
            require_non_empty_value(install_name, "install_name")?;
        }
        if let Some(target) = &self.target_triple {
            require_non_empty_value(target, "target_triple")?;
        }
        for rpath in &self.rpaths {
            require_non_empty_path(rpath, "rpath")?;
        }
        self.validate_platform_link_capabilities()?;
        if let Some(runtime) = &self.openmp.runtime {
            require_non_empty_value(runtime, "openmp.runtime")?;
        }
        for flag in &self.openmp.flags {
            require_non_empty_value(flag, "openmp.flag")?;
        }
        for library in &self.functor_libraries {
            require_non_empty_value(&library.name, "functor_library.name")?;
            for search_path in &library.search_paths {
                require_non_empty_path(search_path, "functor_library.search_path")?;
            }
            for dependency in &library.link_libraries {
                require_non_empty_value(dependency, "functor_library.link_library")?;
            }
        }
        for library in &self.external_libraries {
            require_non_empty_value(&library.name, "external_library.name")?;
            for search_path in &library.search_paths {
                require_non_empty_path(search_path, "external_library.search_path")?;
            }
        }

        let mut program_names = BTreeSet::new();
        for program in &self.programs {
            if !is_valid_identifier(&program.name) {
                return Err(BuildError::InvalidProgramName {
                    program: program.name.clone(),
                });
            }
            if !program_names.insert(program.name.clone()) {
                return Err(BuildError::DuplicateProgramName {
                    program: program.name.clone(),
                });
            }
            if let Some(namespace) = &program.generated_namespace {
                require_non_empty_value(namespace, "program.generated_namespace")?;
                require_namespace_value(namespace, "program.generated_namespace")?;
            }
            require_non_empty_path(&program.entrypoint, "entrypoint")?;
        }

        Ok(())
    }

    fn souffle_command(&self, program: &ProgramConfig) -> SouffleCommand {
        let artifact = self.generated_artifact(&program.name);
        let mut args = Vec::new();
        match self.generated_mode {
            GeneratedMode::Directory => {
                args.push(OsString::from("-G"));
                args.push(artifact.into_os_string());
            }
            GeneratedMode::SingleFile => {
                args.push(OsString::from("-g"));
                args.push(artifact.into_os_string());
            }
        }

        args.push(OsString::from("-N"));
        args.push(OsString::from(self.namespace_for(program)));

        for (name, value) in &self.macros {
            args.push(OsString::from("-M"));
            args.push(OsString::from(format!("{name}={value}")));
        }
        for include_dir in &self.include_dirs {
            args.push(OsString::from("-I"));
            args.push(include_dir.as_os_str().to_owned());
        }
        for library_dir in self.effective_library_dirs() {
            args.push(OsString::from("-L"));
            args.push(library_dir.into_os_string());
        }
        self.push_functor_library_args(&mut args);
        args.push(program.entrypoint.as_os_str().to_owned());

        SouffleCommand::new(
            program.name.clone(),
            self.souffle_bin.clone(),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            args,
        )
    }

    fn schema_command(&self, program: &ProgramConfig) -> SouffleCommand {
        let mut args = vec![OsString::from("--show=transformed-ast")];

        for (name, value) in &self.macros {
            args.push(OsString::from("-M"));
            args.push(OsString::from(format!("{name}={value}")));
        }
        for include_dir in &self.include_dirs {
            args.push(OsString::from("-I"));
            args.push(include_dir.as_os_str().to_owned());
        }
        for library_dir in self.effective_library_dirs() {
            args.push(OsString::from("-L"));
            args.push(library_dir.into_os_string());
        }
        self.push_functor_library_args(&mut args);
        args.push(program.entrypoint.as_os_str().to_owned());

        SouffleCommand::new(
            program.name.clone(),
            self.souffle_bin.clone(),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            args,
        )
    }

    fn resolve_schema_bundles(
        &self,
        metadata: &BuildMetadata,
    ) -> Result<BTreeMap<String, RelationBundle>, BuildError> {
        let mut schemas = self.schema_bundles.clone();
        for program in &self.programs {
            let Some(program_metadata) = metadata
                .programs
                .iter()
                .find(|metadata| metadata.program == program.name)
            else {
                continue;
            };
            if !self.program_needs_schema(program_metadata) || schemas.contains_key(&program.name) {
                continue;
            }
            let schema = extract_schema_bundle(&self.schema_command(program))?;
            schemas.insert(program.name.clone(), schema);
        }
        Ok(schemas)
    }

    fn program_needs_schema(&self, program: &ProgramMetadata) -> bool {
        self.emit_cxx_wrapper
            || program.schema_artifact.is_some()
            || program.typed_api_artifact.is_some()
    }

    fn cargo_directives(&self) -> Result<Vec<CargoDirective>, BuildError> {
        let mut directives = Vec::new();
        if is_watchable_tool_path(&self.souffle_bin) {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(self.souffle_bin.clone()),
            );
        }
        for program in &self.programs {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(program.entrypoint.clone()),
            );
        }
        for include_dir in &self.include_dirs {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(include_dir.clone()),
            );
        }
        if let Some(souffle_include) = &self.souffle_include {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(souffle_include.clone()),
            );
        }
        if let Some(wrapper_source) = &self.wrapper_source {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(wrapper_source.clone()),
            );
        }
        if let Some(compiler) = &self.compiler {
            if is_watchable_tool_path(compiler) {
                push_unique_directive(
                    &mut directives,
                    CargoDirective::RerunIfChanged(compiler.clone()),
                );
            }
        }

        directives.push(CargoDirective::RerunIfEnvChanged("SOUFFLE".to_owned()));
        directives.push(CargoDirective::RerunIfEnvChanged("OUT_DIR".to_owned()));
        if self.emit_typed_api_module {
            directives.push(CargoDirective::RustcEnv {
                key: "SOUFFLE_RS_TYPED_API_MODULE".to_owned(),
                value: absolute_path_string(&self.typed_api_module_artifact()),
            });
        }
        let target = self.effective_target_triple();
        for name in native_compiler_env_vars(target.as_deref()) {
            push_unique_directive(&mut directives, CargoDirective::RerunIfEnvChanged(name));
        }

        for library_dir in self.effective_library_dirs() {
            push_unique_directive(
                &mut directives,
                CargoDirective::RerunIfChanged(library_dir.clone()),
            );
            directives.push(CargoDirective::RustcLinkSearch(library_dir));
        }
        for library in &self.functor_libraries {
            push_unique_directive(
                &mut directives,
                CargoDirective::RustcLinkLib {
                    mode: self.configured_external_link_mode(library.link_mode),
                    library: library.name.clone(),
                },
            );
            for dependency in &library.link_libraries {
                push_unique_directive(
                    &mut directives,
                    CargoDirective::RustcLinkLib {
                        mode: self.dependency_link_mode(),
                        library: dependency.clone(),
                    },
                );
            }
        }
        for library in &self.external_libraries {
            push_unique_directive(
                &mut directives,
                CargoDirective::RustcLinkLib {
                    mode: self.configured_external_link_mode(library.link_mode),
                    library: library.name.clone(),
                },
            );
        }
        if let Some(runtime) = &self.openmp.runtime {
            push_unique_directive(
                &mut directives,
                CargoDirective::RustcLinkLib {
                    mode: self.configured_external_link_mode(self.openmp.link_mode),
                    library: runtime.clone(),
                },
            );
        }
        for rpath in &self.rpaths {
            directives.push(CargoDirective::RustcLinkArg(self.rpath_link_arg(rpath)?));
            directives.push(CargoDirective::RustcEnv {
                key: "SOUFFLE_RS_RPATH".to_owned(),
                value: rpath.display().to_string(),
            });
        }
        if let Some(install_name) = &self.install_name {
            directives.push(CargoDirective::RustcLinkArg(
                self.install_name_link_arg(install_name)?,
            ));
        }

        Ok(directives)
    }

    fn push_functor_library_args(&self, args: &mut Vec<OsString>) {
        for library in &self.functor_libraries {
            args.push(OsString::from(format!("-l{}", library.name)));
        }
    }

    fn configured_external_link_mode(&self, configured: NativeLinkMode) -> NativeLinkMode {
        match self.link_mode {
            LinkMode::Dynamic | LinkMode::StaticGeneratedDynamicExternal => NativeLinkMode::Dynamic,
            LinkMode::StaticGeneratedAndConfiguredExternal => configured,
            LinkMode::StaticAll => NativeLinkMode::Static,
        }
    }

    fn dependency_link_mode(&self) -> NativeLinkMode {
        match self.link_mode {
            LinkMode::StaticAll => NativeLinkMode::Static,
            LinkMode::Dynamic
            | LinkMode::StaticGeneratedAndConfiguredExternal
            | LinkMode::StaticGeneratedDynamicExternal => NativeLinkMode::Dynamic,
        }
    }

    fn validate_platform_link_capabilities(&self) -> Result<(), BuildError> {
        let target = self.effective_target_triple();
        if self.install_name.is_some() {
            self.require_linker_platform(
                target.as_deref(),
                LinkerPlatform::Darwin,
                "install_name",
            )?;
        }
        if !self.rpaths.is_empty() {
            self.require_linker_platform_any(
                target.as_deref(),
                &[LinkerPlatform::Linux, LinkerPlatform::Darwin],
                "rpath",
            )?;
        }
        Ok(())
    }

    fn rpath_link_arg(&self, path: &Path) -> Result<String, BuildError> {
        let target = self.effective_target_triple();
        self.require_linker_platform_any(
            target.as_deref(),
            &[LinkerPlatform::Linux, LinkerPlatform::Darwin],
            "rpath",
        )?;
        Ok(format!("-Wl,-rpath,{}", path.display()))
    }

    fn install_name_link_arg(&self, install_name: &str) -> Result<String, BuildError> {
        let target = self.effective_target_triple();
        self.require_linker_platform(target.as_deref(), LinkerPlatform::Darwin, "install_name")?;
        Ok(format!("-Wl,-install_name,{install_name}"))
    }

    fn require_linker_platform(
        &self,
        target: Option<&str>,
        expected: LinkerPlatform,
        capability: &'static str,
    ) -> Result<(), BuildError> {
        let platform = LinkerPlatform::from_target(target);
        if platform == Some(expected) {
            return Ok(());
        }
        Err(unsupported_platform_capability(capability, target))
    }

    fn require_linker_platform_any(
        &self,
        target: Option<&str>,
        expected: &[LinkerPlatform],
        capability: &'static str,
    ) -> Result<(), BuildError> {
        let platform = LinkerPlatform::from_target(target);
        if platform.is_some_and(|platform| expected.contains(&platform)) {
            return Ok(());
        }
        Err(unsupported_platform_capability(capability, target))
    }

    fn effective_target_triple(&self) -> Option<String> {
        self.target_triple
            .clone()
            .or_else(|| std::env::var("TARGET").ok())
    }

    fn program_metadata(&self, program: &ProgramConfig) -> ProgramMetadata {
        let schema_artifact = self.emit_schema.then(|| {
            self.out_dir
                .join("schema")
                .join(format!("{}.json", program.name))
        });
        let typed_api_artifact = self.emit_typed_api.then(|| {
            self.out_dir
                .join("rust")
                .join(format!("{}.rs", program.name))
        });

        ProgramMetadata {
            program: program.name.clone(),
            entrypoint: program.entrypoint.clone(),
            generated_namespace: self.namespace_for(program),
            generated_artifact: self.generated_artifact(&program.name),
            schema_artifact,
            typed_api_artifact,
            generated_sources: self.planned_generated_sources(&program.name),
        }
    }

    fn native_metadata(&self) -> NativeBuildMetadata {
        let mut include_dirs = self.include_dirs.clone();
        if let Some(souffle_include) = &self.souffle_include {
            include_dirs.push(souffle_include.clone());
        }
        if self.emit_cxx_wrapper {
            include_dirs.push(self.out_dir.join("include"));
        }

        let mut link_libraries = Vec::new();
        for library in &self.functor_libraries {
            push_unique(&mut link_libraries, library.name.clone());
            for dependency in &library.link_libraries {
                push_unique(&mut link_libraries, dependency.clone());
            }
        }
        for library in &self.external_libraries {
            push_unique(&mut link_libraries, library.name.clone());
        }
        if let Some(runtime) = &self.openmp.runtime {
            push_unique(&mut link_libraries, runtime.clone());
        }

        NativeBuildMetadata {
            compile_enabled: self.compile_native,
            static_library: self
                .compile_native
                .then(|| NATIVE_STATIC_LIBRARY.to_owned()),
            target_triple: self.effective_target_triple(),
            compiler: self.compiler.clone(),
            cpp_standard: self.cpp_standard,
            defines: vec!["__EMBEDDED_SOUFFLE__".to_owned()],
            compile_flags: self.compile_flags(),
            include_dirs,
            library_dirs: self.effective_library_dirs(),
            link_libraries,
            rpaths: self.rpaths.clone(),
            install_name: self.install_name.clone(),
            wrapper_sources: self.wrapper_sources(),
        }
    }

    fn compile_flags(&self) -> Vec<String> {
        let mut flags = vec![self.cpp_standard.flag().to_owned()];
        flags.extend(self.openmp.flags.iter().cloned());
        flags
    }

    fn planned_generated_sources(&self, program: &str) -> Vec<PathBuf> {
        match self.generated_mode {
            GeneratedMode::SingleFile => vec![self.generated_artifact(program)],
            GeneratedMode::Directory => Vec::new(),
        }
    }

    fn refresh_generated_sources(&self, metadata: &mut BuildMetadata) -> Result<(), BuildError> {
        for program in &mut metadata.programs {
            program.generated_sources = match self.generated_mode {
                GeneratedMode::SingleFile => vec![program.generated_artifact.clone()],
                GeneratedMode::Directory => collect_cpp_sources(&program.generated_artifact)?,
            };
        }
        metadata.native = self.native_metadata();
        refresh_generated_file_inventory(metadata);
        Ok(())
    }

    fn generated_artifact(&self, program: &str) -> PathBuf {
        match self.generated_mode {
            GeneratedMode::Directory => self.out_dir.join("generated").join(program),
            GeneratedMode::SingleFile => self
                .out_dir
                .join("generated")
                .join(format!("{program}.cpp")),
        }
    }

    pub(crate) fn out_dir_path(&self) -> &Path {
        &self.out_dir
    }

    pub(crate) fn generated_mode_value(&self) -> GeneratedMode {
        self.generated_mode
    }

    fn metadata_path(&self) -> PathBuf {
        self.out_dir.join("build-metadata.json")
    }

    fn c_header_artifact(&self) -> PathBuf {
        self.out_dir.join("include").join("souffle_rs.h")
    }

    fn cxx_wrapper_artifact(&self) -> PathBuf {
        self.out_dir.join("native").join("souffle_rs_wrapper.cpp")
    }

    fn typed_api_module_artifact(&self) -> PathBuf {
        self.out_dir.join("rust").join("mod.rs")
    }

    fn wrapper_sources(&self) -> Vec<PathBuf> {
        let mut sources = self.wrapper_source.iter().cloned().collect::<Vec<_>>();
        if self.emit_cxx_wrapper {
            let wrapper = self.cxx_wrapper_artifact();
            if !sources.contains(&wrapper) {
                sources.push(wrapper);
            }
        }
        sources
    }

    fn effective_library_dirs(&self) -> Vec<PathBuf> {
        let mut library_dirs = self.library_dirs.clone();
        for library in &self.functor_libraries {
            for search_path in &library.search_paths {
                push_unique_path(&mut library_dirs, search_path.clone());
            }
        }
        for library in &self.external_libraries {
            for search_path in &library.search_paths {
                push_unique_path(&mut library_dirs, search_path.clone());
            }
        }
        library_dirs
    }

    fn namespace_for(&self, program: &ProgramConfig) -> String {
        program
            .generated_namespace
            .clone()
            .or_else(|| self.generated_namespace.clone())
            .unwrap_or_else(|| program.name.clone())
    }
}

impl Default for Build {
    fn default() -> Self {
        Self::new()
    }
}

/// Return a path inside the current Cargo package manifest directory.
///
/// This is a small convenience for build scripts that keep Souffle sources
/// under their package directory and want to avoid spelling Cargo environment
/// variables directly.
pub fn cargo_manifest_path(path: impl AsRef<Path>) -> Result<PathBuf, BuildError> {
    Ok(cargo_env_path("CARGO_MANIFEST_DIR")?.join(path))
}

fn validate_schema_bundles(
    metadata: &BuildMetadata,
    schemas: &BTreeMap<String, RelationBundle>,
) -> Result<(), BuildError> {
    for program in &metadata.programs {
        if let Some(schema) = schemas.get(&program.program) {
            validate_schema_bundle(&program.program, schema)?;
        }
    }
    Ok(())
}

fn require_non_empty_path(path: &Path, field: &'static str) -> Result<(), BuildError> {
    if path.as_os_str().is_empty() {
        return Err(BuildError::EmptyPath { field });
    }
    Ok(())
}

fn require_non_empty_value(value: &str, field: &'static str) -> Result<(), BuildError> {
    if value.is_empty() {
        return Err(BuildError::EmptyValue { field });
    }
    Ok(())
}

fn require_identifier_value(value: &str, field: &'static str) -> Result<(), BuildError> {
    if !is_valid_identifier(value) {
        return Err(BuildError::InvalidIdentifierValue {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn require_namespace_value(value: &str, field: &'static str) -> Result<(), BuildError> {
    if value.split("::").all(is_valid_identifier) {
        return Ok(());
    }
    Err(BuildError::InvalidIdentifierValue {
        field,
        value: value.to_owned(),
    })
}

fn is_watchable_tool_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
}

fn cargo_build_output_root() -> Result<PathBuf, BuildError> {
    Ok(cargo_env_path("OUT_DIR")?.join(CARGO_OUT_DIR_SUBDIRECTORY))
}

fn cargo_env_path(variable: &'static str) -> Result<PathBuf, BuildError> {
    let value = std::env::var_os(variable).ok_or(BuildError::MissingCargoEnv { variable })?;
    if value.is_empty() {
        return Err(BuildError::EmptyValue { field: variable });
    }
    Ok(PathBuf::from(value))
}

fn absolute_path_string(path: &Path) -> String {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    path.display().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkerPlatform {
    Linux,
    Darwin,
}

impl LinkerPlatform {
    fn from_target(target: Option<&str>) -> Option<Self> {
        let target = target?;
        if target.contains("apple-darwin") {
            Some(Self::Darwin)
        } else if target.contains("linux") {
            Some(Self::Linux)
        } else {
            None
        }
    }
}

fn unsupported_platform_capability(
    capability: impl Into<String>,
    target: Option<&str>,
) -> BuildError {
    BuildError::UnsupportedPlatformCapability {
        capability: capability.into(),
        target: target.unwrap_or("<unknown>").to_owned(),
    }
}

pub(crate) fn native_compiler_env_vars(target: Option<&str>) -> Vec<String> {
    let mut names = Vec::new();
    for prefix in NATIVE_COMPILER_ENV_PREFIXES {
        push_unique(&mut names, (*prefix).to_owned());
        if let Some(target) = target {
            push_unique(&mut names, format!("{prefix}_{target}"));
            push_unique(&mut names, format!("{prefix}_{}", target.replace('-', "_")));
        }
    }
    for name in NATIVE_COMPILER_ENV_BASE {
        push_unique(&mut names, (*name).to_owned());
    }
    names
}

fn is_valid_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn refresh_generated_file_inventory(metadata: &mut BuildMetadata) {
    let mut generated_files = Vec::new();
    if let Some(header) = &metadata.c_header_artifact {
        push_unique_path(&mut generated_files, header.clone());
    }
    if let Some(wrapper) = &metadata.cxx_wrapper_artifact {
        push_unique_path(&mut generated_files, wrapper.clone());
    }
    for program in &metadata.programs {
        if metadata.generated_mode == GeneratedMode::SingleFile {
            push_unique_path(&mut generated_files, program.generated_artifact.clone());
        }
        for source in &program.generated_sources {
            push_unique_path(&mut generated_files, source.clone());
        }
        if let Some(schema) = &program.schema_artifact {
            push_unique_path(&mut generated_files, schema.clone());
        }
        if let Some(typed_api) = &program.typed_api_artifact {
            push_unique_path(&mut generated_files, typed_api.clone());
        }
    }
    if let Some(module) = &metadata.typed_api_module_artifact {
        push_unique_path(&mut generated_files, module.clone());
    }
    for source in &metadata.native.wrapper_sources {
        push_unique_path(&mut generated_files, source.clone());
    }
    metadata.generated_files = generated_files;
}

fn collect_cpp_sources(root: &Path) -> Result<Vec<PathBuf>, BuildError> {
    let mut sources = Vec::new();
    collect_cpp_sources_recursive(root, &mut sources)?;
    sources.sort();
    Ok(sources)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn push_unique_path(values: &mut Vec<PathBuf>, value: PathBuf) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn push_unique_directive(directives: &mut Vec<CargoDirective>, directive: CargoDirective) {
    if !directives.contains(&directive) {
        directives.push(directive);
    }
}

fn collect_cpp_sources_recursive(
    path: &Path,
    sources: &mut Vec<PathBuf>,
) -> Result<(), BuildError> {
    for entry in std::fs::read_dir(path).map_err(|source| BuildError::Io {
        operation: "read directory".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })? {
        let entry = entry.map_err(|source| BuildError::Io {
            operation: "read directory entry".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| BuildError::Io {
            operation: "inspect file type".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        })?;
        if file_type.is_dir() {
            collect_cpp_sources_recursive(&path, sources)?;
        } else if path.extension().is_some_and(|extension| extension == "cpp") {
            sources.push(path);
        }
    }
    Ok(())
}
