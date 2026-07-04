use std::{collections::BTreeMap, ffi::CString};

use souffle_rs_sys::{
    SouffleRsCompositeRef, SouffleRsInputComposite, SouffleRsRow, SouffleRsString, SouffleRsValue,
    SouffleRsValueData, SouffleRsValueKind,
};

use crate::{
    RelationSchema, Row, SouffleError, TypeRef, Value, ffi, program::validate_row,
    schema::TypeCheck,
};

use super::relation_cstring;

/// Rust-owned buffers for a borrowed `SouffleRsRow` passed to the wrapper.
pub(crate) struct EncodedRow {
    relation_name: CString,
    declared_types: Vec<CString>,
    symbols: Vec<CString>,
    variants: Vec<CString>,
    values: Vec<SouffleRsValue>,
    composite_values: Vec<Vec<SouffleRsValue>>,
    composites: Vec<SouffleRsInputComposite>,
}

impl EncodedRow {
    pub(crate) fn as_ffi(&self) -> SouffleRsRow {
        SouffleRsRow {
            relation_name: self.relation_name.as_ptr(),
            values: if self.values.is_empty() {
                std::ptr::null()
            } else {
                self.values.as_ptr()
            },
            len: self.values.len(),
            composites: if self.composites.is_empty() {
                std::ptr::null()
            } else {
                self.composites.as_ptr()
            },
            composite_count: self.composites.len(),
        }
    }

    #[cfg(test)]
    pub(crate) fn values(&self) -> &[SouffleRsValue] {
        &self.values
    }

    #[cfg(test)]
    pub(crate) fn composites(&self) -> &[SouffleRsInputComposite] {
        &self.composites
    }
}

pub(crate) fn encode_input_row(
    schema: &RelationSchema,
    row: &Row,
) -> Result<EncodedRow, SouffleError> {
    validate_row(schema, row)?;

    let mut encoded = EncodedRow {
        relation_name: relation_cstring(schema.name())?,
        declared_types: Vec::new(),
        symbols: Vec::new(),
        variants: Vec::new(),
        values: Vec::with_capacity(row.len()),
        composite_values: Vec::new(),
        composites: Vec::new(),
    };
    let definitions = named_type_definitions(schema);

    for (attribute, value) in schema.attributes().iter().zip(row.values()) {
        let encoded_value = encode_input_value(
            schema.name(),
            attribute.name(),
            attribute.declared_type(),
            value,
            &mut encoded,
            &definitions,
        )?;
        encoded.values.push(encoded_value);
    }

    Ok(encoded)
}

fn encode_input_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    value: &Value,
    encoded: &mut EncodedRow,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<SouffleRsValue, SouffleError> {
    let declared_type = resolve_type_ref(declared_type, definitions);
    if let TypeRef::Union { variants, .. } = declared_type {
        let variant = select_union_variant(
            relation,
            column,
            declared_type,
            variants,
            value,
            definitions,
        )?;
        let mut encoded_value = encode_input_value(
            relation,
            column,
            variant,
            value.untyped(),
            encoded,
            definitions,
        )?;
        if let Some(declared_type) = type_ref_declared_name(variant, definitions) {
            attach_declared_type(relation, column, &mut encoded_value, declared_type, encoded)?;
        }
        return Ok(encoded_value);
    }

    let value = value.untyped();
    Ok(match value {
        Value::Number(value) => abi_value(
            SouffleRsValueKind::Number,
            SouffleRsValueData { number: *value },
        ),
        Value::Unsigned(value) => abi_value(
            SouffleRsValueKind::Unsigned,
            SouffleRsValueData {
                unsigned_value: *value,
            },
        ),
        Value::Float(value) => abi_value(
            SouffleRsValueKind::Float,
            SouffleRsValueData {
                float_value: *value,
            },
        ),
        Value::Symbol(value) => {
            encoded.symbols.push(ffi::cstring_argument(
                format!("{relation}.{column}"),
                value,
            )?);
            let symbol = encoded.symbols.last().expect("symbol was just pushed");
            abi_value(
                SouffleRsValueKind::Symbol,
                SouffleRsValueData {
                    symbol: SouffleRsString {
                        data: symbol.as_ptr(),
                        len: value.len(),
                    },
                },
            )
        }
        Value::Nullary => abi_value(
            SouffleRsValueKind::Nullary,
            SouffleRsValueData { unsigned_value: 0 },
        ),
        Value::Record(fields) => encode_record_input_value(
            relation,
            column,
            declared_type,
            fields,
            encoded,
            definitions,
        )?,
        Value::List(elements) => encode_list_input_value(
            relation,
            column,
            declared_type,
            elements,
            encoded,
            definitions,
        )?,
        Value::Adt { variant, fields } => encode_adt_input_value(
            relation,
            column,
            declared_type,
            variant,
            fields,
            encoded,
            definitions,
        )?,
        Value::Typed { .. } => unreachable!("untyped values cannot be typed"),
    })
}

