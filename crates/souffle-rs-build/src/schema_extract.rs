use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
};

use serde::Deserialize;
use souffle_rs::{
    AttributeSchema, RelationBundle, RelationId, RelationKind, RelationSchema, TypeRef, ValueKind,
};

use crate::{BuildError, CommandFailure, SouffleCommand};

#[derive(Debug, Deserialize)]
struct AstParams {
    records: BTreeMap<String, AstParamRelation>,
    relation: AstParamRelation,
}

#[derive(Debug, Deserialize)]
struct AstParamRelation {
    arity: usize,
    params: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AstTypes {
    #[serde(rename = "ADTs")]
    adts: BTreeMap<String, AstAdt>,
    records: BTreeMap<String, AstTypeRelation>,
    relation: AstTypeRelation,
}

#[derive(Debug, Deserialize)]
struct AstTypeRelation {
    arity: usize,
    types: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AstAdt {
    branches: Vec<AstAdtBranch>,
    #[serde(rename = "enum", default)]
    is_enum: bool,
}

#[derive(Debug, Deserialize)]
struct AstAdtBranch {
    name: String,
    types: Vec<String>,
}

#[derive(Debug, Default)]
struct TypeDefinitions {
    subtypes: BTreeMap<String, String>,
    unions: BTreeMap<String, Vec<String>>,
    records: BTreeMap<String, Vec<DeclaredField>>,
    adts: BTreeMap<String, Vec<DeclaredAdtBranch>>,
}

#[derive(Debug, Clone)]
struct DeclaredField {
    name: String,
    ty: String,
}

#[derive(Debug, Clone)]
struct DeclaredAdtBranch {
    name: String,
    fields: Vec<String>,
}

struct TypeContext<'a> {
    program: &'a str,
    relation: &'a str,
    column: &'a str,
    params: &'a AstParams,
    types: &'a AstTypes,
    definitions: &'a TypeDefinitions,
}

struct DeclaredTypeContext<'a> {
    program: &'a str,
    relation: &'a str,
    column: &'a str,
    definitions: &'a TypeDefinitions,
}

pub(crate) fn extract_schema_bundle(
    command: &SouffleCommand,
) -> Result<RelationBundle, BuildError> {
    let output = Command::new(command.executable())
        .args(command.args())
        .current_dir(command.working_dir())
        .output()
        .map_err(|source| BuildError::CommandSpawnFailed {
            command: command.command_line(),
            working_dir: command.working_dir().display().to_string(),
            message: source.to_string(),
        })?;

    if !output.status.success() {
        return Err(BuildError::CommandFailed(Box::new(CommandFailure {
            program: command.program().to_owned(),
            command: command.command_line(),
            working_dir: command.working_dir().display().to_string(),
            status: output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string()),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })));
    }

    parse_transformed_ast(command.program(), &String::from_utf8_lossy(&output.stdout))
}

fn parse_transformed_ast(program: &str, ast: &str) -> Result<RelationBundle, BuildError> {
    let mut bundle = RelationBundle::new();
    let mut next_id = 0u32;
    let definitions = type_definitions(ast);

    for line in ast.lines().map(str::trim) {
        if let Some((relation_name, attributes)) = declaration_directive(line) {
            let relation = declaration_relation_schema(
                program,
                relation_name,
                next_id,
                attributes,
                &definitions,
            )?;
            let is_new = bundle.get(relation.name()).is_none();
            merge_relation(program, &mut bundle, relation)?;
            if is_new {
                next_id = next_id.saturating_add(1);
            }
            continue;
        }

        let Some((kind, relation_name)) = relation_directive(line) else {
            continue;
        };
        let params_json = extract_params_json(program, relation_name, line)?;
        let types_json = extract_types_json(program, relation_name, line)?;
        let params = parse_params(program, relation_name, params_json)?;
        let types = parse_types(program, relation_name, types_json)?;
        let relation = relation_schema(
            program,
            relation_name,
            kind,
            next_id,
            params,
            types,
            &definitions,
        )?;
        let is_new = bundle.get(relation.name()).is_none();
        merge_relation(program, &mut bundle, relation)?;
        if is_new {
            next_id = next_id.saturating_add(1);
        }
    }

    Ok(bundle)
}

