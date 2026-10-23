//! Typed simulation inputs. JSON objects/arrays are convenient in Studio;
//! `{field=value;...}` / `[value;...]` remain the CSV-safe trace syntax.
//! Serialized IR type tags are deliberately independent of these UI names.

use std::collections::BTreeMap;

use ol_ir::{Project, Type, TypeBody};
use serde_json::{json, Value as Json};

use crate::Value;

const MAX_DEPTH: usize = 64;
const MAX_SCHEMA_NODES: usize = 10_000;
const MAX_PLACEHOLDER_VALUES: usize = 10_000;

fn depth_ok(depth: usize, path: &str) -> Result<(), String> {
    if depth > MAX_DEPTH {
        Err(format!("input `{path}`: type nesting exceeds {MAX_DEPTH}; recursive or deeper input types are unsupported"))
    } else {
        Ok(())
    }
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::Int8 => "int8".into(),
        Type::Int16 => "int16".into(),
        Type::Int32 => "int32".into(),
        Type::Int64 => "int64".into(),
        Type::Uint8 => "uint8".into(),
        Type::Uint16 => "uint16".into(),
        Type::Uint32 => "uint32".into(),
        Type::Uint64 => "uint64".into(),
        Type::Float32 => "float32".into(),
        Type::Float64 => "float64".into(),
        Type::Char => "char".into(),
        Type::Array { elem, len } => format!("{}[{len}]", type_name(elem)),
        Type::Named { name } => name.clone(),
    }
}

fn named<'a>(name: &str, project: &'a Project, path: &str) -> Result<&'a TypeBody, String> {
    project.packages.iter().flat_map(|p| &p.types).find(|t| t.name() == name)
        .map(|t| &t.body)
        .ok_or_else(|| format!("input `{path}`: named type `{name}` is not defined"))
}

/// Bound the engine's initialization shape before it allocates arrays or
/// nested structs. This checks all signal roles without assigning enum input
/// placeholders or changing the model's runtime initialization semantics.
pub fn simulation_type_size(ty: &Type, project: &Project) -> Result<usize, String> {
    shape_size(ty, project, &type_name(ty), 0)
}

fn shape_size(ty: &Type, project: &Project, path: &str, depth: usize) -> Result<usize, String> {
    depth_ok(depth, path)?;
    let too_large = || format!(
        "simulation type `{path}`: initialized struct/array shape exceeds {MAX_PLACEHOLDER_VALUES} values; this input, local or output type is too large for interactive simulation"
    );
    let size = match ty {
        Type::Array { elem, len } => {
            let elem_size = shape_size(elem, project, &format!("{path}[]"), depth + 1)?;
            elem_size.checked_mul(*len as usize).and_then(|s| s.checked_add(1)).ok_or_else(too_large)?
        }
        Type::Named { name } => match named(name, project, path)? {
            TypeBody::Enum(_) => 1,
            TypeBody::Alias { target, .. } => shape_size(target, project, path, depth + 1)?,
            TypeBody::Record { fields, .. } => {
                let mut size = 1usize;
                for field in fields {
                    size = size.checked_add(shape_size(&field.ty, project, &format!("{path}.{}", field.name), depth + 1)?)
                        .filter(|s| *s <= MAX_PLACEHOLDER_VALUES).ok_or_else(too_large)?;
                }
                size
            }
        },
        _ => 1,
    };
    if size > MAX_PLACEHOLDER_VALUES {
        return Err(too_large());
    }
    Ok(size)
}

/// The declared input shape, including named struct fields, alias targets,
/// array elements and enum variants. This is presentation metadata, not IR.
pub fn input_schema(ty: &Type, project: &Project) -> Result<Json, String> {
    let mut budget = MAX_SCHEMA_NODES;
    schema(ty, project, &type_name(ty), 0, &mut budget)
}

fn schema(ty: &Type, project: &Project, path: &str, depth: usize, budget: &mut usize) -> Result<Json, String> {
    depth_ok(depth, path)?;
    *budget = budget.checked_sub(1).ok_or_else(|| format!(
        "input `{path}`: type inspection exceeds {MAX_SCHEMA_NODES} schema nodes; deeply repeated struct types are unsupported in the input inspector"
    ))?;
    let label = type_name(ty);
    Ok(match ty {
        Type::Array { elem, len } => json!({
            "kind": "array", "type": label, "len": len,
            "elem": schema(elem, project, &format!("{path}[]"), depth + 1, budget)?,
        }),
        Type::Named { name } => match named(name, project, path)? {
            TypeBody::Enum(e) => json!({"kind": "enum", "type": label, "name": name, "variants": e.variants}),
            TypeBody::Alias { target, .. } => json!({
                "kind": "alias", "type": label, "name": name,
                "target": schema(target, project, path, depth + 1, budget)?,
            }),
            TypeBody::Record { fields, .. } => {
                let fs: Result<Vec<_>, String> = fields.iter().map(|f| Ok(json!({
                    "name": f.name, "type": type_name(&f.ty),
                    "schema": schema(&f.ty, project, &format!("{path}.{}", f.name), depth + 1, budget)?,
                }))).collect();
                json!({"kind": "struct", "type": label, "name": name, "fields": fs?})
            }
        },
        _ => json!({"kind": "scalar", "type": label}),
    })
}

