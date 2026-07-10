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
    /// A type variable (`'T`) of a GENERIC operator template. Exists only
    /// inside a template's ports/locals; monomorphization substitutes a
    /// concrete type per call site before any backend runs.
    Var { name: String },
    /// An array whose length is a SIZE parameter (`int32[N]`) of a generic
    /// template. Monomorphization replaces it with a concrete `Array`.
    ArrayVar { elem: Box<Type>, len_param: String },
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
            Type::Var { name } => format!("'{name}"),
            Type::ArrayVar { elem, len_param } => {
                format!("{}^{len_param}", elem.lustre_name())
            }
        }
    }

    /// Does this type mention any generic parameter (type or size)?
    pub fn is_generic(&self) -> bool {
        match self {
            Type::Var { .. } | Type::ArrayVar { .. } => true,
            Type::Array { elem, .. } => elem.is_generic(),
            _ => false,
        }
    }

    /// Substitute generic parameters: type variables from `types`, array
    /// size parameters from `sizes`. Unbound parameters stay in place (the
    /// monomorphizer reports them).
    pub fn substitute(
        &self,
        types: &std::collections::BTreeMap<String, Type>,
        sizes: &std::collections::BTreeMap<String, u32>,
    ) -> Type {
        match self {
            Type::Var { name } => types.get(name).cloned().unwrap_or_else(|| self.clone()),
            Type::ArrayVar { elem, len_param } => {
                let elem = Box::new(elem.substitute(types, sizes));
                match sizes.get(len_param) {
                    Some(&len) => Type::Array { elem, len },
                    None => Type::ArrayVar { elem, len_param: len_param.clone() },
                }
            }
            Type::Array { elem, len } => Type::Array {
                elem: Box::new(elem.substitute(types, sizes)),
                len: *len,
            },
            other => other.clone(),
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
            // Generic parameters never reach codegen: monomorphization
            // replaced them, or the typechecker refused the project.
            Type::Var { name } => format!("/*'{name}*/int32_t"),
            Type::ArrayVar { elem, .. } => elem.c_name(),
        }
    }
}

/// Constraint on a generic TYPE parameter — which concrete types may bind
/// it. Mirrors SCADE's `numeric` / `integer` / `float` genericity classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TypeConstraint {
    #[default]
    Any,
    Numeric,
    Integer,
    Float,
}

impl TypeConstraint {
    pub fn admits(&self, ty: &Type) -> bool {
        match self {
            TypeConstraint::Any => true,
            TypeConstraint::Numeric => ty.is_numeric(),
            TypeConstraint::Integer => ty.is_integer(),
            TypeConstraint::Float => ty.is_float(),
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            TypeConstraint::Any => "any",
            TypeConstraint::Numeric => "numeric",
            TypeConstraint::Integer => "integer",
            TypeConstraint::Float => "float",
        }
    }
}

/// One generic parameter of an operator template: a type variable (`'T`,
/// optionally constrained) or an array size (`N`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GenericParam {
    Type {
        name: String,
        #[serde(default)]
        constraint: TypeConstraint,
    },
    Size {
        name: String,
    },
}

impl GenericParam {
    pub fn name(&self) -> &str {
        match self {
            GenericParam::Type { name, .. } | GenericParam::Size { name } => name,
        }
    }
}
