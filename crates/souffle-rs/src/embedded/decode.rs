use std::{collections::BTreeMap, slice};

use souffle_rs_sys::{
    SouffleRsCompositeRef, SouffleRsError, SouffleRsRelationOutput, SouffleRsRow,
    SouffleRsRowOutput, SouffleRsString, SouffleRsValue, SouffleRsValueData, SouffleRsValueKind,
};

use crate::{AbiError, RelationSchema, Row, SouffleError, TypeRef, Value, ValueKind, ffi};

/// Decode wrapper-owned materialized output into Rust-owned rows, then free it.
pub(crate) fn decode_relation_output(
    schema: &RelationSchema,
    raw: SouffleRsRelationOutput,
) -> Result<Vec<Row>, SouffleError> {
    let output = MaterializedOutput::new(raw);
    decode_rows(schema, output.raw())
}

/// Decode wrapper-owned single-row output into a Rust-owned row, then free it.
pub(crate) fn decode_row_output(
    schema: &RelationSchema,
    raw: SouffleRsRowOutput,
) -> Result<Row, SouffleError> {
    if raw.owner.is_null() {
        return Err(AbiError::NullPointer {
            argument: "SouffleRsRowOutput.owner".to_owned(),
        }
        .into());
    }

    let output = MaterializedRowOutput::new(raw);
    let relation_view = output.relation_view();
    let definitions = named_type_definitions(schema);
    decode_output_row(schema, &relation_view, output.raw_row(), &definitions)
}

struct MaterializedOutput {
    raw: SouffleRsRelationOutput,
}

impl MaterializedOutput {
    fn new(raw: SouffleRsRelationOutput) -> Self {
        Self { raw }
    }

    fn raw(&self) -> &SouffleRsRelationOutput {
        &self.raw
    }
}

impl Drop for MaterializedOutput {
    fn drop(&mut self) {
        // SAFETY: `raw` is initialized by `souffle_rs_program_read_relation`
        // and is freed exactly once by this owner.
        unsafe {
            souffle_rs_sys::souffle_rs_relation_output_free(
                &mut self.raw as *mut SouffleRsRelationOutput,
            );
        }
    }
}

struct MaterializedRowOutput {
    raw: SouffleRsRowOutput,
}

#[derive(Clone, Copy)]
struct DecodeContext<'a> {
    relation: &'a str,
    column: &'a str,
    output: &'a SouffleRsRelationOutput,
    definitions: &'a BTreeMap<String, TypeRef>,
}

impl MaterializedRowOutput {
    fn new(raw: SouffleRsRowOutput) -> Self {
        Self { raw }
    }

    fn raw_row(&self) -> &SouffleRsRow {
        &self.raw.row
    }

    fn relation_view(&self) -> SouffleRsRelationOutput {
        SouffleRsRelationOutput {
            relation_name: self.raw.row.relation_name,
            rows: &self.raw.row as *const SouffleRsRow,
            len: 1,
            owner: self.raw.owner,
        }
    }
}

impl Drop for MaterializedRowOutput {
    fn drop(&mut self) {
        // SAFETY: `raw` is initialized by `souffle_rs_relation_iterator_next`
        // and is freed exactly once by this owner.
        unsafe {
            souffle_rs_sys::souffle_rs_row_output_free(&mut self.raw as *mut SouffleRsRowOutput);
        }
    }
}

fn decode_rows(
    schema: &RelationSchema,
    output: &SouffleRsRelationOutput,
) -> Result<Vec<Row>, SouffleError> {
    let definitions = named_type_definitions(schema);
    let raw_rows = if output.len == 0 {
        &[]
    } else if output.rows.is_null() {
        return Err(AbiError::NullPointer {
            argument: "SouffleRsRelationOutput.rows".to_owned(),
        }
        .into());
    } else {
        // SAFETY: The wrapper pairs `rows` with `len` and owns the buffer for
        // the lifetime of the materialized relation output.
        unsafe { slice::from_raw_parts(output.rows, output.len) }
    };

    raw_rows
        .iter()
        .map(|raw_row| decode_output_row(schema, output, raw_row, &definitions))
        .collect()
}