/// Zero / false placeholders, with an explicitly named neutral enum variant
/// when unambiguous. An arbitrary first enum variant could arm a model or
/// assert health, so otherwise the user must choose it. These are shapes for
/// editing, never representative sensor readings or configuration values.
pub fn input_placeholder(ty: &Type, project: &Project) -> Result<Value, String> {
    let mut budget = MAX_PLACEHOLDER_VALUES;
    placeholder(ty, project, &type_name(ty), 0, &mut budget)
}

fn placeholder(ty: &Type, project: &Project, path: &str, depth: usize, budget: &mut usize) -> Result<Value, String> {
    depth_ok(depth, path)?;
    *budget = budget.checked_sub(1).ok_or_else(|| format!(
        "input `{path}`: generated placeholders are limited to {MAX_PLACEHOLDER_VALUES} values; enter this input explicitly"
    ))?;
    Ok(match ty {
        Type::Bool => Value::Bool(false),
        Type::Float32 => Value::Float32(0.0),
        Type::Float64 => Value::Float(0.0),
        Type::Array { elem, len } => {
            if *len as usize > *budget {
                return Err(format!("input `{path}`: array length {len} exceeds the remaining generated placeholder budget of {budget}; enter this input explicitly"));
            }
            Value::Array((0..*len)
                .map(|i| placeholder(elem, project, &format!("{path}[{i}]"), depth + 1, budget))
                .collect::<Result<_, _>>()?)
        },
        Type::Named { name } => match named(name, project, path)? {
            TypeBody::Alias { target, .. } => placeholder(target, project, path, depth + 1, budget)?,
            TypeBody::Record { fields, .. } => Value::Record(fields.iter()
                .map(|f| Ok((f.name.clone(), placeholder(&f.ty, project, &format!("{path}.{}", f.name), depth + 1, budget)?)))
                .collect::<Result<_, String>>()?),
            TypeBody::Enum(e) => {
                let neutral: Vec<_> = e.variants.iter().filter(|v| {
                    matches!(v.rsplit('_').next().unwrap_or(v).to_ascii_uppercase().as_str(),
                        "DISARMED" | "NONE" | "OFF" | "UNKNOWN" | "INVALID" | "UNAVAILABLE")
                }).collect();
                if neutral.len() != 1 {
                    return Err(format!("input `{path}`: choose a variant of enum `{name}` ({}); no unambiguous neutral placeholder is defined", e.variants.join(", ")));
                }
                Value::Enum(neutral[0].clone())
            }
        },
        _ => Value::Int(0),
    })
}

pub(super) fn parse(raw: &str, ty: &Type, project: &Project, path: &str) -> Result<Value, String> {
    parse_text(raw, ty, project, path, 0)
}

fn expected(path: &str, ty: &Type, detail: &str) -> String {
    format!("input `{path}`: expected {}; {detail}", type_name(ty))
}

/// The inclusive span a declared integer type accepts (uint64 spans 0..=2⁶³−1
/// in simulator storage; wider values are explicitly unsupported). Inputs
/// outside their declared width are refused — the generated C would silently
/// wrap them, and a diagrammed uint8 must never be fed 300.
fn int_range(ty: &Type) -> Option<(i64, i64)> {
    match ty {
        Type::Int8 => Some((i8::MIN as i64, i8::MAX as i64)),
        Type::Int16 => Some((i16::MIN as i64, i16::MAX as i64)),
        Type::Int32 => Some((i32::MIN as i64, i32::MAX as i64)),
        Type::Int64 => Some((i64::MIN, i64::MAX)),
        Type::Uint8 => Some((0, u8::MAX as i64)),
        Type::Uint16 => Some((0, u16::MAX as i64)),
        Type::Uint32 => Some((0, u32::MAX as i64)),
        Type::Uint64 => Some((0, i64::MAX)),
        _ => None,
    }
}

