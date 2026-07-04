use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use strum::IntoStaticStr;

/// Runtime kind of a Souffle value after schema-guided decoding.
///
/// A [`ValueKind`] is the normalized runtime representation, not necessarily
/// the declared Souffle type. For example, a subtype such as
/// `.type Small <: number` still has the runtime kind [`ValueKind::Number`];
/// preserve the declared type with [`Value::typed`] when that identity matters.
///
/// # Example
///
/// ```
/// use souffle_rs::{Value, ValueKind};
///
/// let value = Value::typed("Small", Value::Number(7));
///
/// assert_eq!(value.kind(), ValueKind::Number);
/// assert_eq!(ValueKind::Symbol.as_str(), "symbol");
/// ```
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ValueKind {
    /// Signed Souffle `number`.
    Number,
    /// Unsigned Souffle `unsigned`.
    Unsigned,
    /// Souffle `float`.
    Float,
    /// Souffle `symbol`.
    Symbol,
    /// Composite record value.
    Record,
    /// Composite list value.
    List,
    /// Algebraic data type value.
    Adt,
    /// Nullary relation marker.
    Nullary,
}

impl ValueKind {
    /// Stable lowercase schema and diagnostic label for this runtime kind.
    pub fn as_str(self) -> &'static str {
        self.into()
    }
}

/// Rust-owned representation of a value supported by Souffle generated
/// relation APIs.
///
/// # Example
///
/// ```
/// use souffle_rs::{Value, ValueKind};
///
/// let payload = Value::Record(vec![
///     Value::Unsigned(7),
///     Value::List(vec![Value::Symbol("entry".into())]),
///     Value::Adt {
///         variant: "Some".into(),
///         fields: vec![Value::Number(1)],
///     },
/// ]);
///
/// assert_eq!(payload.kind(), ValueKind::Record);
///
/// let small = Value::typed("Small", Value::Number(7));
/// assert_eq!(small.kind(), ValueKind::Number);
/// assert_eq!(small.declared_type_name(), Some("Small"));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Signed Souffle `number`.
    Number(i64),
    /// Unsigned Souffle `unsigned`.
    Unsigned(u64),
    /// Souffle `float`, preserving the underlying IEEE-754 bits on serialization.
    Float(f64),
    /// Rust-owned Souffle `symbol`.
    Symbol(String),
    /// Souffle record fields in declared order.
    Record(Vec<Value>),
    /// Souffle list elements in declared order.
    List(Vec<Value>),
    /// Souffle algebraic data type value.
    Adt {
        /// Variant constructor name.
        variant: String,
        /// Variant fields in declared order.
        fields: Vec<Value>,
    },
    /// Value annotated with a declared Souffle type.
    ///
    /// Souffle subtypes and unions reuse their base runtime representation.
    /// This wrapper preserves the schema-visible declared type when Rust needs
    /// to distinguish `Small <: number` from a plain `number`, or a `Bucket`
    /// union column from one of its runtime-compatible variants.
    Typed {
        /// Declared Souffle type name.
        declared_type: String,
        /// Runtime value represented by that declared type.
        value: Box<Value>,
    },
    /// Value used for a nullary relation row.
    Nullary,
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        SerializableValue::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        SerializableValue::deserialize(deserializer)?
            .try_into()
            .map_err(D::Error::custom)
    }
}

impl Value {
    /// Annotate a runtime value with a declared Souffle type name.
    ///
    /// Use this for subtypes, unions, ADTs, and declared type aliases when the
    /// runtime kind alone would be ambiguous.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs::Value;
    ///
    /// let small = Value::typed("Small", Value::Number(3));
    ///
    /// assert_eq!(small.declared_type_name(), Some("Small"));
    /// assert_eq!(small.untyped(), &Value::Number(3));
    /// assert_eq!(small.into_untyped(), Value::Number(3));
    /// ```
    pub fn typed(declared_type: impl Into<String>, value: impl Into<Value>) -> Self {
        Self::Typed {
            declared_type: declared_type.into(),
            value: Box::new(value.into()),
        }
    }

    /// Declared Souffle type carried by this value, if one is present.
    pub fn declared_type_name(&self) -> Option<&str> {
        match self {
            Self::Typed { declared_type, .. } => Some(declared_type),
            _ => None,
        }
    }

    /// Runtime value without any declared-type wrapper.
    pub fn untyped(&self) -> &Value {
        match self {
            Self::Typed { value, .. } => value.untyped(),
            value => value,
        }
    }

    /// Consume this value and remove every declared-type wrapper.
    pub fn into_untyped(self) -> Value {
        match self {
            Self::Typed { value, .. } => value.into_untyped(),
            value => value,
        }
    }

