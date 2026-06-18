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
    /// Fixed-point (Q-format): the real value `x` is stored as the integer
    /// `round(x * 2^frac)` in a `bits`-wide integer, signed or unsigned. Surface
    /// syntax `sfix<bits>_<frac>` / `ufix<bits>_<frac>` (e.g. `sfix32_16`).
    /// `bits` is 8/16/32/64 so the store maps to a `<stdint.h>` integer;
    /// add/sub/compare are integer ops on the stored value, multiply is
    /// `(intN)(((int64_t)a * b) >> frac)`, and casts to/from int/float rescale.
    /// Lustre / Kind 2 view it as its underlying integer.
    Fixed { signed: bool, bits: u32, frac: u32 },
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

    pub fn is_fixed(&self) -> bool {
        matches!(self, Type::Fixed { .. })
    }

    /// The integer storage type backing a fixed-point type — `Int{N}` when
    /// signed, `Uint{N}` when unsigned. `None` for non-fixed types or an
    /// unsupported `bits` (only 8/16/32/64 are storable). The Q-format value
    /// lives in this integer, so fixed add/sub/compare reduce to integer ops on
    /// it and the C-Lite emitter declares variables with its `c_name`.
    pub fn fixed_storage(&self) -> Option<Type> {
        if let Type::Fixed { signed, bits, .. } = self {
            Some(match (signed, bits) {
                (true, 8) => Type::Int8,
                (true, 16) => Type::Int16,
                (true, 32) => Type::Int32,
                (true, 64) => Type::Int64,
                (false, 8) => Type::Uint8,
                (false, 16) => Type::Uint16,
                (false, 32) => Type::Uint32,
                (false, 64) => Type::Uint64,
                _ => return None,
            })
        } else {
            None
        }
    }

    /// Saturation bounds `[min, max]` of a fixed-point type's stored integer,
    /// as `i64`. `None` for non-fixed types. The simulator and the C-Lite
    /// emitter both clamp against these exact values, so saturating arithmetic
    /// is bit-identical. (A `uint64` fixed clamps its top at `i64::MAX` — a
    /// documented corner; saturation is exact for signed and for ≤32-bit.)
    pub fn fixed_sat_range(&self) -> Option<(i64, i64)> {
        if let Type::Fixed { signed, bits, .. } = self {
            Some(match (signed, bits) {
                (true, 8) => (i8::MIN as i64, i8::MAX as i64),
                (true, 16) => (i16::MIN as i64, i16::MAX as i64),
                (true, 32) => (i32::MIN as i64, i32::MAX as i64),
                (true, _) => (i64::MIN, i64::MAX),
                (false, 8) => (0, u8::MAX as i64),
                (false, 16) => (0, u16::MAX as i64),
                (false, 32) => (0, u32::MAX as i64),
                (false, _) => (0, i64::MAX),
            })
        } else {
            None
        }
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
            // Fixed-point proves over its stored integer value.
            Type::Fixed { .. } => "int".into(),
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
            // Fixed-point is stored in (and emitted as) its backing integer.
            Type::Fixed { .. } => self
                .fixed_storage()
                .map(|t| t.c_name())
                .unwrap_or_else(|| "int32_t".to_string()),
            Type::Array { elem, .. } => elem.c_name(),
            Type::Named { name } => name.clone(),
        }
    }
}