/// An integer already parsed as i64, checked against its declared width.
fn checked_int(v: i64, ty: &Type, path: &str) -> Result<Value, String> {
    if let Some((lo, hi)) = int_range(ty) {
        if v < lo || v > hi {
            return Err(expected(path, ty, &format!("{v} is outside the declared range; enter an integer from {lo} to {hi}")));
        }
    }
    Ok(Value::Int(v))
}

fn parse_text(raw: &str, ty: &Type, project: &Project, path: &str, depth: usize) -> Result<Value, String> {
    depth_ok(depth, path)?;
    let raw = raw.trim();
    match ty {
        Type::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "t" => Ok(Value::Bool(true)),
            "false" | "0" | "f" => Ok(Value::Bool(false)),
            _ => Err(expected(path, ty, "enter true or false")),
        },
        Type::Float32 => raw.parse::<f32>().ok().filter(|v| v.is_finite()).map(Value::Float32)
            .ok_or_else(|| expected(path, ty, "enter a finite float32 number; NaN, infinity and overflow are unsupported inputs")),
        Type::Float64 => raw.parse::<f64>().ok().filter(|v| v.is_finite()).map(Value::Float)
            .ok_or_else(|| expected(path, ty, "enter a finite float64 number; NaN, infinity and overflow are unsupported inputs")),
        t if t.is_integer() => raw.parse::<i64>()
            .map_err(|_| expected(path, ty, "enter an integer representable as signed 64-bit; larger uint64 inputs are unsupported"))
            .and_then(|v| checked_int(v, ty, path)),
        Type::Char => raw.parse::<u8>().map(|v| Value::Int(v as i64))
            .map_err(|_| expected(path, ty, "enter a character byte as an integer from 0 to 255")),
        Type::Array { elem, len } => {
            if let Ok(value) = serde_json::from_str::<Json>(raw) {
                return parse_json(&value, ty, project, path, depth);
            }
            let inner = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                .ok_or_else(|| expected(path, ty, "enter an array [value;...] or a JSON array"))?;
            let parts = split_items(inner, path)?;
            if parts.len() != *len as usize {
                return Err(expected(path, ty, &format!("array requires {len} elements, got {}", parts.len())));
            }
            Ok(Value::Array(parts.into_iter().enumerate()
                .map(|(i, r)| parse_text(r, elem, project, &format!("{path}[{i}]"), depth + 1))
                .collect::<Result<_, _>>()?))
        },
        Type::Named { name } => match named(name, project, path)? {
            TypeBody::Alias { target, .. } => parse_text(raw, target, project, path, depth + 1),
            TypeBody::Enum(e) => {
                let name = serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.into());
                if e.variants.contains(&name) {
                    Ok(Value::Enum(name))
                } else {
                    Err(expected(path, ty, &format!("choose an enum variant: {}", e.variants.join(", "))))
                }
            },
            TypeBody::Record { fields, .. } => {
                if let Ok(value) = serde_json::from_str::<Json>(raw) {
                    return parse_json(&value, ty, project, path, depth);
                }
                let inner = raw.strip_prefix('{').and_then(|s| s.strip_suffix('}'))
                    .ok_or_else(|| expected(path, ty, "enter a struct with named fields, as a JSON object or {field=value;...}"))?;
                if inner.trim_start().starts_with('"') {
                    let error = serde_json::from_str::<Json>(raw).unwrap_err();
                    return Err(expected(path, ty, &format!("invalid struct JSON: {error}")));
                }
                let mut entries = BTreeMap::new();
                for item in split_items(inner, path)? {
                    let (key, value) = item.split_once('=')
                        .ok_or_else(|| expected(path, ty, "each struct field needs field=value"))?;
                    let key = key.trim();
                    if !fields.iter().any(|f| f.name == key) {
                        return Err(format!("input `{path}.{key}`: unexpected field in struct `{name}`"));
                    }
                    if entries.insert(key, value.trim()).is_some() {
                        return Err(format!("input `{path}.{key}`: duplicate field in struct `{name}`"));
                    }
                }
                let mut values = BTreeMap::new();
                for field in fields {
                    let field_path = format!("{path}.{}", field.name);
                    let raw = entries.get(field.name.as_str())
                        .ok_or_else(|| format!("input `{field_path}`: missing required field of type {} in struct `{name}`", type_name(&field.ty)))?;
                    values.insert(field.name.clone(), parse_text(raw, &field.ty, project, &field_path, depth + 1)?);
                }
                Ok(Value::Record(values))
            },
        },
        _ => Err(expected(path, ty, "unsupported input type")),
    }
}

