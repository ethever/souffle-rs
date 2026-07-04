use std::{
    collections::{BTreeMap, BTreeSet, btree_map},
    fmt,
};

use serde::{Deserialize, Serialize};
use strum::IntoStaticStr;

use crate::{
    SouffleError,
    value::{Value, ValueKind},
};

fn is_false(value: &bool) -> bool {
    !*value
}

/// Stable identifier for a relation within one schema bundle.
///
/// Relation ids come from generated or extracted Souffle metadata. They make
/// [`RelationHandle`] values robust against relation renames or stale handles
/// that happen to carry the same relation name.
///
/// # Example
///
/// ```
/// use souffle_rs::RelationId;
///
/// let id = RelationId::new(42);
///
/// assert_eq!(id.raw(), 42);
/// assert_eq!(id.to_string(), "42");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationId(u32);

impl RelationId {
    /// Create an id from generated metadata.
    pub fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Raw generated metadata id.
    pub fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for RelationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Stable runtime handle for one relation in a schema bundle.
///
/// A handle carries the generated relation id, relation name, relation role,
/// and load/read capabilities that were visible in schema metadata. It is the
/// dynamic counterpart to generated typed relation handles.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, RelationId, RelationKind, RelationSchema, TypeRef,
/// };
///
/// let input = RelationSchema::input(
///     RelationId::new(0),
///     "Input",
///     [AttributeSchema::new("id", TypeRef::Number)],
/// );
/// let handle = input.handle();
///
/// assert_eq!(handle.id(), RelationId::new(0));
/// assert_eq!(handle.name(), "Input");
/// assert_eq!(handle.kind(), RelationKind::Input);
/// assert!(handle.is_loadable());
/// assert!(!handle.is_printable());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelationHandle {
    id: RelationId,
    name: String,
    kind: RelationKind,
    loadable: bool,
    printable: bool,
}

impl RelationHandle {
    /// Create a handle from schema identity and relation capabilities.
    ///
    /// Build metadata and generated typed APIs usually create handles from
    /// `RelationSchema::handle`; manual construction is useful for generated
    /// constants and stale-handle diagnostics in tests.
    pub fn new(
        id: RelationId,
        name: impl Into<String>,
        kind: RelationKind,
        loadable: bool,
        printable: bool,
    ) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            loadable,
            printable,
        }
    }

    /// Generated relation id.
    pub fn id(&self) -> RelationId {
        self.id
    }

    /// Relation name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Relation role.
    pub fn kind(&self) -> RelationKind {
        self.kind
    }

    /// Whether values may be inserted into this relation.
    pub fn is_loadable(&self) -> bool {
        self.loadable
    }

    /// Whether this relation may be read as output.
    pub fn is_printable(&self) -> bool {
        self.printable
    }
}

/// Relation role exposed by Souffle metadata.
///
/// Roles describe how a relation is intended to cross the program boundary.
/// Input relations are loadable, output relations are printable, and
/// intermediate relations are part of the generated schema but not necessarily
/// usable through the public runtime API.
///
/// # Example
///
/// ```
/// use souffle_rs::{RelationId, RelationKind, RelationSchema};
///
/// let input = RelationSchema::new(
///     RelationId::new(0),
///     "Input",
///     RelationKind::Input,
///     Vec::new(),
/// );
///
/// assert!(input.is_loadable());
/// assert!(!input.is_printable());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, IntoStaticStr)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum RelationKind {
    /// Loadable input relation.
    Input,
    /// Printable output relation.
    Output,
    /// Intermediate relation visible through schema metadata.
    Intermediate,
}

