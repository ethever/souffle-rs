//! Standalone executable example for inspecting a `souffle-rs-build` plan.
//!
//! This package shows how a build script or diagnostics tool can configure
//! Souffle generation, inspect the planned command and Cargo directives without
//! running external tools, and read typed API paths from build metadata.
//! Run it with `cargo run -p souffle-rs-example-build-plan`.

use std::path::PathBuf;

use souffle_rs::{AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef};
use souffle_rs_build::{Build, GeneratedMode};

fn main() -> Result<(), souffle_rs_build::BuildError> {
    let schema = reachability_schema();
    let logic_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logic/reachability.dl");
    let build = Build::new()
        .program_with_namespace("reachability", &logic_path, "reachability_generated")
        .souffle_bin("souffle")
        .generated_mode(GeneratedMode::SingleFile)
        .out_dir("target/souffle-rs-example")
        .emit_schema(true)
        .emit_typed_api(true)
        .schema_bundle("reachability", schema);

    let plan = build.plan()?;
    let command = &plan.souffle_commands()[0];
    println!("program: {}", command.program());
    println!("souffle command: {}", command.command_line());

    for directive in plan.cargo_directives() {
        println!("{}", directive.render());
    }

    let metadata = build.metadata()?;
    let typed_api = metadata.programs[0]
        .typed_api_artifact
        .as_ref()
        .expect("typed API was requested");
    println!("typed Rust API: {}", typed_api.display());

    assert_eq!(metadata.programs[0].program, "reachability");
    Ok(())
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
