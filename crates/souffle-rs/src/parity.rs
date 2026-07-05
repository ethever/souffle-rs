use crate::{Backend, Program, RelationIterator, RelationSchema, Row, SouffleError, Value};

/// Verify schema-normalized relation output equivalence between two backends.
///
/// This compares the selected printable relations by schema and decoded row
/// stream. It does not run either backend; callers should insert inputs and
/// call [`Program::run`] before invoking parity checks.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, InMemoryProgram, Program, RelationBundle, RelationId,
///     RelationSchema, Row, TypeRef, Value, verify_backend_parity,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema: RelationBundle = [RelationSchema::output(
///     RelationId::new(0),
///     "Output",
///     [AttributeSchema::new("id", TypeRef::Number)],
/// )]
/// .into_iter()
/// .collect();
/// let mut left = InMemoryProgram::builder("left")
///     .schema(schema.clone())
///     .build_memory()?;
/// let mut right = InMemoryProgram::builder("right")
///     .schema(schema)
///     .build_memory()?;
///
/// left.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
/// right.replace_relation_rows("Output", [Row::new([Value::Number(7)])])?;
///
/// verify_backend_parity(&left, &right, ["Output"])?;
/// # Ok(())
/// # }
/// ```
pub fn verify_backend_parity<L, R, S>(
    left: &L,
    right: &R,
    relations: impl IntoIterator<Item = S>,
) -> Result<(), SouffleError>
where
    L: Program,
    R: Program,
    S: AsRef<str>,
{
    for relation in relations {
        let relation = relation.as_ref();
        let left_schema = left.relation_schema(relation)?.clone();
        let right_schema = right.relation_schema(relation)?.clone();
        verify_relation_schema_parity(
            relation,
            left.backend(),
            right.backend(),
            &left_schema,
            &right_schema,
        )?;

        let mut left_rows = left.iter_relation(relation)?;
        let mut right_rows = right.iter_relation(relation)?;
        verify_relation_iterator_parity(
            relation,
            left.backend(),
            right.backend(),
            &left_schema,
            &mut left_rows,
            &mut right_rows,
        )?;
    }

    Ok(())
}

fn verify_relation_schema_parity(
    relation: &str,
    left_backend: Backend,
    right_backend: Backend,
    left: &RelationSchema,
    right: &RelationSchema,
) -> Result<(), SouffleError> {
    if left != right {
        return parity_error(
            relation,
            left_backend,
            right_backend,
            "relation schemas differ",
        );
    }
    Ok(())
}

fn verify_relation_iterator_parity(
    relation: &str,
    left_backend: Backend,
    right_backend: Backend,
    schema: &RelationSchema,
    left: &mut RelationIterator<'_>,
    right: &mut RelationIterator<'_>,
) -> Result<(), SouffleError> {
    let mut row_index = 0;
    loop {
        match (left.next_row()?, right.next_row()?) {
            (Some(left_row), Some(right_row)) => {
                verify_row_parity(
                    relation,
                    left_backend,
                    right_backend,
                    row_index,
                    schema,
                    &left_row,
                    &right_row,
                )?;
                row_index += 1;
            }
            (None, None) => return Ok(()),
            (Some(_), None) => {
                return parity_error(
                    relation,
                    left_backend,
                    right_backend,
                    format!("row count differs: left has additional row at index {row_index}"),
                );
            }
            (None, Some(_)) => {
                return parity_error(
                    relation,
                    left_backend,
                    right_backend,
                    format!("row count differs: right has additional row at index {row_index}"),
                );
            }
        }
    }
}

fn verify_row_parity(
    relation: &str,
    left_backend: Backend,
    right_backend: Backend,
    row_index: usize,
    schema: &RelationSchema,
    left: &Row,
    right: &Row,
) -> Result<(), SouffleError> {
    if left.len() != right.len() {
        return parity_error(
            relation,
            left_backend,
            right_backend,
            format!(
                "row {row_index} arity differs: left has {}, right has {}",
                left.len(),
                right.len()
            ),
        );
    }

    for (column_index, (left_value, right_value)) in
        left.values().iter().zip(right.values()).enumerate()
    {
        let column = schema.attributes()[column_index].name();
        if let Some(message) = value_mismatch(left_value, right_value, column) {
            return parity_error(
                relation,
                left_backend,
                right_backend,
                format!("row {row_index} column `{column}` differs: {message}"),
            );
        }
    }

    Ok(())
}

fn value_mismatch(left: &Value, right: &Value, path: &str) -> Option<String> {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) if left == right => None,
        (Value::Unsigned(left), Value::Unsigned(right)) if left == right => None,
        (Value::Float(left), Value::Float(right)) if left.to_bits() == right.to_bits() => None,
        (Value::Symbol(left), Value::Symbol(right)) if left == right => None,
        (Value::Nullary, Value::Nullary) => None,
        (Value::Record(left), Value::Record(right)) => {
            values_mismatch(left, right, path, "record field")
        }
        (Value::List(left), Value::List(right)) => values_mismatch(left, right, path, "list item"),
        (
            Value::Adt {
                variant: left_variant,
                fields: left_fields,
            },
            Value::Adt {
                variant: right_variant,
                fields: right_fields,
            },
        ) if left_variant == right_variant => {
            values_mismatch(left_fields, right_fields, path, "ADT field")
        }
        (
            Value::Adt {
                variant: left_variant,
                ..
            },
            Value::Adt {
                variant: right_variant,
                ..
            },
        ) => Some(format!(
            "{path} ADT variant differs: left `{left_variant}`, right `{right_variant}`"
        )),
        (Value::Float(left), Value::Float(right)) => Some(format!(
            "{path} float bits differ: left {:016x}, right {:016x}",
            left.to_bits(),
            right.to_bits()
        )),
        (left, right) if left.kind() != right.kind() => Some(format!(
            "{path} kind differs: left {}, right {}",
            left.kind().as_str(),
            right.kind().as_str()
        )),
        (left, right) => Some(format!(
            "{path} value differs: left {left:?}, right {right:?}"
        )),
    }
}

fn values_mismatch(
    left: &[Value],
    right: &[Value],
    path: &str,
    element_name: &str,
) -> Option<String> {
    if left.len() != right.len() {
        return Some(format!(
            "{path} {element_name} count differs: left {}, right {}",
            left.len(),
            right.len()
        ));
    }

    left.iter()
        .zip(right)
        .enumerate()
        .find_map(|(index, (left, right))| value_mismatch(left, right, &format!("{path}[{index}]")))
}

fn parity_error(
    relation: &str,
    left_backend: Backend,
    right_backend: Backend,
    message: impl Into<String>,
) -> Result<(), SouffleError> {
    Err(SouffleError::BackendParityMismatch {
        relation: relation.to_owned(),
        message: format!("{left_backend:?} vs {right_backend:?}: {}", message.into()),
    })
}
