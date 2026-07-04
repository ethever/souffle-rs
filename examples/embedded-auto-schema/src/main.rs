//! Standalone build-script example for an embedded Souffle program with
//! automatically extracted schema metadata.
//!
//! The `build.rs` in this package does not pass a hand-written
//! `RelationBundle`. It extracts schema from Souffle metadata, emits schema JSON
//! and typed Rust API, builds the generated C++ into a native library, and makes
//! the generated typed API available through
//! `souffle_rs::include_generated_programs!()`.

mod generated {
    souffle_rs::include_generated_programs!();
}

use souffle_rs::{EmbeddedProgram, Program};

use generated::reachability;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut program = EmbeddedProgram::builder(reachability::PROGRAM_NAME)
        .schema(reachability::schema_bundle()?)
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
            payload: reachability::EdgePayload {
                field_0: "a".to_owned(),
                field_1: "b".to_owned(),
            },
        },
    )?;
    reachability::EdgeRelation::insert(
        &mut program,
        reachability::EdgeRow {
            payload: reachability::EdgePayload {
                field_0: "b".to_owned(),
                field_1: "c".to_owned(),
            },
        },
    )?;
    program.run()?;

    let mut rows = reachability::ReachableRelation::read(&program)?
        .into_iter()
        .map(|row| {
            let marker = match row.marker {
                reachability::ReachableMarker::Derived => "derived",
                reachability::ReachableMarker::Seed => "seed",
            };
            (row.node, marker)
        })
        .collect::<Vec<_>>();
    rows.sort();

    assert_eq!(
        rows,
        [
            ("a".to_owned(), "seed"),
            ("b".to_owned(), "derived"),
            ("c".to_owned(), "derived"),
        ]
    );
    println!(
        "embedded auto-schema reachable nodes: {}",
        rows.into_iter()
            .map(|(node, marker)| format!("{node}:{marker}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}
