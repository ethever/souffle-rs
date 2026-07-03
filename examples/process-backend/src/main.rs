//! Standalone executable example for the isolated process backend.
//!
//! The program compiles `logic/reachability.dl` with `souffle`, exchanges facts
//! and outputs through the process backend, and prints the reachable nodes. It
//! can be run with `cargo run -p souffle-rs-example-process-backend` when
//! Souffle is installed or `SOUFFLE_RS_SOUFFLE_BIN` points to a Souffle binary.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

use souffle_rs::{
    AttributeSchema, ProcessConfig, ProcessProgram, Program, RelationBundle, RelationId,
    RelationSchema, TypeRef, Value,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tempdir = tempfile::tempdir()?;
    let logic_path = logic_path();
    let executable = tempdir.path().join("reachability");
    if !compile_souffle(&logic_path, &executable)? {
        return Ok(());
    }

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("reachability")
        .schema(reachability_schema())
        .process_config(ProcessConfig::new(&executable, &work_dir))
        .build_process()?;

    program.insert_row("Seed", [Value::Symbol("a".to_owned())])?;
    program.insert_row(
        "Edge",
        [Value::Symbol("a".to_owned()), Value::Symbol("b".to_owned())],
    )?;
    program.insert_row(
        "Edge",
        [Value::Symbol("b".to_owned()), Value::Symbol("c".to_owned())],
    )?;
    program.run()?;

    let output = program.read_relation("Reachable")?;
    let mut nodes = output
        .rows()
        .iter()
        .map(|row| match row.values() {
            [Value::Symbol(node)] => node.clone(),
            values => panic!("unexpected Reachable row: {values:?}"),
        })
        .collect::<Vec<_>>();
    nodes.sort();

    assert_eq!(nodes, ["a", "b", "c"]);
    println!("reachable nodes: {}", nodes.join(", "));
    println!("fact and output files: {}", work_dir.display());
    Ok(())
}

fn logic_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logic/reachability.dl")
}

fn compile_souffle(
    logic_path: &Path,
    executable: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    let souffle = env::var_os("SOUFFLE_RS_SOUFFLE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("souffle"));
    let output = match Command::new(&souffle)
        .arg("-o")
        .arg(executable)
        .arg(logic_path)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping example: install Souffle or set SOUFFLE_RS_SOUFFLE_BIN to run it");
            return Ok(false);
        }
        Err(error) => return Err(error.into()),
    };

    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "souffle compilation failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }

    Ok(true)
}

fn reachability_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Edge",
            [
                AttributeSchema::new("source", TypeRef::Symbol),
                AttributeSchema::new("target", TypeRef::Symbol),
            ],
        ),
        RelationSchema::input(
            RelationId::new(1),
            "Seed",
            [AttributeSchema::new("node", TypeRef::Symbol)],
        ),
        RelationSchema::output(
            RelationId::new(2),
            "Reachable",
            [AttributeSchema::new("node", TypeRef::Symbol)],
        ),
    ]
    .into_iter()
    .collect()
}
