//! Standalone executable example for build-time schema extraction.
//!
//! This package intentionally does not pass a hand-written `RelationBundle`.
//! Run it with `cargo run -p souffle-rs-example-auto-schema` when supported
//! Souffle is installed or `SOUFFLE_RS_SOUFFLE_BIN` points to a supported
//! Souffle binary.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use souffle_rs::{RelationBundle, TypeRef};
use souffle_rs_build::{Build, GeneratedMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let souffle_bin = find_souffle_bin();
    if !souffle_available(&souffle_bin)? {
        return Ok(());
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let logic_path = manifest_dir.join("logic/reachability.dl");
    let out_dir = manifest_dir.join("../../target/souffle-rs-example-auto-schema");

    let metadata = Build::new()
        .program("reachability", &logic_path)
        .souffle_bin(&souffle_bin)
        .generated_namespace("reachability_auto_schema")
        .generated_mode(GeneratedMode::SingleFile)
        .out_dir(&out_dir)
        .emit_schema(true)
        .emit_typed_api(true)
        .compile()?;

    let program = &metadata.programs[0];
    let schema_path = required_artifact(program.schema_artifact.as_deref(), "schema")?;
    let typed_api_path = required_artifact(program.typed_api_artifact.as_deref(), "typed API")?;

    let schema_json = fs::read_to_string(schema_path)?;
    let schema: RelationBundle = serde_json::from_str(&schema_json)?;
    schema.validate()?;

    let edge = schema
        .get("Edge")
        .ok_or_else(|| io::Error::other("extracted schema is missing Edge"))?;
    let reachable = schema
        .get("Reachable")
        .ok_or_else(|| io::Error::other("extracted schema is missing Reachable"))?;

    assert!(edge.is_loadable());
    assert!(reachable.is_printable());
    assert!(matches!(
        edge.attributes()[0].declared_type(),
        TypeRef::Record(fields) if fields == &[TypeRef::Symbol, TypeRef::Symbol]
    ));
    assert!(reachable.attributes()[1].declared_type().is_enum_adt());

    let typed_api = fs::read_to_string(typed_api_path)?;
    assert!(typed_api.contains("pub struct ReachableRow"));
    assert!(typed_api.contains("pub enum ReachableMarker"));

    println!("schema extracted from: {}", logic_path.display());
    println!("schema artifact: {}", schema_path.display());
    println!("typed API artifact: {}", typed_api_path.display());
    for relation in schema.iter() {
        let columns = relation
            .attributes()
            .iter()
            .map(|attribute| {
                format!(
                    "{}:{}",
                    attribute.name(),
                    attribute.declared_type().display_name()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("{} {:?} ({columns})", relation.name(), relation.kind());
    }

    Ok(())
}

fn required_artifact<'a>(
    artifact: Option<&'a Path>,
    kind: &str,
) -> Result<&'a Path, Box<dyn std::error::Error>> {
    artifact.ok_or_else(|| io::Error::other(format!("{kind} artifact was not emitted")).into())
}

fn find_souffle_bin() -> PathBuf {
    env::var_os("SOUFFLE_RS_SOUFFLE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("souffle"))
}

fn souffle_available(souffle_bin: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    match Command::new(souffle_bin).arg("--version").output() {
        Ok(output) if output.status.success() => Ok(true),
        Ok(output) => Err(io::Error::other(format!(
            "Souffle version check failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!(
                "skipping example: install supported Souffle or set SOUFFLE_RS_SOUFFLE_BIN to run it"
            );
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}