fn encode_record_input_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    fields: &[Value],
    encoded: &mut EncodedRow,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<SouffleRsValue, SouffleError> {
    let TypeRef::Record(field_types) = declared_runtime_type(declared_type, definitions) else {
        return type_mismatch(
            relation,
            column,
            declared_type,
            Value::Record(fields.to_vec()),
        );
    };

    let mut values = Vec::with_capacity(fields.len());
    for (index, (field_type, field)) in field_types.iter().zip(fields).enumerate() {
        values.push(encode_input_value(
            relation,
            &format!("{column}.{index}"),
            field_type,
            field,
            encoded,
            definitions,
        )?);
    }
    push_input_composite(
        relation,
        column,
        encoded,
        SouffleRsValueKind::Record,
        values,
        None,
    )
}

fn encode_list_input_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    elements: &[Value],
    encoded: &mut EncodedRow,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<SouffleRsValue, SouffleError> {
    let TypeRef::List(element_type) = declared_runtime_type(declared_type, definitions) else {
        return type_mismatch(
            relation,
            column,
            declared_type,
            Value::List(elements.to_vec()),
        );
    };

    let mut values = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter().enumerate() {
        values.push(encode_input_value(
            relation,
            &format!("{column}[{index}]"),
            element_type,
            element,
            encoded,
            definitions,
        )?);
    }
    push_input_composite(
        relation,
        column,
        encoded,
        SouffleRsValueKind::List,
        values,
        None,
    )
}

fn encode_adt_input_value(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    variant: &str,
    fields: &[Value],
    encoded: &mut EncodedRow,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<SouffleRsValue, SouffleError> {
    let TypeRef::Adt { variants, .. } = declared_runtime_type(declared_type, definitions) else {
        return type_mismatch(
            relation,
            column,
            declared_type,
            Value::Adt {
                variant: variant.to_owned(),
                fields: fields.to_vec(),
            },
        );
    };
    let Some(field_types) = variants.get(variant) else {
        return Err(SouffleError::AdtVariantMismatch {
            relation: relation.to_owned(),
            column: column.to_owned(),
            variant: variant.to_owned(),
        });
    };

    let mut values = Vec::with_capacity(fields.len());
    for (index, (field_type, field)) in field_types.iter().zip(fields).enumerate() {
        values.push(encode_input_value(
            relation,
            &format!("{column}.{variant}.{index}"),
            field_type,
            field,
            encoded,
            definitions,
        )?);
    }
    push_input_composite(
        relation,
        column,
        encoded,
        SouffleRsValueKind::Adt,
        values,
        Some(variant),
    )
}

fn resolve_type_ref<'a>(
    declared_type: &'a TypeRef,
    definitions: &'a BTreeMap<String, TypeRef>,
) -> &'a TypeRef {
    match declared_type {
        TypeRef::Reference { name, .. } => definitions.get(name).unwrap_or(declared_type),
        _ => declared_type,
    }
}

fn declared_runtime_type<'a>(
    declared_type: &'a TypeRef,
    definitions: &'a BTreeMap<String, TypeRef>,
) -> &'a TypeRef {
    match resolve_type_ref(declared_type, definitions) {
        TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. } => {
            declared_runtime_type(base, definitions)
        }
        TypeRef::Union { variants, .. } => variants
            .first()
            .map(|variant| declared_runtime_type(variant, definitions))
            .unwrap_or(declared_type),
        resolved => resolved,
    }
}

fn select_union_variant<'a>(
    relation: &str,
    column: &str,
    union_type: &TypeRef,
    variants: &'a [TypeRef],
    value: &Value,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<&'a TypeRef, SouffleError> {
    if let Some(declared_type) = value.declared_type_name() {
        if declared_type != union_type.display_name() {
            let Some(variant) = variants.iter().find(|variant| {
                type_ref_declared_name(variant, definitions) == Some(declared_type)
            }) else {
                return Err(SouffleError::TypeMismatch {
                    relation: relation.to_owned(),
                    column: column.to_owned(),
                    expected: union_type.display_name(),
                    actual: declared_type.to_owned(),
                });
            };

            return match variant.accepts_value_with_definitions(value.untyped(), definitions) {
                TypeCheck::Ok => Ok(variant),
                TypeCheck::Mismatch { expected, actual } => Err(SouffleError::TypeMismatch {
                    relation: relation.to_owned(),
                    column: column.to_owned(),
                    expected,
                    actual,
                }),
                TypeCheck::AdtVariantMismatch { variant } => {
                    Err(SouffleError::AdtVariantMismatch {
                        relation: relation.to_owned(),
                        column: column.to_owned(),
                        variant,
                    })
                }
            };
        } else if let Some(inner_declared_type) = typed_inner_declared_type_name(value) {
            let Some(variant) = variants.iter().find(|variant| {
                type_ref_declared_name(variant, definitions) == Some(inner_declared_type)
            }) else {
                return Err(SouffleError::TypeMismatch {
                    relation: relation.to_owned(),
                    column: column.to_owned(),
                    expected: union_type.display_name(),
                    actual: inner_declared_type.to_owned(),
                });
            };

            return match variant.accepts_value_with_definitions(value.untyped(), definitions) {
                TypeCheck::Ok => Ok(variant),
                TypeCheck::Mismatch { expected, actual } => Err(SouffleError::TypeMismatch {
                    relation: relation.to_owned(),
                    column: column.to_owned(),
                    expected,
                    actual,
                }),
                TypeCheck::AdtVariantMismatch { variant } => {
                    Err(SouffleError::AdtVariantMismatch {
                        relation: relation.to_owned(),
                        column: column.to_owned(),
                        variant,
                    })
                }
            };
        }
    }

    variants
        .iter()
        .find(|variant| {
            matches!(
                variant.accepts_value_with_definitions(value.untyped(), definitions),
                TypeCheck::Ok
            )
        })
        .ok_or_else(|| SouffleError::TypeMismatch {
            relation: relation.to_owned(),
            column: column.to_owned(),
            expected: union_type.display_name(),
            actual: value
                .declared_type_name()
                .unwrap_or_else(|| value.kind().as_str())
                .to_owned(),
        })
}

