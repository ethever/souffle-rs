use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    process::Command,
    thread,
    time::Duration,
};

use crate::{
    Build, BuildError, BuildMetadata, BuildPlan, CommandFailure, GeneratedMode,
    NativeCompileFailure, SouffleCommand,
};

pub(crate) fn emit_cargo_directives(plan: &BuildPlan) {
    for directive in plan.cargo_directives() {
        println!("{}", directive.render());
    }
}

pub(crate) fn run_souffle_generation(
    build: &Build,
    plan: &BuildPlan,
    metadata: &BuildMetadata,
) -> Result<(), BuildError> {
    prepare_output_roots(build, metadata)?;

    for (command, program) in plan.souffle_commands().iter().zip(&metadata.programs) {
        prepare_generated_artifact(
            program.generated_artifact.as_path(),
            build.generated_mode_value(),
        )?;
        run_command(command)?;
    }

    Ok(())
}

pub(crate) fn emit_metadata(metadata: &BuildMetadata) -> Result<(), BuildError> {
    if let Some(parent) = metadata.metadata_path.parent() {
        create_dir_all(parent)?;
    }
    let json = metadata.to_json_pretty()?;
    let mut file = create_file(metadata.metadata_path.as_path())?;
    file.write_all(json.as_bytes())
        .map_err(|source| io_error("write", metadata.metadata_path.as_path(), source))?;
    file.write_all(b"\n")
        .map_err(|source| io_error("write", metadata.metadata_path.as_path(), source))?;
    Ok(())
}

pub(crate) fn compile_native_artifacts(
    build: &Build,
    metadata: &BuildMetadata,
) -> Result<(), BuildError> {
    if !metadata.native.compile_enabled {
        return Ok(());
    }

    let library = metadata
        .native
        .static_library
        .clone()
        .unwrap_or_else(|| "souffle_rs_generated".to_owned());
    let sources = native_sources(metadata);
    if sources.is_empty() {
        return Err(BuildError::NativeSourcesUnavailable { library });
    }

    let native_out_dir = build.out_dir_path().join("native");
    create_dir_all(native_out_dir.as_path())?;

    let mut compiler = cc::Build::new();
    compiler.cpp(true);
    compiler.out_dir(native_out_dir);

    if let Some(path) = &metadata.native.compiler {
        compiler.compiler(path);
    }
    for define in &metadata.native.defines {
        compiler.define(define, None);
    }
    for include_dir in &metadata.native.include_dirs {
        compiler.include(include_dir);
    }
    for flag in &metadata.native.compile_flags {
        compiler.flag(flag);
    }
    for source in &sources {
        compiler.file(source);
    }

    compiler
        .try_compile(library.as_str())
        .map_err(|source| native_compile_error(metadata, library, sources, source))
}

fn prepare_output_roots(build: &Build, metadata: &BuildMetadata) -> Result<(), BuildError> {
    create_dir_all(build.out_dir_path())?;
    create_dir_all(build.out_dir_path().join("generated").as_path())?;

    if metadata
        .programs
        .iter()
        .any(|program| program.schema_artifact.is_some())
    {
        create_dir_all(build.out_dir_path().join("schema").as_path())?;
    }
    if metadata
        .programs
        .iter()
        .any(|program| program.typed_api_artifact.is_some())
    {
        create_dir_all(build.out_dir_path().join("rust").as_path())?;
    }

    Ok(())
}

fn prepare_generated_artifact(path: &Path, mode: GeneratedMode) -> Result<(), BuildError> {
    match mode {
        GeneratedMode::Directory => {
            remove_path_if_exists(path)?;
            create_dir_all(path)
        }
        GeneratedMode::SingleFile => {
            remove_path_if_exists(path)?;
            if let Some(parent) = path.parent() {
                create_dir_all(parent)?;
            }
            Ok(())
        }
    }
}

fn run_command(command: &SouffleCommand) -> Result<(), BuildError> {
    let output = run_command_with_busy_retry(command)?;

    if !output.status.success() {
        return Err(BuildError::CommandFailed(Box::new(CommandFailure {
            program: command.program().to_owned(),
            command: command.command_line(),
            working_dir: command.working_dir().display().to_string(),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })));
    }

    Ok(())
}

fn run_command_with_busy_retry(
    command: &SouffleCommand,
) -> Result<std::process::Output, BuildError> {
    let mut attempts = 0;
    loop {
        match Command::new(command.executable())
            .args(command.args())
            .current_dir(command.working_dir())
            .output()
        {
            Ok(output) => return Ok(output),
            Err(source) if is_executable_busy(&source) && attempts < 5 => {
                attempts += 1;
                thread::sleep(Duration::from_millis(10));
            }
            Err(source) => {
                return Err(BuildError::CommandSpawnFailed {
                    command: command.command_line(),
                    working_dir: command.working_dir().display().to_string(),
                    message: source.to_string(),
                });
            }
        }
    }
}

fn is_executable_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}

fn remove_path_if_exists(path: &Path) -> Result<(), BuildError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            fs::remove_dir_all(path).map_err(|source| io_error("remove directory", path, source))
        }
        Ok(_) => fs::remove_file(path).map_err(|source| io_error("remove file", path, source)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect", path, source)),
    }
}

fn create_dir_all(path: &Path) -> Result<(), BuildError> {
    fs::create_dir_all(path).map_err(|source| io_error("create directory", path, source))
}

fn create_file(path: &Path) -> Result<File, BuildError> {
    File::create(path).map_err(|source| io_error("create file", path, source))
}

fn io_error(operation: &str, path: &Path, source: std::io::Error) -> BuildError {
    BuildError::Io {
        operation: operation.to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    }
}

fn native_sources(metadata: &BuildMetadata) -> Vec<std::path::PathBuf> {
    let mut sources = Vec::new();
    for program in &metadata.programs {
        for source in &program.generated_sources {
            push_unique_path(&mut sources, source.clone());
        }
    }
    for source in &metadata.native.wrapper_sources {
        push_unique_path(&mut sources, source.clone());
    }
    sources
}

fn native_compile_error(
    metadata: &BuildMetadata,
    library: String,
    sources: Vec<std::path::PathBuf>,
    source: cc::Error,
) -> BuildError {
    BuildError::NativeCompileFailed(Box::new(NativeCompileFailure {
        library,
        compiler: metadata
            .native
            .compiler
            .as_ref()
            .map(|path| path.display().to_string()),
        working_dir: std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_owned()),
        sources: display_paths(&sources),
        include_dirs: display_paths(&metadata.native.include_dirs),
        library_dirs: display_paths(&metadata.native.library_dirs),
        flags: metadata.native.compile_flags.clone(),
        message: source.to_string(),
    }))
}

fn push_unique_path(values: &mut Vec<std::path::PathBuf>, value: std::path::PathBuf) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn display_paths(paths: &[std::path::PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}
