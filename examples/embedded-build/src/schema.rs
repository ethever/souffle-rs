use souffle_rs::{AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef};

pub fn reachability_schema() -> RelationBundle {
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