fn typed_inner_declared_type_name(value: &Value) -> Option<&str> {
    match value {
        Value::Typed { value, .. } => value.declared_type_name(),
        _ => None,
    }
}

fn type_ref_declared_name<'a>(
    declared_type: &'a TypeRef,
    definitions: &'a BTreeMap<String, TypeRef>,
) -> Option<&'a str> {
    match declared_type {
        TypeRef::Adt { name, .. }
        | TypeRef::Reference { name, .. }
        | TypeRef::Subtype { name, .. }
        | TypeRef::Union { name, .. }
        | TypeRef::Declared { name, .. } => Some(name),
        other => match resolve_type_ref(other, definitions) {
            TypeRef::Adt { name, .. }
            | TypeRef::Reference { name, .. }
            | TypeRef::Subtype { name, .. }
            | TypeRef::Union { name, .. }
            | TypeRef::Declared { name, .. } => Some(name),
            _ => None,
        },
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

fn push_input_composite(
    relation: &str,
    column: &str,
    encoded: &mut EncodedRow,
    kind: SouffleRsValueKind,
    values: Vec<SouffleRsValue>,
    variant: Option<&str>,
) -> Result<SouffleRsValue, SouffleError> {
    let index = encoded.composites.len();
    encoded.composite_values.push(values);
    let values = encoded
        .composite_values
        .last()
        .expect("composite values were just pushed");
    let variant_string = if let Some(variant) = variant {
        encoded.variants.push(ffi::cstring_argument(
            format!("{relation}.{column}.variant"),
            variant,
        )?);
        let variant = encoded.variants.last().expect("variant was just pushed");
        SouffleRsString {
            data: variant.as_ptr(),
            len: variant.as_bytes().len(),
        }
    } else {
        SouffleRsString::null()
    };
    encoded.composites.push(SouffleRsInputComposite {
        kind,
        values: if values.is_empty() {
            std::ptr::null()
        } else {
            values.as_ptr()
        },
        len: values.len(),
        variant: variant_string,
    });
    Ok(abi_value(
        kind,
        SouffleRsValueData {
            composite: SouffleRsCompositeRef { index },
        },
    ))
}

fn abi_value(kind: SouffleRsValueKind, data: SouffleRsValueData) -> SouffleRsValue {
    SouffleRsValue {
        kind,
        declared_type: SouffleRsString::null(),
        as_: data,
    }
}

fn attach_declared_type(
    relation: &str,
    column: &str,
    value: &mut SouffleRsValue,
    declared_type: &str,
    encoded: &mut EncodedRow,
) -> Result<(), SouffleError> {
    encoded.declared_types.push(ffi::cstring_argument(
        format!("{relation}.{column}.declared_type"),
        declared_type,
    )?);
    let declared_type = encoded
        .declared_types
        .last()
        .expect("declared type was just pushed");
    value.declared_type = SouffleRsString {
        data: declared_type.as_ptr(),
        len: declared_type.as_bytes().len(),
    };
    Ok(())
}

fn type_mismatch(
    relation: &str,
    column: &str,
    declared_type: &TypeRef,
    value: Value,
) -> Result<SouffleRsValue, SouffleError> {
    let actual = match declared_type.accepts_value(&value) {
        TypeCheck::Mismatch { actual, .. } => actual,
        _ => value.kind().as_str().to_owned(),
    };
    Err(SouffleError::TypeMismatch {
        relation: relation.to_owned(),
        column: column.to_owned(),
        expected: declared_type.display_name(),
        actual,
    })
}
