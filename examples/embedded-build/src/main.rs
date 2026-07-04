//! Standalone build-script example for an embedded Souffle program.
//!
//! The `build.rs` in this package compiles `logic/reachability.dl`, emits the
//! C ABI wrapper and typed Rust API, builds the generated C++ into a native
//! library, and makes the generated typed API available through Cargo's
//! deterministic `OUT_DIR`.

include!(concat!(
    env!("OUT_DIR"),
    "/souffle-rs/rust/reachability_mod.rs"
));

use souffle_rs::{EmbeddedProgram, Program};

mod schema;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut program = EmbeddedProgram::builder("reachability")
        .schema(schema::reachability_schema())
        .build_embedded()?;

    reachability::SeedRelation::insert(
        &mut program,
        reachability::SeedRow {
            node: "a".to_owned(),
        },
    )?;
    reachability::EdgeRelation::insert(
        &mut program,
        reachability::EdgeRow {
            source: "a".to_owned(),
            target: "b".to_owned(),
        },
    )?;
    reachability::EdgeRelation::insert(
        &mut program,
        reachability::EdgeRow {
            source: "b".to_owned(),
            target: "c".to_owned(),
        },
    )?;
    program.run()?;

    let mut nodes = reachability::ReachableRelation::read(&program)?
        .into_iter()
        .map(|row| row.node)
        .collect::<Vec<_>>();
    nodes.sort();

    assert_eq!(nodes, ["a", "b", "c"]);
    println!("embedded reachable nodes: {}", nodes.join(", "));
    Ok(())
}