/// Declared type of a relation attribute.
///
/// # Example
///
/// ```
/// use souffle_rs::TypeRef;
///
/// let payload = TypeRef::Record(vec![
///     TypeRef::Number,
///     TypeRef::List(Box::new(TypeRef::Symbol)),
/// ]);
/// assert_eq!(payload.display_name(), "record<number, list<symbol>>");
///
/// let expr = TypeRef::adt(
///     "Expr",
///     [
///         ("Const".into(), vec![TypeRef::Number]),
///         ("Name".into(), vec![TypeRef::Symbol]),
///     ],
/// );
/// assert_eq!(expr.display_name(), "Expr");
/// assert_eq!(expr.runtime_value_kinds(), vec![souffle_rs::ValueKind::Adt]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeRef {
    /// Souffle `number`.
    Number,
    /// Souffle `unsigned`.
    Unsigned,
    /// Souffle `float`.
    Float,
    /// Souffle `symbol`.
    Symbol,
    /// Nullary relation marker type.
    Nullary,
    /// Souffle record with field types in declared order.
    Record(Vec<TypeRef>),
    /// Souffle list with one element type.
    List(Box<TypeRef>),
    /// Souffle algebraic data type.
    Adt {
        /// Declared ADT type name.
        name: String,
        /// Variant field types keyed by variant name.
        variants: BTreeMap<String, Vec<TypeRef>>,
        /// Variant names in the order used by Souffle's encoded variant index.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        variant_order: Vec<String>,
        /// Whether Souffle encodes this ADT as a bare branch id because every
        /// variant is nullary.
        #[serde(default, skip_serializing_if = "is_false")]
        is_enum: bool,
    },
    /// Reference to a named schema type, used to preserve recursive ADTs
    /// without expanding the schema forever.
    Reference {
        /// Declared type name being referenced.
        name: String,
        /// Runtime value kind accepted by this named type.
        runtime: ValueKind,
    },
    /// Souffle subtype that reuses the base runtime representation.
    Subtype {
        /// Declared subtype name.
        name: String,
        /// Base type accepted by this subtype.
        base: Box<TypeRef>,
    },
    /// Souffle union type preserving each declared variant type.
    Union {
        /// Declared union type name.
        name: String,
        /// Types that are accepted by the union.
        variants: Vec<TypeRef>,
    },
    /// Declared named type with an explicit runtime representation.
    Declared {
        /// Declared type name.
        name: String,
        /// Runtime representation used for values of this type.
        runtime: Box<TypeRef>,
    },
}

impl TypeRef {
    /// Create an ADT type with the variant order used by Souffle's encoded
    /// variant index.
    ///
    /// If every variant has no fields, the type is treated as a Souffle enum
    /// ADT and [`TypeRef::is_enum_adt`] returns `true`.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::TypeRef;
    ///
    /// let color = TypeRef::adt(
    ///     "Color",
    ///     [
    ///         ("Red".to_owned(), vec![]),
    ///         ("Green".to_owned(), vec![]),
    ///     ],
    /// );
    ///
    /// assert!(color.is_enum_adt());
    /// assert_eq!(
    ///     color
    ///         .ordered_adt_variants()
    ///         .unwrap()
    ///         .into_iter()
    ///         .map(|(name, _)| name)
    ///         .collect::<Vec<_>>(),
    ///     ["Red", "Green"],
    /// );
    /// ```
    pub fn adt(
        name: impl Into<String>,
        variants: impl IntoIterator<Item = (String, Vec<TypeRef>)>,
    ) -> Self {
        let variants = variants.into_iter().collect::<Vec<_>>();
        let is_enum = variants.iter().all(|(_, fields)| fields.is_empty());
        Self::adt_with_enum_encoding(name, variants, is_enum)
    }

    /// Create an ADT type while preserving Souffle's enum encoding metadata.
    ///
    /// Souffle encodes an ADT with only nullary variants as the variant branch
    /// id itself, not as an ADT record. Schema extracted from Souffle metadata
    /// should pass that `enum` flag through here.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::TypeRef;
    ///
    /// let enum_adt = TypeRef::adt_with_enum_encoding(
    ///     "Color",
    ///     [("Red".to_owned(), vec![]), ("Green".to_owned(), vec![])],
    ///     true,
    /// );
    ///
    /// assert!(enum_adt.is_enum_adt());
    /// ```
    pub fn adt_with_enum_encoding(
        name: impl Into<String>,
        variants: impl IntoIterator<Item = (String, Vec<TypeRef>)>,
        is_enum: bool,
    ) -> Self {
        let mut variant_map = BTreeMap::new();
        let mut variant_order = Vec::new();
        for (variant, fields) in variants {
            if !variant_map.contains_key(&variant) {
                variant_order.push(variant.clone());
            }
            variant_map.insert(variant, fields);
        }

        Self::Adt {
            name: name.into(),
            variants: variant_map,
            variant_order,
            is_enum,
        }
    }

    /// Return whether this ADT uses Souffle's bare branch-id enum encoding.
    ///
    /// This is relevant for the embedded backend because Souffle emits enum ADT
    /// values as the branch id itself rather than an ADT record.
    pub fn is_enum_adt(&self) -> bool {
        let Self::Adt {
            variants, is_enum, ..
        } = self
        else {
            return false;
        };
        *is_enum || variants.values().all(Vec::is_empty)
    }