fn parse_json(value: &Json, ty: &Type, project: &Project, path: &str, depth: usize) -> Result<Value, String> {
    depth_ok(depth, path)?;
    let wrong = || expected(path, ty, &format!("got JSON {}; use the declared field type", json_kind(value)));
    match ty {
        Type::Bool => value.as_bool().map(Value::Bool).ok_or_else(wrong),
        Type::Float32 => value.as_f64().map(|v| v as f32).filter(|v| v.is_finite()).map(Value::Float32)
            .ok_or_else(|| expected(path, ty, "JSON value must be a finite float32 number; strings, NaN, infinity and overflow are unsupported inputs")),
        Type::Float64 => value.as_f64().filter(|v| v.is_finite()).map(Value::Float)
            .ok_or_else(|| expected(path, ty, "JSON value must be a finite float64 number; strings, NaN and infinity are unsupported inputs")),
        t if t.is_integer() => value.as_i64().ok_or_else(|| {
            expected(path, ty, "JSON value must be an integer representable as signed 64-bit; strings, fractions and larger uint64 inputs are unsupported")
        }).and_then(|v| checked_int(v, ty, path)),
        Type::Char => value.as_u64().filter(|v| *v <= 255).map(|v| Value::Int(v as i64))
            .ok_or_else(|| expected(path, ty, "JSON value must be a character byte from 0 to 255")),
        Type::Array { elem, len } => {
            let values = value.as_array().ok_or_else(wrong)?;
            if values.len() != *len as usize {
                return Err(expected(path, ty, &format!("array requires {len} elements, got {}", values.len())));
            }
            Ok(Value::Array(values.iter().enumerate()
                .map(|(i, v)| parse_json(v, elem, project, &format!("{path}[{i}]"), depth + 1))
                .collect::<Result<_, _>>()?))
        },
        Type::Named { name } => match named(name, project, path)? {
            TypeBody::Alias { target, .. } => parse_json(value, target, project, path, depth + 1),
            TypeBody::Enum(e) => {
                let variant = value.as_str().ok_or_else(wrong)?;
                if e.variants.iter().any(|v| v == variant) {
                    Ok(Value::Enum(variant.into()))
                } else {
                    Err(expected(path, ty, &format!("choose an enum variant: {}", e.variants.join(", "))))
                }
            },
            TypeBody::Record { fields, .. } => {
                let entries = value.as_object().ok_or_else(|| expected(path, ty,
                    &format!("struct requires a JSON object with named fields; got JSON {}", json_kind(value))))?;
                for key in entries.keys() {
                    if !fields.iter().any(|f| f.name == *key) {
                        return Err(format!("input `{path}.{key}`: unexpected field in struct `{name}`"));
                    }
                }
                let mut values = BTreeMap::new();
                for field in fields {
                    let field_path = format!("{path}.{}", field.name);
                    let value = entries.get(&field.name)
                        .ok_or_else(|| format!("input `{field_path}`: missing required field of type {} in struct `{name}`", type_name(&field.ty)))?;
                    values.insert(field.name.clone(), parse_json(value, &field.ty, project, &field_path, depth + 1)?);
                }
                Ok(Value::Record(values))
            },
        },
        _ => Err(wrong()),
    }
}

fn json_kind(value: &Json) -> &'static str {
    match value {
        Json::Null => "null",
        Json::Bool(_) => "Boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "array",
        Json::Object(_) => "object",
    }
}

/// Split only at the current level: separators inside nested structs or
/// arrays belong to those values. This makes trace text round-trip.
fn split_items<'a>(inner: &'a str, path: &str) -> Result<Vec<&'a str>, String> {
    if inner.trim().is_empty() {
        return Ok(Vec::new());
    }
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut begin = 0;
    let mut parts = Vec::new();
    for (i, ch) in inner.char_indices() {
        if quoted {
            if escaped { escaped = false; }
            else if ch == '\\' { escaped = true; }
            else if ch == '"' { quoted = false; }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '[' | '{' => stack.push(ch),
            ']' | '}' => {
                let opening = if ch == ']' { '[' } else { '{' };
                if stack.pop() != Some(opening) {
                    return Err(format!("input `{path}`: mismatched struct/array brackets"));
                }
            },
            ';' if stack.is_empty() => {
                parts.push(inner[begin..i].trim());
                begin = i + 1;
            },
            _ => {},
        }
    }
    if quoted || !stack.is_empty() {
        return Err(format!("input `{path}`: unclosed quote or struct/array bracket"));
    }
    parts.push(inner[begin..].trim());
    Ok(parts)
}