fn declaration_directive(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix(".decl ")?;
    let (name, attributes) = rest.split_once('(')?;
    let attributes = attributes.trim().strip_suffix(')')?.trim();
    let name = name.trim();
    (!name.is_empty()).then_some((name, attributes))
}

fn relation_directive(line: &str) -> Option<(RelationKind, &str)> {
    let (kind, rest) = line
        .strip_prefix(".input ")
        .map(|rest| (RelationKind::Input, rest))
        .or_else(|| {
            line.strip_prefix(".output ")
                .map(|rest| (RelationKind::Output, rest))
        })?;
    let name = rest.split_once('(')?.0.trim();
    (!name.is_empty()).then_some((kind, name))
}

fn type_definitions(ast: &str) -> TypeDefinitions {
    let mut definitions = TypeDefinitions::default();

    for line in ast.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix(".type ") else {
            continue;
        };

        if let Some((name, base)) = rest.split_once("<:") {
            let name = name.trim();
            let base = base.trim();
            if !name.is_empty() && !base.is_empty() {
                definitions
                    .subtypes
                    .insert(name.to_owned(), base.to_owned());
            }
            continue;
        }

        let Some((name, variants)) = rest.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let variants = variants.trim();
        if let Some(fields) = record_definition_fields(variants) {
            definitions.records.insert(name.to_owned(), fields);
            continue;
        }

        let variants = variants.split('|').map(str::trim).collect::<Vec<_>>();
        if variants.len() < 2 || !variants.iter().all(|variant| is_identifier(variant)) {
            if let Some(branches) = adt_definition_branches(variants) {
                definitions.adts.insert(name.to_owned(), branches);
            }
            continue;
        }
        definitions.unions.insert(
            name.to_owned(),
            variants.into_iter().map(str::to_owned).collect::<Vec<_>>(),
        );
    }

    definitions
}

fn record_definition_fields(definition: &str) -> Option<Vec<DeclaredField>> {
    let inner = definition.strip_prefix('[')?.strip_suffix(']')?;
    parse_declared_fields(inner)
}