    /// Return ADT variants in Souffle's encoded variant-index order.
    ///
    /// `None` means this type is not an ADT, or the schema does not contain a
    /// complete variant order.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::TypeRef;
    ///
    /// let option = TypeRef::adt(
    ///     "OptionNumber",
    ///     [
    ///         ("None".to_owned(), vec![]),
    ///         ("Some".to_owned(), vec![TypeRef::Number]),
    ///     ],
    /// );
    /// let variants = option.ordered_adt_variants().unwrap();
    ///
    /// assert_eq!(variants[0].0, "None");
    /// assert_eq!(variants[1].1, &[TypeRef::Number]);
    /// ```
    pub fn ordered_adt_variants(&self) -> Option<Vec<(&str, &[TypeRef])>> {
        let Self::Adt {
            variants,
            variant_order,
            ..
        } = self
        else {
            return None;
        };
        if variant_order.len() != variants.len() {
            return None;
        }

        let mut ordered = Vec::with_capacity(variant_order.len());
        let mut seen = BTreeSet::new();
        for variant in variant_order {
            if !seen.insert(variant) {
                return None;
            }
            let fields = variants.get(variant)?;
            ordered.push((variant.as_str(), fields.as_slice()));
        }
        Some(ordered)
    }

    /// Human-readable type name for diagnostics and generated rustdoc.
    pub fn display_name(&self) -> String {
        match self {
            Self::Number => ValueKind::Number.as_str().to_owned(),
            Self::Unsigned => ValueKind::Unsigned.as_str().to_owned(),
            Self::Float => ValueKind::Float.as_str().to_owned(),
            Self::Symbol => ValueKind::Symbol.as_str().to_owned(),
            Self::Nullary => ValueKind::Nullary.as_str().to_owned(),
            Self::Record(fields) => {
                let fields = fields
                    .iter()
                    .map(Self::display_name)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("record<{fields}>")
            }
            Self::List(element) => format!("list<{}>", element.display_name()),
            Self::Adt { name, .. }
            | Self::Reference { name, .. }
            | Self::Subtype { name, .. }
            | Self::Union { name, .. }
            | Self::Declared { name, .. } => name.clone(),
        }
    }

    /// Normalized runtime value kinds that may represent this declared type.
    ///
    /// Primitive and composite types have exactly one runtime kind. Subtypes
    /// and declared wrappers reuse their base runtime representation. Unions
    /// preserve every schema-visible runtime kind instead of silently choosing
    /// one variant.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{TypeRef, ValueKind};
    ///
    /// let bucket = TypeRef::Union {
    ///     name: "Bucket".into(),
    ///     variants: vec![
    ///         TypeRef::Subtype {
    ///             name: "Small".into(),
    ///             base: Box::new(TypeRef::Number),
    ///         },
    ///         TypeRef::Declared {
    ///             name: "Label".into(),
    ///             runtime: Box::new(TypeRef::Symbol),
    ///         },
    ///     ],
    /// };
    ///
    /// assert_eq!(
    ///     bucket.runtime_value_kinds(),
    ///     vec![ValueKind::Number, ValueKind::Symbol],
    /// );
    /// ```
    pub fn runtime_value_kinds(&self) -> Vec<ValueKind> {
        let mut kinds = BTreeSet::new();
        self.collect_runtime_value_kinds(&mut kinds);
        kinds.into_iter().collect()
    }

    fn collect_runtime_value_kinds(&self, kinds: &mut BTreeSet<ValueKind>) {
        match self {
            Self::Number => {
                kinds.insert(ValueKind::Number);
            }
            Self::Unsigned => {
                kinds.insert(ValueKind::Unsigned);
            }
            Self::Float => {
                kinds.insert(ValueKind::Float);
            }
            Self::Symbol => {
                kinds.insert(ValueKind::Symbol);
            }
            Self::Nullary => {
                kinds.insert(ValueKind::Nullary);
            }
            Self::Record(_) => {
                kinds.insert(ValueKind::Record);
            }
            Self::List(_) => {
                kinds.insert(ValueKind::List);
            }
            Self::Adt { .. } => {
                kinds.insert(ValueKind::Adt);
            }
            Self::Reference { runtime, .. } => {
                kinds.insert(*runtime);
            }
            Self::Subtype { base, .. } | Self::Declared { runtime: base, .. } => {
                base.collect_runtime_value_kinds(kinds);
            }
            Self::Union { variants, .. } => {
                for variant in variants {
                    variant.collect_runtime_value_kinds(kinds);
                }
            }
        }
    }