fn decode_output_row(
    schema: &RelationSchema,
    output: &SouffleRsRelationOutput,
    raw_row: &SouffleRsRow,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<Row, SouffleError> {
    if raw_row.len != schema.arity() {
        return Err(SouffleError::ArityMismatch {
            relation: schema.name().to_owned(),
            expected: schema.arity(),
            actual: raw_row.len,
        });
    }

    let raw_values = if raw_row.len == 0 {
        &[]
    } else if raw_row.values.is_null() {
        return Err(AbiError::NullPointer {
            argument: "SouffleRsRow.values".to_owned(),
        }
        .into());
    } else {
        // SAFETY: The wrapper pairs `values` with `len`; the row borrows
        // wrapper-owned output storage for the lifetime of `output`.
        unsafe { slice::from_raw_parts(raw_row.values, raw_row.len) }
    };

    let values = schema
        .attributes()
        .iter()
        .zip(raw_values)
        .map(|(attribute, raw_value)| {
            decode_output_value(
                schema.name(),
                attribute.name(),
                attribute.declared_type(),
                output,
                raw_value,
                definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Row::new(values))
}

fn decode_output_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    output: &SouffleRsRelationOutput,
    raw_value: &SouffleRsValue,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<Value, SouffleError> {
    match declared_type {
        TypeRef::Subtype { name, base } => {
            decode_output_value(relation, column, base, output, raw_value, definitions)
                .map(|value| Value::typed(name.clone(), value.into_untyped()))
        }
        TypeRef::Declared { name, runtime } => {
            decode_output_value(relation, column, runtime, output, raw_value, definitions)
                .map(|value| Value::typed(name.clone(), value.into_untyped()))
        }
        TypeRef::Reference { name, .. } => {
            let Some(resolved) = definitions.get(name) else {
                return decode_scalar_value(relation, column, declared_type, raw_value);
            };
            decode_output_value(relation, column, resolved, output, raw_value, definitions)
        }
        TypeRef::Union { name, variants } => decode_union_value(
            relation,
            column,
            declared_type,
            variants,
            output,
            raw_value,
            definitions,
        )
        .map(|value| Value::typed(name.clone(), value.into_untyped())),
        TypeRef::Record(fields) => decode_composite_fields(
            DecodeContext {
                relation,
                column,
                output,
                definitions,
            },
            declared_type,
            fields,
            raw_value,
            Value::Record,
        ),
        TypeRef::List(element) => {
            let composite = composite_ref(relation, column, SouffleRsValueKind::List, raw_value)?;
            let len = composite_len(relation, column, output, composite)?;
            let values = (0..len)
                .map(|index| {
                    let raw_field = composite_value(relation, column, output, composite, index)?;
                    decode_output_value(relation, column, element, output, &raw_field, definitions)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::List(values))
        }
        TypeRef::Adt { variants, .. } => {
            let composite = composite_ref(relation, column, SouffleRsValueKind::Adt, raw_value)?;
            let variant = adt_variant(relation, column, output, composite)?;
            let Some(fields) = variants.get(&variant) else {
                return Err(decode_error(
                    relation,
                    column,
                    format!("ADT variant `{variant}` is not in schema"),
                ));
            };
            decode_composite_fields(
                DecodeContext {
                    relation,
                    column,
                    output,
                    definitions,
                },
                declared_type,
                fields,
                raw_value,
                |fields| Value::Adt {
                    variant: variant.clone(),
                    fields,
                },
            )
        }
        TypeRef::Number
        | TypeRef::Unsigned
        | TypeRef::Float
        | TypeRef::Symbol
        | TypeRef::Nullary => decode_scalar_value(relation, column, declared_type, raw_value),
    }
}

fn decode_union_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    variants: &[TypeRef],
    output: &SouffleRsRelationOutput,
    raw_value: &SouffleRsValue,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<Value, SouffleError> {
    let mut last_error = None;
    for variant in variants {
        if abi_kind_matches_type(raw_value.kind, variant, definitions) {
            match decode_output_value(relation, column, variant, output, raw_value, definitions) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = Some(error),
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        decode_error(
            relation,
            column,
            format!(
                "value kind `{}` did not match union `{}`",
                abi_kind_name(raw_value.kind),
                declared_type.display_name()
            ),
        )
    }))
}

fn decode_composite_fields(
    context: DecodeContext<'_>,
    declared_type: &TypeRef,
    fields: &[TypeRef],
    raw_value: &SouffleRsValue,
    build: impl FnOnce(Vec<Value>) -> Value,
) -> Result<Value, SouffleError> {
    let composite = composite_ref(
        context.relation,
        context.column,
        expected_composite_kind(declared_type, context.definitions),
        raw_value,
    )?;
    let len = composite_len(context.relation, context.column, context.output, composite)?;
    if len != fields.len() {
        return Err(decode_error(
            context.relation,
            context.column,
            format!(
                "composite `{}` has {len} fields but schema expects {}",
                declared_type.display_name(),
                fields.len()
            ),
        ));
    }

    let values = fields
        .iter()
        .enumerate()
        .map(|(index, field_type)| {
            let raw_field = composite_value(
                context.relation,
                context.column,
                context.output,
                composite,
                index,
            )?;
            decode_output_value(
                context.relation,
                context.column,
                field_type,
                context.output,
                &raw_field,
                context.definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(build(values))
}

pub(crate) fn decode_scalar_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    raw_value: &SouffleRsValue,
) -> Result<Value, SouffleError> {
    Ok(match declared_type {
        TypeRef::Number if raw_value.kind == SouffleRsValueKind::Number => {
            // SAFETY: The ABI kind selects the active union field.
            Value::Number(unsafe { raw_value.as_.number })
        }
        TypeRef::Unsigned if raw_value.kind == SouffleRsValueKind::Unsigned => {
            // SAFETY: The ABI kind selects the active union field.
            Value::Unsigned(unsafe { raw_value.as_.unsigned_value })
        }
        TypeRef::Float if raw_value.kind == SouffleRsValueKind::Float => {
            // SAFETY: The ABI kind selects the active union field.
            Value::Float(unsafe { raw_value.as_.float_value })
        }
        TypeRef::Symbol if raw_value.kind == SouffleRsValueKind::Symbol => {
            // SAFETY: The ABI kind selects the active union field.
            let symbol = unsafe { raw_value.as_.symbol };
            Value::Symbol(ffi::decode_abi_string(symbol, "SouffleRsValue.symbol")?)
        }
        TypeRef::Nullary if raw_value.kind == SouffleRsValueKind::Nullary => Value::Nullary,
        TypeRef::Subtype { name, base } => {
            return decode_scalar_value(relation, column, base, raw_value)
                .map(|value| Value::typed(name.clone(), value.into_untyped()));
        }
        TypeRef::Declared { name, runtime } => {
            return decode_scalar_value(relation, column, runtime, raw_value)
                .map(|value| Value::typed(name.clone(), value.into_untyped()));
        }
        _ => {
            return Err(decode_error(
                relation,
                column,
                format!(
                    "expected `{}` but ABI value kind was `{}`",
                    declared_type.display_name(),
                    abi_kind_name(raw_value.kind)
                ),
            ));
        }
    })
}

fn composite_ref(
    relation: &str,
    column: &str,
    expected: SouffleRsValueKind,
    raw_value: &SouffleRsValue,
) -> Result<SouffleRsCompositeRef, SouffleError> {
    if raw_value.kind != expected {
        return Err(decode_error(
            relation,
            column,
            format!(
                "expected `{}` but ABI value kind was `{}`",
                abi_kind_name(expected),
                abi_kind_name(raw_value.kind)
            ),
        ));
    }

    // SAFETY: The ABI kind selects the active union field.
    Ok(unsafe { raw_value.as_.composite })
}

fn composite_len(
    relation: &str,
    column: &str,
    output: &SouffleRsRelationOutput,
    composite: SouffleRsCompositeRef,
) -> Result<usize, SouffleError> {
    let mut len = 0;
    let mut error = ffi::empty_error();
    // SAFETY: `output` owns the wrapper storage containing `composite`, and
    // `len` is a valid out pointer for the duration of the call.
    let status = unsafe {
        souffle_rs_sys::souffle_rs_relation_output_composite_len(
            output as *const SouffleRsRelationOutput,
            composite,
            &mut len as *mut usize,
            &mut error as *mut SouffleRsError,
        )
    };
    ffi::check_owned_status(
        "souffle_rs_relation_output_composite_len",
        status,
        &mut error,
    )
    .map_err(|error| wrap_decode_context(relation, column, error))?;
    Ok(len)
}

fn composite_value(
    relation: &str,
    column: &str,
    output: &SouffleRsRelationOutput,
    composite: SouffleRsCompositeRef,
    index: usize,
) -> Result<SouffleRsValue, SouffleError> {
    let mut value = SouffleRsValue {
        kind: SouffleRsValueKind::Nullary,
        as_: SouffleRsValueData { unsigned_value: 0 },
    };
    let mut error = ffi::empty_error();
    // SAFETY: `output` owns the wrapper storage containing `composite`, and
    // `value` is a valid out pointer for the duration of the call.
    let status = unsafe {
        souffle_rs_sys::souffle_rs_relation_output_composite_value(
            output as *const SouffleRsRelationOutput,
            composite,
            index,
            &mut value as *mut SouffleRsValue,
            &mut error as *mut SouffleRsError,
        )
    };
    ffi::check_owned_status(
        "souffle_rs_relation_output_composite_value",
        status,
        &mut error,
    )
    .map_err(|error| wrap_decode_context(relation, column, error))?;
    Ok(value)
}

fn adt_variant(
    relation: &str,
    column: &str,
    output: &SouffleRsRelationOutput,
    composite: SouffleRsCompositeRef,
) -> Result<String, SouffleError> {
    let mut variant = SouffleRsString::null();
    let mut error = ffi::empty_error();
    // SAFETY: `output` owns the wrapper storage containing `composite`, and
    // `variant` is a valid out pointer for the duration of the call.
    let status = unsafe {
        souffle_rs_sys::souffle_rs_relation_output_adt_variant(
            output as *const SouffleRsRelationOutput,
            composite,
            &mut variant as *mut SouffleRsString,
            &mut error as *mut SouffleRsError,
        )
    };
    ffi::check_owned_status("souffle_rs_relation_output_adt_variant", status, &mut error)
        .map_err(|error| wrap_decode_context(relation, column, error))?;
    ffi::decode_abi_string(variant, "SouffleRsValue.adt_variant")
}

fn expected_composite_kind(
    declared_type: &TypeRef,
    definitions: &BTreeMap<String, TypeRef>,
) -> SouffleRsValueKind {
    match declared_type {
        TypeRef::Record(_) => SouffleRsValueKind::Record,
        TypeRef::List(_) => SouffleRsValueKind::List,
        TypeRef::Adt { .. } => SouffleRsValueKind::Adt,
        TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. } => {
            expected_composite_kind(base, definitions)
        }
        TypeRef::Reference { name, runtime } => definitions
            .get(name)
            .map(|resolved| expected_composite_kind(resolved, definitions))
            .unwrap_or_else(|| value_kind_to_abi_kind(*runtime)),
        TypeRef::Union { variants, .. } => variants
            .iter()
            .find_map(
                |variant| match expected_composite_kind(variant, definitions) {
                    kind @ (SouffleRsValueKind::Record
                    | SouffleRsValueKind::List
                    | SouffleRsValueKind::Adt) => Some(kind),
                    _ => None,
                },
            )
            .unwrap_or(SouffleRsValueKind::Nullary),
        TypeRef::Number => SouffleRsValueKind::Number,
        TypeRef::Unsigned => SouffleRsValueKind::Unsigned,
        TypeRef::Float => SouffleRsValueKind::Float,
        TypeRef::Symbol => SouffleRsValueKind::Symbol,
        TypeRef::Nullary => SouffleRsValueKind::Nullary,
    }
}

fn value_kind_to_abi_kind(kind: ValueKind) -> SouffleRsValueKind {
    match kind {
        ValueKind::Number => SouffleRsValueKind::Number,
        ValueKind::Unsigned => SouffleRsValueKind::Unsigned,
        ValueKind::Float => SouffleRsValueKind::Float,
        ValueKind::Symbol => SouffleRsValueKind::Symbol,
        ValueKind::Record => SouffleRsValueKind::Record,
        ValueKind::List => SouffleRsValueKind::List,
        ValueKind::Adt => SouffleRsValueKind::Adt,
        ValueKind::Nullary => SouffleRsValueKind::Nullary,
    }
}

fn abi_kind_matches_type(
    kind: SouffleRsValueKind,
    declared_type: &TypeRef,
    definitions: &BTreeMap<String, TypeRef>,
) -> bool {
    match declared_type {
        TypeRef::Number => kind == SouffleRsValueKind::Number,
        TypeRef::Unsigned => kind == SouffleRsValueKind::Unsigned,
        TypeRef::Float => kind == SouffleRsValueKind::Float,
        TypeRef::Symbol => kind == SouffleRsValueKind::Symbol,
        TypeRef::Nullary => kind == SouffleRsValueKind::Nullary,
        TypeRef::Record(_) => kind == SouffleRsValueKind::Record,
        TypeRef::List(_) => kind == SouffleRsValueKind::List,
        TypeRef::Adt { .. } => kind == SouffleRsValueKind::Adt,
        TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. } => {
            abi_kind_matches_type(kind, base, definitions)
        }
        TypeRef::Reference { name, runtime } => definitions
            .get(name)
            .map(|resolved| abi_kind_matches_type(kind, resolved, definitions))
            .unwrap_or_else(|| kind == value_kind_to_abi_kind(*runtime)),
        TypeRef::Union { variants, .. } => variants
            .iter()
            .any(|variant| abi_kind_matches_type(kind, variant, definitions)),
    }
}

fn named_type_definitions(schema: &RelationSchema) -> BTreeMap<String, TypeRef> {
    let mut definitions = BTreeMap::new();
    for attribute in schema.attributes() {
        attribute
            .declared_type()
            .collect_named_type_definitions(&mut definitions);
    }
    definitions
}

fn abi_kind_name(kind: SouffleRsValueKind) -> &'static str {
    kind.into()
}

fn decode_error(relation: &str, column: &str, message: String) -> SouffleError {
    SouffleError::DecodeFailed {
        relation: relation.to_owned(),
        column: column.to_owned(),
        message,
    }
}

fn wrap_decode_context(relation: &str, column: &str, error: SouffleError) -> SouffleError {
    SouffleError::DecodeFailed {
        relation: relation.to_owned(),
        column: column.to_owned(),
        message: error.to_string(),
    }
}
