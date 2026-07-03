use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
};

use crate::{
    Backend, RelationSchema, Row, SouffleError, TypeRef, Value,
    program::{RelationIteratorSource, validate_row},
    schema::TypeCheck,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactValueContext {
    TopLevel,
    Composite,
}

pub(super) fn prepare_exchange_dir(path: &Path) -> Result<(), SouffleError> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|source| SouffleError::FileIo {
            operation: "remove directory".to_owned(),
            path: path.display().to_string(),
            message: source.to_string(),
        })?;
    }
    fs::create_dir_all(path).map_err(|source| SouffleError::FileIo {
        operation: "create directory".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

pub(super) fn write_fact_file(
    root: &Path,
    relation: &RelationSchema,
    rows: &[Row],
) -> Result<(), SouffleError> {
    let path = root.join(relation_file_name(relation, "facts")?);
    let file = File::create(&path).map_err(|source| SouffleError::FileIo {
        operation: "create file".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })?;
    let mut writer = BufWriter::new(file);

    for row in rows {
        write_fact_row(&mut writer, relation, row, &path)?;
    }
    writer.flush().map_err(|source| SouffleError::FileIo {
        operation: "flush".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })
}

pub(super) fn stream_output_file(
    root: &Path,
    relation: &RelationSchema,
) -> Result<ProcessOutputRows, SouffleError> {
    let path = root.join(relation_file_name(relation, "csv")?);
    Ok(ProcessOutputRows {
        reader: open_output_reader(&path)?,
        path,
    })
}

#[derive(Debug)]
pub(super) struct ProcessOutputRows {
    path: PathBuf,
    reader: Option<BufReader<File>>,
}

impl RelationIteratorSource for ProcessOutputRows {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        let Some(reader) = &mut self.reader else {
            return Ok(None);
        };

        let mut record = String::new();
        loop {
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .map_err(|source| SouffleError::FileIo {
                    operation: "read".to_owned(),
                    path: self.path.display().to_string(),
                    message: source.to_string(),
                })?;
            if bytes == 0 {
                self.reader = None;
                if record.is_empty() {
                    return Ok(None);
                }
                break;
            }
            trim_line_ending(&mut line);
            record.push_str(&line);
            if output_record_complete(schema, &record) {
                break;
            }
            record.push('\n');
        }

        let row = parse_output_row(schema, &record, &self.path)?;
        validate_row(schema, &row)?;
        Ok(Some(row))
    }
}

pub(super) fn ensure_relation_supported(
    backend: Backend,
    relation: &RelationSchema,
) -> Result<(), SouffleError> {
    let definitions = named_type_definitions(relation);
    for attribute in relation.attributes() {
        ensure_type_supported(
            backend,
            relation,
            attribute.name(),
            attribute.declared_type(),
            FactValueContext::TopLevel,
            &definitions,
            &mut BTreeSet::new(),
        )?;
    }
    Ok(())
}

pub(super) fn ensure_row_fact_encodable(
    relation: &RelationSchema,
    row: &Row,
) -> Result<(), SouffleError> {
    let definitions = named_type_definitions(relation);
    for (attribute, value) in relation.attributes().iter().zip(row.values()) {
        encode_fact_value(
            relation,
            attribute.name(),
            attribute.declared_type(),
            value,
            FactValueContext::TopLevel,
            &definitions,
        )?;
    }
    Ok(())
}

pub(super) fn process_failure_message(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    format!(
        "process exited with status {}; stdout: {}; stderr: {}",
        output.status,
        stdout.trim(),
        stderr.trim()
    )
}

fn open_output_reader(path: &Path) -> Result<Option<BufReader<File>>, SouffleError> {
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(path).map_err(|source| SouffleError::FileIo {
        operation: "open file".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    })?;
    Ok(Some(BufReader::new(file)))
}

fn trim_line_ending(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
}

fn write_fact_row(
    writer: &mut impl Write,
    relation: &RelationSchema,
    row: &Row,
    path: &Path,
) -> Result<(), SouffleError> {
    let definitions = named_type_definitions(relation);
    for (index, (attribute, value)) in relation.attributes().iter().zip(row.values()).enumerate() {
        if index > 0 {
            writer
                .write_all(b"\t")
                .map_err(|source| file_write_error(path, source))?;
        }
        let field = encode_fact_value(
            relation,
            attribute.name(),
            attribute.declared_type(),
            value,
            FactValueContext::TopLevel,
            &definitions,
        )?;
        writer
            .write_all(field.as_bytes())
            .map_err(|source| file_write_error(path, source))?;
    }
    writer
        .write_all(b"\n")
        .map_err(|source| file_write_error(path, source))
}

fn parse_output_row(
    relation: &RelationSchema,
    line: &str,
    path: &Path,
) -> Result<Row, SouffleError> {
    let definitions = named_type_definitions(relation);
    let fields = split_output_fields(relation, line, path)?;
    if fields.len() != relation.arity() {
        return Err(SouffleError::ArtifactDecodeFailed {
            artifact: path.display().to_string(),
            message: format!(
                "relation `{}` expected {} fields but decoded {}",
                relation.name(),
                relation.arity(),
                fields.len()
            ),
        });
    }

    let values = relation
        .attributes()
        .iter()
        .zip(fields)
        .map(|(attribute, field)| {
            parse_output_value(
                relation,
                attribute.name(),
                attribute.declared_type(),
                field,
                path,
                &definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Row::new(values))
}

fn split_output_fields<'a>(
    relation: &RelationSchema,
    line: &'a str,
    path: &Path,
) -> Result<Vec<&'a str>, SouffleError> {
    if relation.arity() == 0 {
        return if line.is_empty() || line == "()" {
            Ok(Vec::new())
        } else {
            Ok(line.split('\t').collect())
        };
    }
    if relation.arity() == 1 {
        if !relation_output_may_span_lines(relation) || output_composite_depth(line) == 0 {
            return Ok(vec![line]);
        }
        return Err(SouffleError::ArtifactDecodeFailed {
            artifact: path.display().to_string(),
            message: format!(
                "relation `{}` has unbalanced composite output",
                relation.name()
            ),
        });
    }

    let mut fields = Vec::with_capacity(relation.arity());
    let mut start = 0;
    let mut syntax = OutputSyntax::default();
    for (offset, character) in line.char_indices() {
        if character == '\t' && syntax.is_top_level() {
            fields.push(&line[start..offset]);
            start = offset + character.len_utf8();
        } else {
            syntax.advance(character);
        }
    }
    fields.push(&line[start..]);

    if !syntax.is_complete() {
        return Err(SouffleError::ArtifactDecodeFailed {
            artifact: path.display().to_string(),
            message: format!(
                "relation `{}` has unbalanced composite output",
                relation.name()
            ),
        });
    }
    Ok(fields)
}

fn output_record_complete(relation: &RelationSchema, record: &str) -> bool {
    if relation.arity() == 0 {
        return true;
    }
    if output_composite_depth(record) != 0 {
        return false;
    }
    if relation.arity() == 1 {
        return true;
    }
    top_level_tab_count(record) + 1 >= relation.arity()
}

fn output_composite_depth(value: &str) -> usize {
    output_syntax(value).incomplete_depth()
}

fn top_level_tab_count(value: &str) -> usize {
    let mut count = 0;
    let mut syntax = OutputSyntax::default();
    for character in value.chars() {
        if character == '\t' && syntax.is_top_level() {
            count += 1;
        } else {
            syntax.advance(character);
        }
    }
    count
}

#[derive(Debug, Default)]
struct OutputSyntax {
    square_depth: usize,
    paren_depth: usize,
    in_quoted_symbol: bool,
    escaping: bool,
}

impl OutputSyntax {
    fn advance(&mut self, character: char) {
        if self.in_quoted_symbol {
            if self.escaping {
                self.escaping = false;
                return;
            }
            match character {
                '\\' => self.escaping = true,
                '"' => self.in_quoted_symbol = false,
                _ => {}
            }
            return;
        }

        match character {
            '"' => self.in_quoted_symbol = true,
            '[' => self.square_depth = self.square_depth.saturating_add(1),
            ']' if self.square_depth > 0 => self.square_depth -= 1,
            '(' => self.paren_depth = self.paren_depth.saturating_add(1),
            ')' if self.paren_depth > 0 => self.paren_depth -= 1,
            _ => {}
        }
    }

    fn is_top_level(&self) -> bool {
        !self.in_quoted_symbol && self.square_depth == 0 && self.paren_depth == 0
    }

    fn is_complete(&self) -> bool {
        self.square_depth == 0 && self.paren_depth == 0 && !self.in_quoted_symbol && !self.escaping
    }

    fn incomplete_depth(&self) -> usize {
        self.square_depth
            + self.paren_depth
            + usize::from(self.in_quoted_symbol)
            + usize::from(self.escaping)
    }
}

fn output_syntax(value: &str) -> OutputSyntax {
    let mut syntax = OutputSyntax::default();
    for character in value.chars() {
        syntax.advance(character);
    }
    syntax
}

fn relation_output_may_span_lines(relation: &RelationSchema) -> bool {
    relation.arity() == 1
        && relation
            .attributes()
            .first()
            .is_some_and(|attribute| type_output_may_span_lines(attribute.declared_type()))
}

fn type_output_may_span_lines(type_ref: &TypeRef) -> bool {
    match type_ref {
        TypeRef::Record(_) | TypeRef::List(_) | TypeRef::Adt { .. } => true,
        TypeRef::Reference { runtime, .. } => matches!(
            runtime,
            crate::ValueKind::Record | crate::ValueKind::List | crate::ValueKind::Adt
        ),
        TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. } => {
            type_output_may_span_lines(base)
        }
        TypeRef::Union { variants, .. } => variants.iter().any(type_output_may_span_lines),
        TypeRef::Number
        | TypeRef::Unsigned
        | TypeRef::Float
        | TypeRef::Symbol
        | TypeRef::Nullary => false,
    }
}