    pub(crate) fn accepts_value(&self, value: &Value) -> TypeCheck {
        let definitions = self.named_type_definitions();
        self.accepts_value_with_definitions(value, &definitions)
    }

    pub(crate) fn named_type_definitions(&self) -> BTreeMap<String, TypeRef> {
        let mut definitions = BTreeMap::new();
        self.collect_named_type_definitions(&mut definitions);
        definitions
    }

    pub(crate) fn collect_named_type_definitions(
        &self,
        definitions: &mut BTreeMap<String, TypeRef>,
    ) {
        match self {
            Self::Record(fields) => {
                for field in fields {
                    field.collect_named_type_definitions(definitions);
                }
            }
            Self::List(element) => {
                element.collect_named_type_definitions(definitions);
            }
            Self::Adt { name, variants, .. } => {
                if definitions.insert(name.clone(), self.clone()).is_some() {
                    return;
                }
                for fields in variants.values() {
                    for field in fields {
                        field.collect_named_type_definitions(definitions);
                    }
                }
            }
            Self::Subtype { base, .. } | Self::Declared { runtime: base, .. } => {
                base.collect_named_type_definitions(definitions);
            }
            Self::Union { variants, .. } => {
                for variant in variants {
                    variant.collect_named_type_definitions(definitions);
                }
            }
            Self::Number
            | Self::Unsigned
            | Self::Float
            | Self::Symbol
            | Self::Nullary
            | Self::Reference { .. } => {}
        }
    }

    fn validate_schema(
        &self,
        relation: &str,
        path: &str,
        definitions: &BTreeMap<String, TypeRef>,
        visiting: &mut BTreeSet<String>,
    ) -> Result<(), SouffleError> {
        match self {
            Self::Number | Self::Unsigned | Self::Float | Self::Symbol | Self::Nullary => Ok(()),
            Self::Record(fields) => {
                for (index, field) in fields.iter().enumerate() {
                    field.validate_schema(
                        relation,
                        &format!("{path}.{index}"),
                        definitions,
                        visiting,
                    )?;
                }
                Ok(())
            }
            Self::List(element) => {
                element.validate_schema(relation, &format!("{path}[]"), definitions, visiting)
            }
            Self::Adt {
                name,
                variants,
                variant_order,
                is_enum,
            } => {
                if variants.is_empty() {
                    return Err(schema_validation_error(
                        relation,
                        path,
                        format!("ADT `{name}` has no variants"),
                    ));
                }
                if variant_order.len() != variants.len() {
                    return Err(schema_validation_error(
                        relation,
                        path,
                        format!(
                            "ADT `{name}` variant_order has {} entries but variants has {}",
                            variant_order.len(),
                            variants.len()
                        ),
                    ));
                }
                if *is_enum {
                    for (variant, fields) in variants {
                        if !fields.is_empty() {
                            return Err(schema_validation_error(
                                relation,
                                path,
                                format!(
                                    "ADT `{name}` is marked enum but variant `{variant}` has {} fields",
                                    fields.len()
                                ),
                            ));
                        }
                    }
                }

                let mut seen = BTreeSet::new();
                for variant in variant_order {
                    if !seen.insert(variant) {
                        return Err(schema_validation_error(
                            relation,
                            path,
                            format!("ADT `{name}` variant_order repeats variant `{variant}`"),
                        ));
                    }
                    if !variants.contains_key(variant) {
                        return Err(schema_validation_error(
                            relation,
                            path,
                            format!(
                                "ADT `{name}` variant_order references missing variant `{variant}`"
                            ),
                        ));
                    }
                }

                if !visiting.insert(name.clone()) {
                    return Ok(());
                }
                for (variant, fields) in variants {
                    for (index, field) in fields.iter().enumerate() {
                        field.validate_schema(
                            relation,
                            &format!("{path}.{variant}.{index}"),
                            definitions,
                            visiting,
                        )?;
                    }
                }
                visiting.remove(name);
                Ok(())
            }
            Self::Reference { name, .. } => {
                let Some(resolved) = definitions.get(name) else {
                    return Err(schema_validation_error(
                        relation,
                        path,
                        format!("reference to unknown type `{name}`"),
                    ));
                };
                if !visiting.insert(name.clone()) {
                    return Ok(());
                }
                let result = resolved.validate_schema(relation, path, definitions, visiting);
                visiting.remove(name);
                result
            }
            Self::Subtype { base, .. } | Self::Declared { runtime: base, .. } => {
                base.validate_schema(relation, path, definitions, visiting)
            }
            Self::Union { name, variants } => {
                if variants.is_empty() {
                    return Err(schema_validation_error(
                        relation,
                        path,
                        format!("union `{name}` has no variants"),
                    ));
                }
                for (index, variant) in variants.iter().enumerate() {
                    variant.validate_schema(
                        relation,
                        &format!("{path}|{index}"),
                        definitions,
                        visiting,
                    )?;
                }
                Ok(())
            }
        }
    }

