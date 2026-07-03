//! Standalone executable example for the schema-checked dynamic runtime API.
//!
//! This package demonstrates how to construct a relation schema, use stable
//! relation handles, insert rows, and stream a printable relation with the
//! in-memory backend. It is intentionally a separate workspace package so it
//! matches the common Rust examples layout while remaining runnable with
//! `cargo run -p souffle-rs-example-dynamic-api`.

use souffle_rs::{
    AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId, RelationSchema, Row,
    TypeRef, Value,
};

fn main() -> Result<(), souffle_rs::SouffleError> {
    let schema = reachability_schema();
    let mut program = InMemoryProgram::builder("reachability")
        .schema(schema)
        .build_memory();

    let edge = program.relation_handle("Edge")?;
    program.insert_row_by_handle(
        &edge,
        [Value::Symbol("a".to_owned()), Value::Symbol("b".to_owned())],
    )?;
    program.insert_row_by_handle(
        &edge,
        [Value::Symbol("b".to_owned()), Value::Symbol("c".to_owned())],
    )?;

    // The in-memory backend is a schema-checked facade that is useful for
    // tests, fixtures, export, and parity. It does not execute Souffle rules,
    // so tests can write the expected printable relation directly.
    program.replace_relation_rows(
        "Reachable",
        [
            Row::new([Value::Symbol("b".to_owned())]),
            Row::new([Value::Symbol("c".to_owned())]),
        ],
    )?;
    program.run()?;

    let mut reachable = Vec::new();
    let mut rows = program.iter_relation("Reachable")?;
    while let Some(row) = rows.next_row()? {
        let [Value::Symbol(node)] = row.values() else {
            panic!("unexpected Reachable row: {row:?}");
        };
        reachable.push(node.clone());
    }
    reachable.sort();

    assert_eq!(reachable, ["b", "c"]);
    println!("reachable nodes: {}", reachable.join(", "));
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
        RelationSchema::output(
            RelationId::new(1),
            "Reachable",
            [AttributeSchema::new("node", TypeRef::Symbol)],
        ),
    ]
    .into_iter()
    .collect()
}