fn encode_fact_value(
    relation: &RelationSchema,
    column: &str,
    declared_type: &TypeRef,
    value: &Value,
    context: FactValueContext,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<String, SouffleError> {
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
        return encode_fact_value(
            relation,
            column,
            variant,
            value.untyped(),
            context,
            definitions,
        );
    }

    let value = value.untyped();
    match (declared_type, value) {
        (TypeRef::Number, Value::Number(value)) => Ok(value.to_string()),
        (TypeRef::Unsigned, Value::Unsigned(value)) => Ok(value.to_string()),
        (TypeRef::Float, Value::Float(value)) => Ok(format!("{value:?}")),
        (TypeRef::Symbol, Value::Symbol(value)) => encode_symbol(relation, column, value, context),
        (TypeRef::Nullary, Value::Nullary) if context == FactValueContext::TopLevel => {
            Ok(String::new())
        }
        (TypeRef::Nullary, Value::Nullary) => Err(unsupported_process_type(
            relation,
            column,
            declared_type,
            "process fact exchange only supports nullary values as top-level columns",
        )),
        (TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. }, value) => {
            encode_fact_value(relation, column, base, value, context, definitions)
        }
        (TypeRef::Record(field_types), Value::Record(fields)) => {
            encode_record_fact_value(relation, column, field_types, fields, definitions)
        }
        (TypeRef::List(element_type), Value::List(elements)) => {
            encode_list_fact_value(relation, column, element_type, elements, definitions)
        }
        (TypeRef::Adt { variants, .. }, Value::Adt { variant, fields }) => {
            encode_adt_fact_value(relation, column, variants, variant, fields, definitions)
        }
        _ => Err(SouffleError::TypeMismatch {
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            expected: declared_type.display_name(),
            actual: value.kind().as_str().to_owned(),
        }),
    }
}