    pub(crate) fn accepts_value_with_definitions(
        &self,
        value: &Value,
        definitions: &BTreeMap<String, TypeRef>,
    ) -> TypeCheck {
        let untyped_value = value.untyped();
        match self {
            Self::Number => TypeCheck::kind(value, ValueKind::Number),
            Self::Unsigned => TypeCheck::kind(value, ValueKind::Unsigned),
            Self::Float => TypeCheck::kind(value, ValueKind::Float),
            Self::Symbol => TypeCheck::kind(value, ValueKind::Symbol),
            Self::Nullary => TypeCheck::kind(value, ValueKind::Nullary),
            Self::Record(fields) => match untyped_value {
                Value::Record(values) if values.len() == fields.len() => TypeCheck::all(
                    fields
                        .iter()
                        .zip(values)
                        .map(|(ty, value)| ty.accepts_value_with_definitions(value, definitions)),
                ),
                Value::Record(values) => TypeCheck::Mismatch {
                    expected: self.display_name(),
                    actual: format!("record<{} fields>", values.len()),
                },
                _ => TypeCheck::Mismatch {
                    expected: self.display_name(),
                    actual: value.kind().as_str().to_owned(),
                },
            },
            Self::List(element) => match untyped_value {
                Value::List(values) => TypeCheck::all(
                    values
                        .iter()
                        .map(|value| element.accepts_value_with_definitions(value, definitions)),
                ),
                _ => TypeCheck::Mismatch {
                    expected: self.display_name(),
                    actual: value.kind().as_str().to_owned(),
                },
            },
            Self::Adt { name, variants, .. } => {
                match typed_value_for_named_type(value, name) {
                    TypeCheck::Ok => {}
                    mismatch => return mismatch,
                }
                match untyped_value {
                    Value::Adt { variant, fields } => match variants.get(variant) {
                        Some(expected_fields) if expected_fields.len() == fields.len() => {
                            TypeCheck::all(expected_fields.iter().zip(fields).map(|(ty, value)| {
                                ty.accepts_value_with_definitions(value, definitions)
                            }))
                        }
                        Some(expected_fields) => TypeCheck::Mismatch {
                            expected: format!(
                                "{}::{variant}<{} fields>",
                                self.display_name(),
                                expected_fields.len()
                            ),
                            actual: format!("adt::{variant}<{} fields>", fields.len()),
                        },
                        None => TypeCheck::AdtVariantMismatch {
                            variant: variant.clone(),
                        },
                    },
                    _ => TypeCheck::Mismatch {
                        expected: self.display_name(),
                        actual: value.kind().as_str().to_owned(),
                    },
                }
            }
            Self::Reference { name, runtime } => definitions.get(name).map_or_else(
                || TypeCheck::kind(value, *runtime),
                |resolved| resolved.accepts_value_with_definitions(value, definitions),
            ),
            Self::Subtype { name, base }
            | Self::Declared {
                name,
                runtime: base,
            } => match typed_value_for_named_type(value, name) {
                TypeCheck::Ok => base.accepts_value_with_definitions(untyped_value, definitions),
                mismatch => mismatch,
            },
            Self::Union { name, variants } => {
                if let Some(declared_type) = value.declared_type_name() {
                    if declared_type == name {
                        return union_accepts_value(variants, untyped_value, definitions)
                            .unwrap_or_else(|| TypeCheck::Mismatch {
                                expected: self.display_name(),
                                actual: untyped_value.kind().as_str().to_owned(),
                            });
                    }

                    let Some(variant) = variants.iter().find(|variant| {
                        type_ref_declared_name(variant, definitions) == Some(declared_type)
                    }) else {
                        return TypeCheck::Mismatch {
                            expected: self.display_name(),
                            actual: declared_type.to_owned(),
                        };
                    };
                    return variant.accepts_value_with_definitions(untyped_value, definitions);
                }

                union_accepts_value(variants, value, definitions).unwrap_or_else(|| {
                    TypeCheck::Mismatch {
                        expected: self.display_name(),
                        actual: value.kind().as_str().to_owned(),
                    }
                })
            }
        }
    }
}

