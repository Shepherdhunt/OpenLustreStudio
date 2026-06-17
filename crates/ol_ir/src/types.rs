use serde::{Deserialize, Serialize};

/// Primitive and structured types in the OpenLustre profile.
///
/// Numeric widths are explicit so the C-Lite emitter can map them to
/// `<stdint.h>` typedefs without ambiguity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Type {
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Float32,
    Float64,
    /// A character (SCADE `char`). Stored as a byte; a string constant is an
    /// `Array { elem: Char, len }`. Lustre has no char, so it views as `int`.
    Char,
    /// Fixed-size array of `elem` with `len` elements.
    Array { elem: Box<Type>, len: u32 },
    /// Reference to a user-declared record or enum type. A struct variant —
    /// not a newtype — because `#[serde(tag = "kind")]` cannot serialize a
    /// tagged newtype wrapping a bare string.
    Named { name: String },
}

impl Type {
    pub fn named(name: impl Into<String>) -> Self {
        Type::Named { name: name.into() }
    }
}

impl Type {
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::Uint8
                | Type::Uint16
                | Type::Uint32
                | Type::Uint64
                | Type::Float32
                | Type::Float64
        )
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::Uint8
                | Type::Uint16
                | Type::Uint32
                | Type::Uint64
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float32 | Type::Float64)
    }

    pub fn is_bool(&self) -> bool {
        matches!(self, Type::Bool)
    }

    /// Canonical name used by emitters (Lustre and C-Lite agree on shape).
    pub fn lustre_name(&self) -> String {
        match self {
            Type::Bool => "bool".into(),
            Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 => "int".into(),
            Type::Uint8 | Type::Uint16 | Type::Uint32 | Type::Uint64 => "int".into(),
            Type::Float32 | Type::Float64 => "real".into(),
            // Lustre has no char; a char is viewed as a (small) integer.
            Type::Char => "int".into(),
            Type::Array { elem, len } => format!("{}^{}", elem.lustre_name(), len),
            Type::Named { name } => name.clone(),
        }
    }

    pub fn c_name(&self) -> String {
        match self {
            Type::Bool => "bool".into(),
            Type::Int8 => "int8_t".into(),
            Type::Int16 => "int16_t".into(),
            Type::Int32 => "int32_t".into(),
            Type::Int64 => "int64_t".into(),
            Type::Uint8 => "uint8_t".into(),
            Type::Uint16 => "uint16_t".into(),
            Type::Uint32 => "uint32_t".into(),
            Type::Uint64 => "uint64_t".into(),
            Type::Float32 => "float".into(),
            Type::Float64 => "double".into(),
            Type::Char => "char".into(),
            Type::Array { elem, .. } => elem.c_name(),
            Type::Named { name } => name.clone(),
        }
    }
}