    /// Runtime kind of this value.
    pub fn kind(&self) -> ValueKind {
        match self.untyped() {
            Self::Number(_) => ValueKind::Number,
            Self::Unsigned(_) => ValueKind::Unsigned,
            Self::Float(_) => ValueKind::Float,
            Self::Symbol(_) => ValueKind::Symbol,
            Self::Record(_) => ValueKind::Record,
            Self::List(_) => ValueKind::List,
            Self::Adt { .. } => ValueKind::Adt,
            Self::Typed { .. } => unreachable!("untyped values cannot be typed"),
            Self::Nullary => ValueKind::Nullary,
        }
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<u64> for Value {
    fn from(value: u64) -> Self {
        Self::Unsigned(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Symbol(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Symbol(value.to_owned())
    }
}

/// Rust-owned row of relation values.
///
/// # Example
///
/// ```
/// use souffle_rs::{Row, Value};
///
/// let row = Row::new([
///     Value::Number(1),
///     Value::Symbol("entry".into()),
/// ]);
///
/// assert_eq!(row.len(), 2);
/// assert!(!row.is_empty());
/// assert_eq!(row.values()[0], Value::Number(1));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    /// Create a row from already materialized values.
    pub fn new(values: impl Into<Vec<Value>>) -> Self {
        Self {
            values: values.into(),
        }
    }

    /// Values in relation column order.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Consume the row into its values.
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }

    /// Number of values in this row.
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether this row has no values.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl From<Vec<Value>> for Row {
    fn from(values: Vec<Value>) -> Self {
        Self::new(values)
    }
}

impl<const N: usize> From<[Value; N]> for Row {
    fn from(values: [Value; N]) -> Self {
        Self::new(values.to_vec())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SerializableValue {
    Number {
        value: i64,
    },
    Unsigned {
        value: u64,
    },
    Float {
        bits: String,
    },
    Symbol {
        value: String,
    },
    Record {
        fields: Vec<SerializableValue>,
    },
    List {
        elements: Vec<SerializableValue>,
    },
    Adt {
        variant: String,
        fields: Vec<SerializableValue>,
    },
    Typed {
        declared_type: String,
        value: Box<SerializableValue>,
    },
    Nullary,
}

impl From<&Value> for SerializableValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Number(value) => Self::Number { value: *value },
            Value::Unsigned(value) => Self::Unsigned { value: *value },
            Value::Float(value) => Self::Float {
                bits: format!("{:016x}", value.to_bits()),
            },
            Value::Symbol(value) => Self::Symbol {
                value: value.clone(),
            },
            Value::Record(fields) => Self::Record {
                fields: fields.iter().map(Self::from).collect(),
            },
            Value::List(elements) => Self::List {
                elements: elements.iter().map(Self::from).collect(),
            },
            Value::Adt { variant, fields } => Self::Adt {
                variant: variant.clone(),
                fields: fields.iter().map(Self::from).collect(),
            },
            Value::Typed {
                declared_type,
                value,
            } => Self::Typed {
                declared_type: declared_type.clone(),
                value: Box::new(Self::from(value.as_ref())),
            },
            Value::Nullary => Self::Nullary,
        }
    }
}

impl TryFrom<SerializableValue> for Value {
    type Error = String;

    fn try_from(value: SerializableValue) -> Result<Self, Self::Error> {
        Ok(match value {
            SerializableValue::Number { value } => Self::Number(value),
            SerializableValue::Unsigned { value } => Self::Unsigned(value),
            SerializableValue::Float { bits } => {
                let bits = parse_float_bits(&bits)?;
                Self::Float(f64::from_bits(bits))
            }
            SerializableValue::Symbol { value } => Self::Symbol(value),
            SerializableValue::Record { fields } => Self::Record(convert_values(fields, "record")?),
            SerializableValue::List { elements } => Self::List(convert_values(elements, "list")?),
            SerializableValue::Adt { variant, fields } => Self::Adt {
                variant,
                fields: convert_values(fields, "adt")?,
            },
            SerializableValue::Typed {
                declared_type,
                value,
            } => Self::typed(declared_type, Value::try_from(*value)?),
            SerializableValue::Nullary => Self::Nullary,
        })
    }
}

fn convert_values(values: Vec<SerializableValue>, context: &str) -> Result<Vec<Value>, String> {
    values
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("{context} value decode failed: {error}"))
}

fn parse_float_bits(bits: &str) -> Result<u64, String> {
    if bits.len() != 16 {
        return Err(format!(
            "float bits must be 16 hex digits, received {}",
            bits.len()
        ));
    }

    u64::from_str_radix(bits, 16).map_err(|source| format!("invalid float bits `{bits}`: {source}"))
}