fn typed_value_for_named_type(value: &Value, expected: &str) -> TypeCheck {
    match value.declared_type_name() {
        Some(actual) if actual != expected => TypeCheck::Mismatch {
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        },
        _ => TypeCheck::Ok,
    }
}

fn union_accepts_value(
    variants: &[TypeRef],
    value: &Value,
    definitions: &BTreeMap<String, TypeRef>,
) -> Option<TypeCheck> {
    let mut last_mismatch = None;
    for variant in variants {
        match variant.accepts_value_with_definitions(value, definitions) {
            TypeCheck::Ok => return Some(TypeCheck::Ok),
            TypeCheck::AdtVariantMismatch { variant } => {
                last_mismatch = Some(TypeCheck::AdtVariantMismatch { variant });
            }
            TypeCheck::Mismatch { expected, actual } => {
                last_mismatch = Some(TypeCheck::Mismatch { expected, actual });
            }
        }
    }

    last_mismatch
}

fn type_ref_declared_name<'a>(
    ty: &'a TypeRef,
    definitions: &'a BTreeMap<String, TypeRef>,
) -> Option<&'a str> {
    match ty {
        TypeRef::Adt { name, .. }
        | TypeRef::Reference { name, .. }
        | TypeRef::Subtype { name, .. }
        | TypeRef::Union { name, .. }
        | TypeRef::Declared { name, .. } => Some(name),
        TypeRef::Record(_)
        | TypeRef::List(_)
        | TypeRef::Number
        | TypeRef::Unsigned
        | TypeRef::Float
        | TypeRef::Symbol
        | TypeRef::Nullary => definitions
            .iter()
            .find_map(|(name, definition)| (definition == ty).then_some(name.as_str())),
    }
}

pub(crate) enum TypeCheck {
    Ok,
    Mismatch { expected: String, actual: String },
    AdtVariantMismatch { variant: String },
}

impl TypeCheck {
    fn kind(value: &Value, expected: ValueKind) -> Self {
        let actual = value.kind();
        if actual == expected {
            Self::Ok
        } else {
            Self::Mismatch {
                expected: expected.as_str().to_owned(),
                actual: actual.as_str().to_owned(),
            }
        }
    }

    fn all(checks: impl IntoIterator<Item = Self>) -> Self {
        for check in checks {
            if !matches!(check, Self::Ok) {
                return check;
            }
        }

        Self::Ok
    }
}

fn schema_validation_error(
    relation: impl Into<String>,
    path: impl Into<String>,
    message: impl Into<String>,
) -> SouffleError {
    SouffleError::SchemaValidation {
        relation: relation.into(),
        path: path.into(),
        message: message.into(),
    }
}

/// One relation column with Souffle's declared type preserved.
///
/// # Example
///
/// ```
/// use souffle_rs::{AttributeSchema, TypeRef, ValueKind};
///
/// let column = AttributeSchema::new(
///     "bucket",
///     TypeRef::Subtype {
///         name: "Small".into(),
///         base: Box::new(TypeRef::Number),
///     },
/// );
///
/// assert_eq!(column.name(), "bucket");
/// assert_eq!(column.runtime_types(), vec![ValueKind::Number]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributeSchema {
    name: String,
    declared_type: TypeRef,
    #[serde(default)]
    runtime_types: Vec<ValueKind>,
}

impl AttributeSchema {
    /// Create an attribute schema.
    pub fn new(name: impl Into<String>, declared_type: TypeRef) -> Self {
        let runtime_types = declared_type.runtime_value_kinds();
        Self {
            name: name.into(),
            declared_type,
            runtime_types,
        }
    }

    /// Souffle attribute name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Declared Souffle type.
    pub fn declared_type(&self) -> &TypeRef {
        &self.declared_type
    }

    /// Normalized runtime value kinds accepted by this attribute.
    ///
    /// The value is serialized into schema artifacts. If an older artifact does
    /// not contain it, the accessor recomputes the kinds from the declared
    /// Souffle type.
    pub fn runtime_types(&self) -> Vec<ValueKind> {
        if self.runtime_types.is_empty() {
            self.declared_type.runtime_value_kinds()
        } else {
            self.runtime_types.clone()
        }
    }
}