fn adt_definition_branches(variants: Vec<&str>) -> Option<Vec<DeclaredAdtBranch>> {
    let mut branches = Vec::with_capacity(variants.len());
    for variant in variants {
        let variant = variant.trim();
        if variant.is_empty() {
            return None;
        }
        let Some((name, fields)) = variant.split_once('{') else {
            branches.push(DeclaredAdtBranch {
                name: variant.to_owned(),
                fields: Vec::new(),
            });
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        let fields = fields.trim().strip_suffix('}')?;
        branches.push(DeclaredAdtBranch {
            name: name.to_owned(),
            fields: parse_declared_fields(fields)?
                .into_iter()
                .map(|field| field.ty)
                .collect(),
        });
    }
    Some(branches)
}

fn parse_declared_fields(fields: &str) -> Option<Vec<DeclaredField>> {
    let fields = fields.trim();
    if fields.is_empty() {
        return Some(Vec::new());
    }

    split_top_level(fields, ',')
        .into_iter()
        .map(|field| {
            let (name, ty) = field.split_once(':')?;
            let name = name.trim();
            let ty = ty.trim();
            (!name.is_empty() && !ty.is_empty()).then(|| DeclaredField {
                name: name.to_owned(),
                ty: ty.to_owned(),
            })
        })
        .collect()
}

fn split_top_level(value: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (index, ch) in value.char_indices() {
        match ch {
            '[' | '{' | '(' => depth = depth.saturating_add(1),
            ']' | '}' | ')' => depth = depth.saturating_sub(1),
            _ if ch == delimiter && depth == 0 => {
                parts.push(value[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(value[start..].trim());
    parts
}

fn extract_params_json<'line>(
    program: &str,
    relation: &str,
    line: &'line str,
) -> Result<&'line str, BuildError> {
    let Some(start) = line.find("params=\"") else {
        return Err(schema_error(
            program,
            format!("relation `{relation}` metadata has no params JSON"),
        ));
    };
    let start = start + "params=\"".len();
    let Some(end) = line[start..].find("\",types=\"") else {
        return Err(schema_error(
            program,
            format!("relation `{relation}` params JSON is not followed by types JSON"),
        ));
    };
    Ok(&line[start..start + end])
}

fn primitive_type_name(name: &str) -> Option<TypeRef> {
    match name {
        "number" => Some(TypeRef::Number),
        "unsigned" => Some(TypeRef::Unsigned),
        "float" => Some(TypeRef::Float),
        "symbol" => Some(TypeRef::Symbol),
        _ => None,
    }
}

fn scalar_runtime_type(tag: &str) -> Option<TypeRef> {
    match tag {
        "i" => Some(TypeRef::Number),
        "u" => Some(TypeRef::Unsigned),
        "f" => Some(TypeRef::Float),
        "s" => Some(TypeRef::Symbol),
        _ => None,
    }
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn extract_types_json<'line>(
    program: &str,
    relation: &str,
    line: &'line str,
) -> Result<&'line str, BuildError> {
    let Some(start) = line.find("types=\"") else {
        return Err(schema_error(
            program,
            format!("relation `{relation}` metadata has no types JSON"),
        ));
    };
    let start = start + "types=\"".len();
    let Some(end) = line.rfind("\")") else {
        return Err(schema_error(
            program,
            format!("relation `{relation}` types JSON is not terminated"),
        ));
    };
    if end < start {
        return Err(schema_error(
            program,
            format!("relation `{relation}` types JSON terminator precedes value"),
        ));
    }
    Ok(&line[start..end])
}

fn parse_params(program: &str, relation: &str, json: &str) -> Result<AstParams, BuildError> {
    serde_json::from_str(json).map_err(|source| {
        schema_error(
            program,
            format!("relation `{relation}` params JSON could not be parsed: {source}"),
        )
    })
}

fn parse_types(program: &str, relation: &str, json: &str) -> Result<AstTypes, BuildError> {
    serde_json::from_str(json).map_err(|source| {
        schema_error(
            program,
            format!("relation `{relation}` types JSON could not be parsed: {source}"),
        )
    })
}

fn relation_schema(
    program: &str,
    relation: &str,
    kind: RelationKind,
    id: u32,
    params: AstParams,
    types: AstTypes,
    definitions: &TypeDefinitions,
) -> Result<RelationSchema, BuildError> {
    if params.relation.arity != types.relation.arity
        || params.relation.params.len() != params.relation.arity
        || types.relation.types.len() != types.relation.arity
    {
        return Err(schema_error(
            program,
            format!(
                "relation `{relation}` params/types arity mismatch: params arity {}, params len {}, types arity {}, types len {}",
                params.relation.arity,
                params.relation.params.len(),
                types.relation.arity,
                types.relation.types.len()
            ),
        ));
    }

    let mut attributes = Vec::with_capacity(params.relation.arity);
    for (index, (name, ty)) in params
        .relation
        .params
        .iter()
        .zip(&types.relation.types)
        .enumerate()
    {
        let column = if name.is_empty() {
            format!("column_{index}")
        } else {
            name.clone()
        };
        let context = TypeContext {
            program,
            relation,
            column: &column,
            params: &params,
            types: &types,
            definitions,
        };
        let declared_type = type_ref(&context, ty, &mut Vec::new())?;
        attributes.push(AttributeSchema::new(column, declared_type));
    }

    Ok(RelationSchema::new(
        RelationId::new(id),
        relation,
        kind,
        attributes,
    ))
}

fn declaration_relation_schema(
    program: &str,
    relation: &str,
    id: u32,
    attributes_source: &str,
    definitions: &TypeDefinitions,
) -> Result<RelationSchema, BuildError> {
    let mut attributes = Vec::new();
    if !attributes_source.is_empty() {
        for (index, attribute) in split_top_level(attributes_source, ',')
            .into_iter()
            .enumerate()
        {
            let Some((name, ty)) = attribute.split_once(':') else {
                return Err(schema_error(
                    program,
                    format!(
                        "relation `{relation}` declaration attribute `{attribute}` has no type"
                    ),
                ));
            };
            let column = if name.trim().is_empty() {
                format!("column_{index}")
            } else {
                name.trim().to_owned()
            };
            let context = DeclaredTypeContext {
                program,
                relation,
                column: &column,
                definitions,
            };
            let declared_type = declared_type_ref(&context, ty.trim(), &mut Vec::new())?;
            attributes.push(AttributeSchema::new(column, declared_type));
        }
    }

    Ok(RelationSchema::intermediate(
        RelationId::new(id),
        relation,
        attributes,
    ))
}

fn merge_relation(
    program: &str,
    bundle: &mut RelationBundle,
    relation: RelationSchema,
) -> Result<(), BuildError> {
    let Some(existing) = bundle.get(relation.name()).cloned() else {
        bundle.insert(relation);
        return Ok(());
    };

    let (kind, attributes) = match (existing.kind(), relation.kind()) {
        (RelationKind::Intermediate, RelationKind::Intermediate) => {
            if existing.attributes() != relation.attributes() {
                return Err(schema_error(
                    program,
                    format!(
                        "relation `{}` has inconsistent declaration schemas",
                        relation.name()
                    ),
                ));
            }
            (RelationKind::Intermediate, existing.attributes().to_vec())
        }
        (RelationKind::Intermediate, kind) => (kind, relation.attributes().to_vec()),
        (kind, RelationKind::Intermediate) => (kind, existing.attributes().to_vec()),
        (kind, _) => {
            if existing.attributes() != relation.attributes() {
                return Err(schema_error(
                    program,
                    format!(
                        "relation `{}` has inconsistent input/output schemas",
                        relation.name()
                    ),
                ));
            }
            (kind, existing.attributes().to_vec())
        }
    };
    let merged = RelationSchema::new(existing.id(), existing.name(), kind, attributes)
        .with_loadable(existing.is_loadable() || relation.is_loadable())
        .with_printable(existing.is_printable() || relation.is_printable());
    bundle.insert(merged);
    Ok(())
}

fn type_ref(
    context: &TypeContext<'_>,
    token: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    let Some((tag, name)) = token.split_once(':') else {
        return Err(unsupported_type(context, token));
    };

    match (tag, name) {
        ("i", "number") => Ok(TypeRef::Number),
        ("u", "unsigned") => Ok(TypeRef::Unsigned),
        ("f", "float") => Ok(TypeRef::Float),
        ("s", "symbol") => Ok(TypeRef::Symbol),
        ("i", declared) | ("u", declared) | ("f", declared) | ("s", declared) => {
            declared_scalar_type(context, tag, declared, stack)
        }
        ("r", _) => record_type(context, token, stack),
        ("+", _) => adt_type(context, token, stack),
        _ => Err(unsupported_type(context, token)),
    }
}

fn declared_scalar_type(
    context: &TypeContext<'_>,
    tag: &str,
    declared: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    if let Some(subtype) = subtype_type_ref(
        context.program,
        context.relation,
        context.column,
        context.definitions,
        declared,
        stack,
    )? {
        return Ok(subtype);
    }

    if let Some(variants) = context.definitions.unions.get(declared) {
        let marker = format!("union:{declared}");
        if stack.iter().any(|entry| entry == &marker) {
            return Err(schema_error(
                context.program,
                format!(
                    "relation `{}` column `{}` uses recursive union type `{declared}`",
                    context.relation, context.column
                ),
            ));
        }

        stack.push(marker);
        let variants = variants
            .iter()
            .map(|variant| declared_scalar_type(context, tag, variant, stack))
            .collect::<Result<Vec<_>, _>>()?;
        stack.pop();
        return Ok(TypeRef::Union {
            name: declared.to_owned(),
            variants,
        });
    }

    let Some(runtime) = scalar_runtime_type(tag) else {
        return Err(unsupported_type(context, declared));
    };
    Ok(TypeRef::Declared {
        name: declared.to_owned(),
        runtime: Box::new(runtime),
    })
}

fn subtype_type_ref(
    program: &str,
    relation: &str,
    column: &str,
    definitions: &TypeDefinitions,
    declared: &str,
    stack: &mut Vec<String>,
) -> Result<Option<TypeRef>, BuildError> {
    let Some(base_name) = definitions.subtypes.get(declared).cloned() else {
        return Ok(None);
    };

    let marker = format!("subtype:{declared}");
    if stack.iter().any(|entry| entry == &marker) {
        return Err(schema_error(
            program,
            format!(
                "relation `{relation}` column `{column}` uses recursive subtype type `{declared}`"
            ),
        ));
    }

    stack.push(marker);
    let base = if let Some(primitive) = primitive_type_name(&base_name) {
        Ok(primitive)
    } else if let Some(subtype) =
        subtype_type_ref(program, relation, column, definitions, &base_name, stack)?
    {
        Ok(subtype)
    } else {
        Err(schema_error(
            program,
            format!(
                "relation `{relation}` column `{column}` subtype `{declared}` has unsupported base type `{base_name}`"
            ),
        ))
    };
    stack.pop();

    Ok(Some(TypeRef::Subtype {
        name: declared.to_owned(),
        base: Box::new(base?),
    }))
}

fn record_type(
    context: &TypeContext<'_>,
    token: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    let Some(record) = context.types.records.get(token) else {
        return Err(schema_error(
            context.program,
            format!(
                "relation `{}` column `{}` references missing record `{token}`",
                context.relation, context.column
            ),
        ));
    };
    if record.types.len() != record.arity {
        return Err(schema_error(
            context.program,
            format!(
                "relation `{}` column `{}` record `{token}` arity mismatch: arity {}, types len {}",
                context.relation,
                context.column,
                record.arity,
                record.types.len()
            ),
        ));
    }

    if stack.iter().any(|entry| entry == token) {
        return Err(schema_error(
            context.program,
            format!(
                "relation `{}` column `{}` uses recursive non-list record type `{token}`",
                context.relation, context.column
            ),
        ));
    }
    if is_list_record(token, record, context.params) {
        stack.push(token.to_owned());
        let element = type_ref(context, &record.types[0], stack)?;
        stack.pop();
        return Ok(TypeRef::List(Box::new(element)));
    }

    stack.push(token.to_owned());
    let fields = record
        .types
        .iter()
        .map(|field| type_ref(context, field, stack))
        .collect::<Result<Vec<_>, _>>()?;
    stack.pop();
    Ok(TypeRef::Record(fields))
}

fn is_list_record(token: &str, record: &AstTypeRelation, params: &AstParams) -> bool {
    if record.arity != 2 || record.types.len() != 2 || record.types[1] != token {
        return false;
    }

    let record_name = token.trim_start_matches("r:");
    let Some(record_params) = params.records.get(record_name) else {
        return false;
    };
    record_params.arity == 2
        && record_params.params.len() == 2
        && record_params.params[0] == "head"
        && record_params.params[1] == "tail"
}

fn adt_type(
    context: &TypeContext<'_>,
    token: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    if stack.iter().any(|entry| entry == token) {
        return Ok(TypeRef::Reference {
            name: token.trim_start_matches("+:").to_owned(),
            runtime: ValueKind::Adt,
        });
    }
    let Some(adt) = context.types.adts.get(token) else {
        return Err(schema_error(
            context.program,
            format!(
                "relation `{}` column `{}` references missing ADT `{token}`",
                context.relation, context.column
            ),
        ));
    };

    stack.push(token.to_owned());
    let mut seen = BTreeSet::new();
    let variants = adt
        .branches
        .iter()
        .map(|branch| {
            if !seen.insert(branch.name.clone()) {
                return Err(schema_error(
                    context.program,
                    format!(
                        "relation `{}` column `{}` ADT `{token}` repeats variant `{}`",
                        context.relation, context.column, branch.name
                    ),
                ));
            }
            let fields = branch
                .types
                .iter()
                .map(|field| type_ref(context, field, stack))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((branch.name.clone(), fields))
        })
        .collect::<Result<Vec<_>, _>>()?;
    stack.pop();

    Ok(TypeRef::adt_with_enum_encoding(
        token.trim_start_matches("+:").to_owned(),
        variants,
        adt.is_enum,
    ))
}

fn declared_type_ref(
    context: &DeclaredTypeContext<'_>,
    declared: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    if let Some(primitive) = primitive_type_name(declared) {
        return Ok(primitive);
    }

    if let Some(subtype) = subtype_type_ref(
        context.program,
        context.relation,
        context.column,
        context.definitions,
        declared,
        stack,
    )? {
        return Ok(subtype);
    }

    if let Some(variants) = context.definitions.unions.get(declared) {
        let marker = format!("union:{declared}");
        if stack.iter().any(|entry| entry == &marker) {
            return Err(schema_error(
                context.program,
                format!(
                    "relation `{}` column `{}` uses recursive union type `{declared}`",
                    context.relation, context.column
                ),
            ));
        }

        stack.push(marker);
        let variants = variants
            .iter()
            .map(|variant| declared_type_ref(context, variant, stack))
            .collect::<Result<Vec<_>, _>>()?;
        stack.pop();
        return Ok(TypeRef::Union {
            name: declared.to_owned(),
            variants,
        });
    }

    if context.definitions.records.contains_key(declared) {
        return declared_record_type(context, declared, stack);
    }

    if context.definitions.adts.contains_key(declared) {
        return declared_adt_type(context, declared, stack);
    }

    Err(schema_error(
        context.program,
        format!(
            "relation `{}` column `{}` uses unsupported declared Souffle type `{declared}`",
            context.relation, context.column
        ),
    ))
}

fn declared_record_type(
    context: &DeclaredTypeContext<'_>,
    declared: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    let fields = context
        .definitions
        .records
        .get(declared)
        .expect("record presence checked by caller");
    if is_declared_list_record(declared, fields) {
        let marker = format!("record:{declared}");
        stack.push(marker);
        let element = declared_type_ref(context, &fields[0].ty, stack)?;
        stack.pop();
        return Ok(TypeRef::List(Box::new(element)));
    }

    let marker = format!("record:{declared}");
    if stack.iter().any(|entry| entry == &marker) {
        return Err(schema_error(
            context.program,
            format!(
                "relation `{}` column `{}` uses recursive non-list record type `{declared}`",
                context.relation, context.column
            ),
        ));
    }

    stack.push(marker);
    let fields = fields
        .iter()
        .map(|field| declared_type_ref(context, &field.ty, stack))
        .collect::<Result<Vec<_>, _>>()?;
    stack.pop();
    Ok(TypeRef::Record(fields))
}

fn is_declared_list_record(declared: &str, fields: &[DeclaredField]) -> bool {
    fields.len() == 2
        && fields[0].name == "head"
        && fields[1].name == "tail"
        && fields[1].ty == declared
}

fn declared_adt_type(
    context: &DeclaredTypeContext<'_>,
    declared: &str,
    stack: &mut Vec<String>,
) -> Result<TypeRef, BuildError> {
    let marker = format!("adt:{declared}");
    if stack.iter().any(|entry| entry == &marker) {
        return Ok(TypeRef::Reference {
            name: declared.to_owned(),
            runtime: ValueKind::Adt,
        });
    }
    let branches = context
        .definitions
        .adts
        .get(declared)
        .expect("ADT presence checked by caller");

    stack.push(marker);
    let mut seen = BTreeSet::new();
    let variants = branches
        .iter()
        .map(|branch| {
            if !seen.insert(branch.name.clone()) {
                return Err(schema_error(
                    context.program,
                    format!(
                        "relation `{}` column `{}` ADT `{declared}` repeats variant `{}`",
                        context.relation, context.column, branch.name
                    ),
                ));
            }
            let fields = branch
                .fields
                .iter()
                .map(|field| declared_type_ref(context, field, stack))
                .collect::<Result<Vec<_>, _>>()?;
            Ok((branch.name.clone(), fields))
        })
        .collect::<Result<Vec<_>, _>>()?;
    stack.pop();

    Ok(TypeRef::adt(declared.to_owned(), variants))
}

fn unsupported_type(context: &TypeContext<'_>, token: &str) -> BuildError {
    schema_error(
        context.program,
        format!(
            "relation `{}` column `{}` uses unsupported Souffle type token `{token}`",
            context.relation, context.column
        ),
    )
}

fn schema_error(program: &str, message: String) -> BuildError {
    BuildError::SchemaExtraction {
        program: program.to_owned(),
        message,
    }
}