fn encode_record_fact_value(
    relation: &RelationSchema,
    column: &str,
    field_types: &[TypeRef],
    fields: &[Value],
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<String, SouffleError> {
    if field_types.len() != fields.len() {
        return Err(SouffleError::TypeMismatch {
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            expected: format!("record<{} fields>", field_types.len()),
            actual: format!("record<{} fields>", fields.len()),
        });
    }

    let fields = field_types
        .iter()
        .zip(fields)
        .enumerate()
        .map(|(index, (field_type, field))| {
            encode_fact_value(
                relation,
                &format!("{column}.{index}"),
                field_type,
                field,
                FactValueContext::Composite,
                definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("[{}]", fields.join(", ")))
}

fn encode_list_fact_value(
    relation: &RelationSchema,
    column: &str,
    element_type: &TypeRef,
    elements: &[Value],
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<String, SouffleError> {
    let mut tail = "nil".to_owned();
    for (index, element) in elements.iter().enumerate().rev() {
        let head = encode_fact_value(
            relation,
            &format!("{column}[{index}]"),
            element_type,
            element,
            FactValueContext::Composite,
            definitions,
        )?;
        tail = format!("[{head}, {tail}]");
    }
    Ok(tail)
}

fn encode_adt_fact_value(
    relation: &RelationSchema,
    column: &str,
    variants: &BTreeMap<String, Vec<TypeRef>>,
    variant: &str,
    fields: &[Value],
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<String, SouffleError> {
    let Some(field_types) = variants.get(variant) else {
        return Err(SouffleError::AdtVariantMismatch {
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            variant: variant.to_owned(),
        });
    };
    if field_types.len() != fields.len() {
        return Err(SouffleError::TypeMismatch {
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            expected: format!("adt::{variant}<{} fields>", field_types.len()),
            actual: format!("adt::{variant}<{} fields>", fields.len()),
        });
    }

    if fields.is_empty() {
        return Ok(format!("${variant}"));
    }

    let fields = field_types
        .iter()
        .zip(fields)
        .enumerate()
        .map(|(index, (field_type, field))| {
            encode_fact_value(
                relation,
                &format!("{column}.{variant}.{index}"),
                field_type,
                field,
                FactValueContext::Composite,
                definitions,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("${variant}({})", fields.join(", ")))
}

fn parse_output_value(
    relation: &RelationSchema,
    column: &str,
    declared_type: &TypeRef,
    field: &str,
    path: &Path,
    definitions: &BTreeMap<String, TypeRef>,
) -> Result<Value, SouffleError> {
    let declared_type = resolve_type_ref(declared_type, definitions);
    match declared_type {
        TypeRef::Number => parse_field(path, field, "number", str::parse::<i64>).map(Value::Number),
        TypeRef::Unsigned => {
            parse_field(path, field, "unsigned", str::parse::<u64>).map(Value::Unsigned)
        }
        TypeRef::Float => parse_field(path, field, "float", str::parse::<f64>).map(Value::Float),
        TypeRef::Symbol => Ok(Value::Symbol(field.to_owned())),
        TypeRef::Nullary => {
            if field.is_empty() {
                Ok(Value::Nullary)
            } else {
                Err(SouffleError::ArtifactDecodeFailed {
                    artifact: path.display().to_string(),
                    message: format!("nullary column `{column}` received `{field}`"),
                })
            }
        }
        TypeRef::Subtype { name, base } => {
            parse_output_value(relation, column, base, field, path, definitions)
                .map(|value| Value::typed(name.clone(), value.into_untyped()))
        }
        TypeRef::Declared { name, runtime } => {
            parse_output_value(relation, column, runtime, field, path, definitions)
                .map(|value| Value::typed(name.clone(), value.into_untyped()))
        }
        TypeRef::Union { name, variants } => {
            let mut last_error = None;
            for variant in variants {
                match parse_output_value(relation, column, variant, field, path, definitions) {
                    Ok(value) => {
                        return Ok(Value::typed(name.clone(), value.into_untyped()));
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| {
                unsupported_process_type(
                    relation,
                    column,
                    declared_type,
                    "process output exchange could not decode union field",
                )
            }))
        }
        TypeRef::Record(_) | TypeRef::List(_) | TypeRef::Adt { .. } => {
            let mut parser = CompositeParser::new(field, path, definitions);
            let value = parser.parse_value(relation, column, declared_type)?;
            parser.finish()?;
            Ok(value)
        }
        TypeRef::Reference { .. } => unreachable!("reference resolved before output parse match"),
    }
}

fn ensure_type_supported(
    backend: Backend,
    relation: &RelationSchema,
    column: &str,
    declared_type: &TypeRef,
    context: FactValueContext,
    definitions: &BTreeMap<String, TypeRef>,
    seen: &mut BTreeSet<String>,
) -> Result<(), SouffleError> {
    if let TypeRef::Reference { name, .. } = declared_type {
        if !seen.insert(name.clone()) {
            return Ok(());
        }
        let result = definitions.get(name).map_or(Ok(()), |resolved| {
            ensure_type_supported(
                backend,
                relation,
                column,
                resolved,
                context,
                definitions,
                seen,
            )
        });
        seen.remove(name);
        return result;
    }

    match declared_type {
        TypeRef::Number | TypeRef::Unsigned | TypeRef::Float | TypeRef::Symbol => Ok(()),
        TypeRef::Nullary if context == FactValueContext::TopLevel => Ok(()),
        TypeRef::Nullary => Err(SouffleError::UnsupportedType {
            backend,
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            declared_type: declared_type.display_name(),
            message:
                "process fact/output exchange only supports nullary values as top-level columns"
                    .to_owned(),
        }),
        TypeRef::Subtype { base, .. } | TypeRef::Declared { runtime: base, .. } => {
            ensure_type_supported(backend, relation, column, base, context, definitions, seen)
        }
        TypeRef::Union { variants, .. } => {
            for variant in variants {
                ensure_type_supported(
                    backend,
                    relation,
                    column,
                    variant,
                    context,
                    definitions,
                    seen,
                )?;
            }
            Ok(())
        }
        TypeRef::Record(fields) => {
            for field in fields {
                ensure_type_supported(
                    backend,
                    relation,
                    column,
                    field,
                    FactValueContext::Composite,
                    definitions,
                    seen,
                )?;
            }
            Ok(())
        }
        TypeRef::List(element) => ensure_type_supported(
            backend,
            relation,
            column,
            element,
            FactValueContext::Composite,
            definitions,
            seen,
        ),
        TypeRef::Adt { variants, .. } => {
            for fields in variants.values() {
                for field in fields {
                    ensure_type_supported(
                        backend,
                        relation,
                        column,
                        field,
                        FactValueContext::Composite,
                        definitions,
                        seen,
                    )?;
                }
            }
            Ok(())
        }
        TypeRef::Reference { .. } => unreachable!("reference handled before match"),
    }
}

fn unsupported_process_type(
    relation: &RelationSchema,
    column: &str,
    declared_type: &TypeRef,
    message: &str,
) -> SouffleError {
    SouffleError::UnsupportedType {
        backend: Backend::Process,
        relation: relation.name().to_owned(),
        column: column.to_owned(),
        declared_type: declared_type.display_name(),
        message: message.to_owned(),
    }
}

fn accepts_value(
    declared_type: &TypeRef,
    value: &Value,
    definitions: &BTreeMap<String, TypeRef>,
) -> bool {
    let declared_type = resolve_type_ref(declared_type, definitions);
    matches!(
        declared_type.accepts_value_with_definitions(value, definitions),
        TypeCheck::Ok
    )
}

fn select_union_variant<'a>(
    relation: &RelationSchema,
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
                    relation: relation.name().to_owned(),
                    column: column.to_owned(),
                    expected: union_type.display_name(),
                    actual: declared_type.to_owned(),
                });
            };

            return match resolve_type_ref(variant, definitions)
                .accepts_value_with_definitions(value.untyped(), definitions)
            {
                TypeCheck::Ok => Ok(variant),
                TypeCheck::Mismatch { expected, actual } => Err(SouffleError::TypeMismatch {
                    relation: relation.name().to_owned(),
                    column: column.to_owned(),
                    expected,
                    actual,
                }),
                TypeCheck::AdtVariantMismatch { variant } => {
                    Err(SouffleError::AdtVariantMismatch {
                        relation: relation.name().to_owned(),
                        column: column.to_owned(),
                        variant,
                    })
                }
            };
        }
    }

    variants
        .iter()
        .find(|variant| accepts_value(variant, value.untyped(), definitions))
        .ok_or_else(|| SouffleError::TypeMismatch {
            relation: relation.name().to_owned(),
            column: column.to_owned(),
            expected: union_type.display_name(),
            actual: value
                .declared_type_name()
                .unwrap_or_else(|| value.kind().as_str())
                .to_owned(),
        })
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

fn named_type_definitions(relation: &RelationSchema) -> BTreeMap<String, TypeRef> {
    let mut definitions = BTreeMap::new();
    for attribute in relation.attributes() {
        attribute
            .declared_type()
            .collect_named_type_definitions(&mut definitions);
    }
    definitions
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

fn encode_symbol(
    relation: &RelationSchema,
    column: &str,
    value: &str,
    context: FactValueContext,
) -> Result<String, SouffleError> {
    if value
        .bytes()
        .any(|byte| matches!(byte, b'\t' | b'\n' | b'\r'))
    {
        return Err(SouffleError::EncodeFailed {
            artifact: format!("{}.facts", relation.name()),
            message: format!("symbol column `{column}` contains a fact-file delimiter"),
        });
    }

    if context == FactValueContext::TopLevel {
        return Ok(value.to_owned());
    }

    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '"' => encoded.push_str("\\\""),
            _ => encoded.push(character),
        }
    }
    encoded.push('"');
    Ok(encoded)
}

struct CompositeParser<'a> {
    input: &'a str,
    path: &'a Path,
    definitions: &'a BTreeMap<String, TypeRef>,
    position: usize,
}

impl<'a> CompositeParser<'a> {
    fn new(input: &'a str, path: &'a Path, definitions: &'a BTreeMap<String, TypeRef>) -> Self {
        Self {
            input,
            path,
            definitions,
            position: 0,
        }
    }

    fn finish(&mut self) -> Result<(), SouffleError> {
        self.skip_spaces();
        if self.position == self.input.len() {
            Ok(())
        } else {
            Err(self.decode_error(format!(
                "unexpected trailing text `{}`",
                &self.input[self.position..]
            )))
        }
    }

    fn parse_value(
        &mut self,
        relation: &RelationSchema,
        column: &str,
        declared_type: &TypeRef,
    ) -> Result<Value, SouffleError> {
        let declared_type = resolve_type_ref(declared_type, self.definitions);
        match declared_type {
            TypeRef::Number => self
                .parse_atom("number", str::parse::<i64>)
                .map(Value::Number),
            TypeRef::Unsigned => self
                .parse_atom("unsigned", str::parse::<u64>)
                .map(Value::Unsigned),
            TypeRef::Float => self
                .parse_atom("float", str::parse::<f64>)
                .map(Value::Float),
            TypeRef::Symbol => self.parse_symbol().map(Value::Symbol),
            TypeRef::Nullary => {
                let atom = self.take_atom();
                if atom.is_empty() {
                    Ok(Value::Nullary)
                } else {
                    Err(self.decode_error(format!("nullary column `{column}` received `{atom}`")))
                }
            }
            TypeRef::Subtype { name, base } => self
                .parse_value(relation, column, base)
                .map(|value| Value::typed(name.clone(), value.into_untyped())),
            TypeRef::Declared { name, runtime } => self
                .parse_value(relation, column, runtime)
                .map(|value| Value::typed(name.clone(), value.into_untyped())),
            TypeRef::Union { name, variants } => {
                let start = self.position;
                let mut last_error = None;
                for variant in variants {
                    self.position = start;
                    match self.parse_value(relation, column, variant) {
                        Ok(value) => {
                            return Ok(Value::typed(name.clone(), value.into_untyped()));
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                self.position = start;
                Err(last_error.unwrap_or_else(|| {
                    self.decode_error(format!(
                        "process output exchange could not decode union `{}`",
                        declared_type.display_name()
                    ))
                }))
            }
            TypeRef::Record(fields) => self.parse_record(relation, column, declared_type, fields),
            TypeRef::List(element) => self.parse_list(relation, column, element),
            TypeRef::Adt { variants, .. } => {
                self.parse_adt(relation, column, declared_type, variants)
            }
            TypeRef::Reference { .. } => unreachable!("reference resolved before parse match"),
        }
    }

    fn parse_record(
        &mut self,
        relation: &RelationSchema,
        column: &str,
        declared_type: &TypeRef,
        fields: &[TypeRef],
    ) -> Result<Value, SouffleError> {
        self.expect('[')?;
        let mut values = Vec::with_capacity(fields.len());
        for (index, field_type) in fields.iter().enumerate() {
            if index > 0 {
                self.expect(',')?;
            }
            values.push(self.parse_value(relation, &format!("{column}.{index}"), field_type)?);
        }
        self.expect(']')?;
        if values.len() == fields.len() {
            Ok(Value::Record(values))
        } else {
            Err(self.decode_error(format!(
                "record `{}` decoded {} fields but schema expects {}",
                declared_type.display_name(),
                values.len(),
                fields.len()
            )))
        }
    }

    fn parse_list(
        &mut self,
        relation: &RelationSchema,
        column: &str,
        element: &TypeRef,
    ) -> Result<Value, SouffleError> {
        if self.consume_nil() {
            return Ok(Value::List(Vec::new()));
        }

        self.expect('[')?;
        let head = self.parse_value(relation, &format!("{column}[]"), element)?;
        self.expect(',')?;
        let tail = self.parse_list(relation, column, element)?;
        self.expect(']')?;

        let Value::List(mut elements) = tail else {
            return Err(self.decode_error("list tail did not decode as a list".to_owned()));
        };
        elements.insert(0, head);
        Ok(Value::List(elements))
    }

    fn parse_adt(
        &mut self,
        relation: &RelationSchema,
        column: &str,
        declared_type: &TypeRef,
        variants: &BTreeMap<String, Vec<TypeRef>>,
    ) -> Result<Value, SouffleError> {
        self.expect('$')?;
        let variant = self.take_variant_name()?;
        let Some(fields) = variants.get(&variant) else {
            return Err(self.decode_error(format!(
                "ADT variant `{variant}` is not in schema `{}`",
                declared_type.display_name()
            )));
        };

        if fields.is_empty() && !self.peek_is('(') {
            return Ok(Value::Adt {
                variant,
                fields: Vec::new(),
            });
        }

        self.expect('(')?;
        let mut values = Vec::with_capacity(fields.len());
        for (index, field_type) in fields.iter().enumerate() {
            if index > 0 {
                self.expect(',')?;
            }
            values.push(self.parse_value(
                relation,
                &format!("{column}.{variant}.{index}"),
                field_type,
            )?);
        }
        self.expect(')')?;
        Ok(Value::Adt {
            variant,
            fields: values,
        })
    }

    fn parse_atom<T, E>(
        &mut self,
        type_name: &str,
        parse: impl FnOnce(&str) -> Result<T, E>,
    ) -> Result<T, SouffleError>
    where
        E: std::fmt::Display,
    {
        let atom = self.take_atom();
        parse(atom).map_err(|source| {
            self.decode_error(format!("failed to parse `{atom}` as {type_name}: {source}"))
        })
    }

    fn parse_symbol(&mut self) -> Result<String, SouffleError> {
        self.skip_spaces();
        if self.peek_is('"') {
            self.position += 1;
            let mut symbol = String::new();
            loop {
                let Some(character) = self.next_char() else {
                    return Err(self.decode_error("unterminated quoted symbol".to_owned()));
                };
                match character {
                    '"' => return Ok(symbol),
                    '\\' => {
                        let Some(escaped) = self.next_char() else {
                            return Err(self.decode_error("unterminated symbol escape".to_owned()));
                        };
                        match escaped {
                            'n' => symbol.push('\n'),
                            'r' => symbol.push('\r'),
                            't' => symbol.push('\t'),
                            '"' => symbol.push('"'),
                            '\\' => symbol.push('\\'),
                            other => symbol.push(other),
                        }
                    }
                    other => symbol.push(other),
                }
            }
        }

        Ok(self.take_atom().to_owned())
    }

    fn take_atom(&mut self) -> &'a str {
        self.skip_spaces();
        let start = self.position;
        while let Some(character) = self.peek_char() {
            if matches!(character, ',' | ']' | ')') {
                break;
            }
            self.position += character.len_utf8();
        }
        self.input[start..self.position].trim_end()
    }

    fn take_variant_name(&mut self) -> Result<String, SouffleError> {
        let start = self.position;
        while let Some(character) = self.peek_char() {
            if character == '(' || character.is_whitespace() || matches!(character, ',' | ']' | ')')
            {
                break;
            }
            self.position += character.len_utf8();
        }
        if start == self.position {
            return Err(self.decode_error("expected ADT variant name".to_owned()));
        }
        Ok(self.input[start..self.position].to_owned())
    }

    fn consume_nil(&mut self) -> bool {
        self.skip_spaces();
        let Some(rest) = self.input.get(self.position..) else {
            return false;
        };
        if !rest.starts_with("nil") {
            return false;
        }
        let next = self.position + "nil".len();
        let at_boundary = self
            .input
            .get(next..)
            .and_then(|tail| tail.chars().next())
            .is_none_or(|character| matches!(character, ',' | ']' | ')' | ' '));
        if !at_boundary {
            return false;
        }
        self.position = next;
        true
    }

    fn expect(&mut self, expected: char) -> Result<(), SouffleError> {
        self.skip_spaces();
        match self.next_char() {
            Some(actual) if actual == expected => Ok(()),
            Some(actual) => {
                Err(self.decode_error(format!("expected `{expected}` but found `{actual}`")))
            }
            None => {
                Err(self.decode_error(format!("expected `{expected}` but reached end of field")))
            }
        }
    }

    fn peek_is(&mut self, expected: char) -> bool {
        self.skip_spaces();
        self.peek_char() == Some(expected)
    }

    fn skip_spaces(&mut self) {
        while let Some(character) = self.peek_char() {
            if !character.is_whitespace() {
                break;
            }
            self.position += character.len_utf8();
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.input.get(self.position..)?.chars().next()
    }

    fn next_char(&mut self) -> Option<char> {
        let character = self.peek_char()?;
        self.position += character.len_utf8();
        Some(character)
    }

    fn decode_error(&self, message: String) -> SouffleError {
        SouffleError::ArtifactDecodeFailed {
            artifact: self.path.display().to_string(),
            message,
        }
    }
}

fn parse_field<T, E>(
    path: &Path,
    field: &str,
    type_name: &str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, SouffleError>
where
    E: std::fmt::Display,
{
    parse(field).map_err(|source| SouffleError::ArtifactDecodeFailed {
        artifact: path.display().to_string(),
        message: format!("failed to parse `{field}` as {type_name}: {source}"),
    })
}

fn relation_file_name(relation: &RelationSchema, extension: &str) -> Result<String, SouffleError> {
    let name = relation.name();
    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err(SouffleError::EncodeFailed {
            artifact: format!("{name}.{extension}"),
            message: "relation name is not safe as a process backend file name".to_owned(),
        });
    }
    Ok(format!("{name}.{extension}"))
}

fn file_write_error(path: &Path, source: std::io::Error) -> SouffleError {
    SouffleError::FileIo {
        operation: "write".to_owned(),
        path: path.display().to_string(),
        message: source.to_string(),
    }
}