/// Schema for one Souffle relation.
///
/// Relation schemas validate row arity, attribute names, declared types, and
/// metadata required for ADT decoding before a backend accepts user rows.
///
/// # Example
///
/// ```
/// use std::collections::BTreeMap;
///
/// use souffle_rs::{AttributeSchema, RelationId, RelationSchema, TypeRef};
///
/// let mut variants = BTreeMap::new();
/// variants.insert("Some".to_owned(), vec![TypeRef::Number]);
///
/// let relation = RelationSchema::input(
///     RelationId::new(0),
///     "Input",
///     [AttributeSchema::new(
///         "value",
///         TypeRef::Adt {
///             name: "OptionNumber".into(),
///             variants,
///             variant_order: vec![],
///             is_enum: false,
///         },
///     )],
/// );
///
/// assert!(relation.validate().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationSchema {
    id: RelationId,
    name: String,
    kind: RelationKind,
    attributes: Vec<AttributeSchema>,
    loadable: bool,
    printable: bool,
}

impl RelationSchema {
    /// Create a relation schema from generated or extracted metadata.
    pub fn new(
        id: RelationId,
        name: impl Into<String>,
        kind: RelationKind,
        attributes: impl Into<Vec<AttributeSchema>>,
    ) -> Self {
        let loadable = kind == RelationKind::Input;
        let printable = kind == RelationKind::Output;
        Self {
            id,
            name: name.into(),
            kind,
            attributes: attributes.into(),
            loadable,
            printable,
        }
    }

    /// Create a loadable input relation schema.
    pub fn input(
        id: RelationId,
        name: impl Into<String>,
        attributes: impl Into<Vec<AttributeSchema>>,
    ) -> Self {
        Self::new(id, name, RelationKind::Input, attributes)
    }

    /// Create a printable output relation schema.
    pub fn output(
        id: RelationId,
        name: impl Into<String>,
        attributes: impl Into<Vec<AttributeSchema>>,
    ) -> Self {
        Self::new(id, name, RelationKind::Output, attributes)
    }

    /// Create an intermediate relation schema.
    pub fn intermediate(
        id: RelationId,
        name: impl Into<String>,
        attributes: impl Into<Vec<AttributeSchema>>,
    ) -> Self {
        Self::new(id, name, RelationKind::Intermediate, attributes)
    }

    /// Override Souffle loadable metadata.
    pub fn with_loadable(mut self, loadable: bool) -> Self {
        self.loadable = loadable;
        self
    }

    /// Override Souffle printable metadata.
    pub fn with_printable(mut self, printable: bool) -> Self {
        self.printable = printable;
        self
    }

    /// Generated relation id.
    pub fn id(&self) -> RelationId {
        self.id
    }

    /// Relation name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Relation role.
    pub fn kind(&self) -> RelationKind {
        self.kind
    }

    /// Relation arity.
    pub fn arity(&self) -> usize {
        self.attributes.len()
    }

    /// Whether the relation has zero attributes.
    pub fn is_nullary(&self) -> bool {
        self.attributes.is_empty()
    }

    /// Whether values may be inserted into this relation.
    pub fn is_loadable(&self) -> bool {
        self.loadable
    }

    /// Whether this relation may be read as output.
    pub fn is_printable(&self) -> bool {
        self.printable
    }

    /// Relation attributes in column order.
    pub fn attributes(&self) -> &[AttributeSchema] {
        &self.attributes
    }

    /// Validate that the relation schema is internally consistent.
    pub fn validate(&self) -> Result<(), SouffleError> {
        let mut attribute_names = BTreeSet::new();
        let mut definitions = BTreeMap::new();
        for attribute in &self.attributes {
            if !attribute_names.insert(attribute.name()) {
                return Err(schema_validation_error(
                    &self.name,
                    attribute.name(),
                    format!("duplicate attribute name `{}`", attribute.name()),
                ));
            }
            attribute
                .declared_type()
                .collect_named_type_definitions(&mut definitions);
        }

        for attribute in &self.attributes {
            attribute.declared_type().validate_schema(
                &self.name,
                attribute.name(),
                &definitions,
                &mut BTreeSet::new(),
            )?;
        }
        Ok(())
    }

    /// Stable runtime handle for this relation.
    pub fn handle(&self) -> RelationHandle {
        RelationHandle::new(
            self.id,
            self.name.clone(),
            self.kind,
            self.loadable,
            self.printable,
        )
    }
}

/// Complete schema inventory for one generated program.
///
/// # Example
///
/// ```
/// use souffle_rs::{
///     AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef,
/// };
///
/// let schema: RelationBundle = [
///     RelationSchema::input(
///         RelationId::new(0),
///         "Input",
///         [AttributeSchema::new("value", TypeRef::Unsigned)],
///     ),
///     RelationSchema::output(
///         RelationId::new(1),
///         "Output",
///         [AttributeSchema::new("value", TypeRef::Unsigned)],
///     ),
/// ]
/// .into_iter()
/// .collect();
///
/// assert_eq!(schema.len(), 2);
/// assert!(schema.get("Input").unwrap().is_loadable());
/// assert!(schema.get("Output").unwrap().is_printable());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RelationBundle {
    relations: BTreeMap<String, RelationSchema>,
}

impl RelationBundle {
    /// Create an empty relation bundle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Deserialize and validate a relation bundle from JSON.
    ///
    /// This is the runtime counterpart for schema JSON emitted by
    /// `souffle-rs-build` generated typed APIs. It keeps JSON parsing behind
    /// the `souffle-rs` public API, so crates using generated APIs do not need
    /// to depend on `serde_json` directly.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef,
    /// };
    ///
    /// let schema = RelationBundle::from_json_str(
    ///     r#"{"Input":{"id":0,"name":"Input","kind":"input","attributes":[{"name":"id","declared_type":"number","runtime_types":["number"]}],"loadable":true,"printable":false}}"#,
    /// )
    /// .unwrap();
    ///
    /// assert_eq!(schema.get("Input").unwrap().id(), RelationId::new(0));
    /// assert_eq!(
    ///     schema.get("Input").unwrap().attributes(),
    ///     &[AttributeSchema::new("id", TypeRef::Number)]
    /// );
    /// ```
    pub fn from_json_str(json: &str) -> Result<Self, SouffleError> {
        let schema = serde_json::from_str::<Self>(json).map_err(|source| {
            SouffleError::ArtifactDecodeFailed {
                artifact: "relation schema JSON".to_owned(),
                message: source.to_string(),
            }
        })?;
        schema.validate()?;
        Ok(schema)
    }

    /// Add or replace one relation schema.
    pub fn insert(&mut self, relation: RelationSchema) -> Option<RelationSchema> {
        self.relations.insert(relation.name().to_owned(), relation)
    }

    /// Return a relation schema by name.
    pub fn get(&self, relation: &str) -> Option<&RelationSchema> {
        self.relations.get(relation)
    }

    /// Number of relations in the bundle.
    pub fn len(&self) -> usize {
        self.relations.len()
    }

    /// Whether the bundle has no relations.
    pub fn is_empty(&self) -> bool {
        self.relations.is_empty()
    }

    /// Iterate relation schemas by relation name.
    pub fn iter(&self) -> btree_map::Values<'_, String, RelationSchema> {
        self.relations.values()
    }

    /// Validate all relation schemas and bundle-level relation ids.
    pub fn validate(&self) -> Result<(), SouffleError> {
        let mut relation_ids = BTreeMap::new();
        for relation in self.relations.values() {
            if relation.name().is_empty() {
                return Err(schema_validation_error(
                    relation.name(),
                    "<relation>",
                    "relation name is empty",
                ));
            }
            if let Some(previous) = relation_ids.insert(relation.id(), relation.name()) {
                return Err(schema_validation_error(
                    relation.name(),
                    "<relation>",
                    format!(
                        "relation id {} is also used by relation `{previous}`",
                        relation.id()
                    ),
                ));
            }
            relation.validate()?;
        }
        Ok(())
    }

    /// Iterate stable handles for all relations by relation name.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::{
    ///     AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef,
    /// };
    ///
    /// let schema: RelationBundle = [
    ///     RelationSchema::input(
    ///         RelationId::new(0),
    ///         "Input",
    ///         [AttributeSchema::new("id", TypeRef::Number)],
    ///     ),
    ///     RelationSchema::output(
    ///         RelationId::new(1),
    ///         "Output",
    ///         [AttributeSchema::new("id", TypeRef::Number)],
    ///     ),
    /// ]
    /// .into_iter()
    /// .collect();
    ///
    /// let handles = schema.handles().map(|handle| handle.name().to_owned()).collect::<Vec<_>>();
    /// assert_eq!(handles, ["Input", "Output"]);
    /// ```
    pub fn handles(&self) -> impl Iterator<Item = RelationHandle> + '_ {
        self.relations.values().map(RelationSchema::handle)
    }
}

impl FromIterator<RelationSchema> for RelationBundle {
    fn from_iter<T: IntoIterator<Item = RelationSchema>>(iter: T) -> Self {
        let mut bundle = Self::new();
        for relation in iter {
            bundle.insert(relation);
        }
        bundle
    }
}
