//! A small Lustre frontend for *importing* existing `.lus` models so they can
//! be reused in a project. It parses the declaration shell — `type`, `const`,
//! `node`, and `function` — and delegates the hard parts to the parsers the
//! rest of the tool already uses: equation bodies and constant values go to
//! [`ol_stdlib::parse_expr`], and types to [`ol_stdlib::parse_type`] (with the
//! Lustre `elem^len` array form mapped here, since `parse_type` speaks the
//! `elem[len]` form). It covers the dataflow subset OpenLustre itself emits —
//! so an operator's own `<operator>.lus` round-trips — and produces a loud,
//! located error for anything outside that subset (assertions, inline
//! contracts, malformed declarations) rather than importing something wrong.

use ol_ir::{ConstDef, EnumDef, Equation, Local, NodeDef, NodeKind, Port, RecordField, Type, TypeBody, TypeDef};

/// Everything parsed out of a `.lus` source, in declaration order.
#[derive(Debug, Default, PartialEq)]
pub struct Imported {
    pub types: Vec<TypeDef>,
    pub constants: Vec<ConstDef>,
    pub nodes: Vec<NodeDef>,
}

/// Parse every `type` / `const` / `node` / `function` declaration in `src`.
///
/// `(*@layout <Node> {json} @*)` pragmas — the comments
/// [`ol_lustre_emit::emit_project_with_layout`] writes — are read back and
/// applied to the matching node's diagram, so a `.lus` file round-trips
/// *with its drawing*. Every other tool sees an ordinary block comment.
pub fn parse_lustre(src: &str) -> Result<Imported, String> {
    let mut imp = parse_declarations(src)?;
    apply_layout_pragmas(src, &mut imp.nodes)?;
    Ok(imp)
}

fn parse_declarations(src: &str) -> Result<Imported, String> {
    let cleaned = strip_comments(src);
    let mut s = cleaned.as_str();
    let mut imp = Imported::default();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            break;
        }
        if starts_with_kw(s, "type") {
            let (td, rest) = parse_type_decl(s)?;
            imp.types.push(td);
            s = rest;
        } else if starts_with_kw(s, "const") {
            let (cd, rest) = parse_const_decl(s)?;
            imp.constants.push(cd);
            s = rest;
        } else if starts_with_kw(s, "node") || starts_with_kw(s, "function") {
            let (nd, rest) = parse_node_decl(s)?;
            imp.nodes.push(nd);
            s = rest;
        } else {
            return Err(format!(
                "expected a `type`, `const`, `node`, or `function` declaration, found `{}`",
                snippet(s)
            ));
        }
    }
    if imp.types.is_empty() && imp.constants.is_empty() && imp.nodes.is_empty() {
        return Err("no `node`, `function`, `type`, or `const` declarations found".into());
    }
    Ok(imp)
}

// --- layout pragmas ----------------------------------------------------------

/// Find every `(*@layout <Node> {json} @*)` pragma in the raw source and
/// apply its geometry to the matching imported node. Malformed pragmas and
/// pragmas naming a node the file doesn't declare are loud errors — silent
/// geometry loss is exactly what this feature exists to prevent.
fn apply_layout_pragmas(src: &str, nodes: &mut [NodeDef]) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Payload {
        #[serde(default)]
        grid: Option<u32>,
        #[serde(default)]
        positions: std::collections::BTreeMap<String, ol_ir::NodePos>,
    }
    let mut rest = src;
    while let Some(at) = rest.find("(*@layout") {
        let after = &rest[at + "(*@layout".len()..];
        let end = after
            .find("@*)")
            .ok_or("unterminated `(*@layout` pragma (missing `@*)`)")?;
        let body = after[..end].trim();
        let (name, json) = body
            .split_once(char::is_whitespace)
            .ok_or("`(*@layout` pragma needs a node name and a JSON payload")?;
        let payload: Payload = serde_json::from_str(json.trim())
            .map_err(|e| format!("layout pragma for `{name}`: bad JSON payload: {e}"))?;
        let node = nodes
            .iter_mut()
            .find(|n| n.name == name)
            .ok_or_else(|| format!("layout pragma names `{name}`, which this file does not declare"))?;
        node.diagram.grid = payload.grid;
        node.diagram.positions = payload.positions;
        rest = &after[end + "@*)".len()..];
    }
    Ok(())
}

// --- declarations ------------------------------------------------------------

fn parse_node_decl(input: &str) -> Result<(NodeDef, &str), String> {
    let s = input.trim_start();
    let (kw, s) = read_ident(s).ok_or("expected `node` or `function`")?;
    let kind = match kw {
        "node" => NodeKind::Operator,
        "function" => NodeKind::Function,
        other => return Err(format!("expected `node`/`function`, found `{other}`")),
    };
    let (name, s) = read_ident(s).ok_or("expected an operator name")?;
    let name = name.to_string();
    let (params, s) = read_balanced(s, '(', ')').map_err(|e| format!("`{name}` parameters: {e}"))?;
    let s = s.trim_start();
    if !starts_with_kw(s, "returns") {
        return Err(format!("`{name}`: expected `returns` after the parameter list"));
    }
    let s = s.trim_start()[ "returns".len()..].trim_start();
    let (outs, s) = read_balanced(s, '(', ')').map_err(|e| format!("`{name}` returns: {e}"))?;
    let s = s.trim_start();
    let s = s
        .strip_prefix(';')
        .ok_or_else(|| format!("`{name}`: expected `;` after `returns (...)`"))?;

    // Everything from here to `let` is the optional `var` section; the body is
    // between `let` and `tel`.
    let let_at = find_keyword(s, "let").ok_or_else(|| format!("`{name}`: expected `let`"))?;
    let var_section = &s[..let_at];
    let after_let = &s[let_at + 3..];
    let tel_at = find_keyword(after_let, "tel").ok_or_else(|| format!("`{name}`: expected `tel`"))?;
    let body = &after_let[..tel_at];
    let mut rest = after_let[tel_at + 3..].trim_start();
    rest = rest.strip_prefix(';').unwrap_or(rest); // optional trailing `;`

    let inputs = parse_decls(params)
        .map_err(|e| format!("`{name}` inputs: {e}"))?
        .into_iter()
        .map(|(name, ty)| Port { name, ty })
        .collect();
    let outputs = parse_decls(outs)
        .map_err(|e| format!("`{name}` outputs: {e}"))?
        .into_iter()
        .map(|(name, ty)| Port { name, ty })
        .collect();
    let mut var_section = var_section.trim();
    if starts_with_kw(var_section, "var") {
        var_section = var_section.trim_start()["var".len()..].trim();
    }
    let locals = parse_decls(var_section)
        .map_err(|e| format!("`{name}` locals: {e}"))?
        .into_iter()
        .map(|(name, ty)| Local { name, ty })
        .collect();
    let equations = parse_equations(body).map_err(|e| format!("`{name}` body: {e}"))?;

    Ok((
        NodeDef {
            name,
            kind,
            inputs,
            outputs,
            locals,
            equations,
            contract: None,
            diagram: Default::default(),
            probes: vec![],
            requirements: vec![],
        sysml: None,
        generics: vec![],
        },
        rest,
    ))
}

fn parse_type_decl(input: &str) -> Result<(TypeDef, &str), String> {
    let s = input.trim_start()["type".len()..].trim_start();
    let (name, s) = read_ident(s).ok_or("`type`: expected a name")?;
    let name = name.to_string();
    let s = s
        .trim_start()
        .strip_prefix('=')
        .ok_or_else(|| format!("type `{name}`: expected `=`"))?;
    let semi = find_top(s, ';').ok_or_else(|| format!("type `{name}`: expected `;`"))?;
    let body_str = s[..semi].trim();
    let rest = &s[semi + 1..];
    let body = parse_type_body(&name, body_str)?;
    Ok((TypeDef { body }, rest))
}

fn parse_type_body(name: &str, body: &str) -> Result<TypeBody, String> {
    if starts_with_kw(body, "enum") {
        let after = body.trim_start()["enum".len()..].trim_start();
        let (inside, _) = read_balanced(after, '{', '}').map_err(|e| format!("enum `{name}`: {e}"))?;
        let variants: Vec<String> = inside
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect();
        if variants.is_empty() {
            return Err(format!("enum `{name}` has no variants"));
        }
        for v in &variants {
            if !is_ident(v) {
                return Err(format!("enum `{name}`: `{v}` is not a valid variant"));
            }
        }
        Ok(TypeBody::Enum(EnumDef { name: name.to_string(), variants }))
    } else if starts_with_kw(body, "struct") {
        let after = body.trim_start()["struct".len()..].trim_start();
        let (inside, _) = read_balanced(after, '{', '}').map_err(|e| format!("struct `{name}`: {e}"))?;
        let mut fields = Vec::new();
        for (fname, ty) in parse_decls(inside).map_err(|e| format!("struct `{name}`: {e}"))? {
            fields.push(RecordField { name: fname, ty });
        }
        if fields.is_empty() {
            return Err(format!("struct `{name}` has no fields"));
        }
        Ok(TypeBody::Record { name: name.to_string(), fields })
    } else {
        // Alias to another type (including arrays: `int^4`, `int[4]`).
        let target = parse_lustre_type(body)?;
        Ok(TypeBody::Alias { name: name.to_string(), target })
    }
}

fn parse_const_decl(input: &str) -> Result<(ConstDef, &str), String> {
    let s = input.trim_start()["const".len()..].trim_start();
    let (name, s) = read_ident(s).ok_or("`const`: expected a name")?;
    let name = name.to_string();
    let s = s
        .trim_start()
        .strip_prefix(':')
        .ok_or_else(|| format!("const `{name}`: expected `: <type>`"))?;
    // The type runs to the first `=`; the value runs to the top-level `;`.
    let eq = s.find('=').ok_or_else(|| format!("const `{name}`: expected `=`"))?;
    let ty = parse_lustre_type(s[..eq].trim())?;
    let after_eq = &s[eq + 1..];
    let semi = find_top(after_eq, ';').ok_or_else(|| format!("const `{name}`: expected `;`"))?;
    let value_str = after_eq[..semi].trim();
    let value = ol_stdlib::parse_expr(value_str).map_err(|e| format!("const `{name}` value: {e}"))?;
    Ok((ConstDef { name, ty, value }, &after_eq[semi + 1..]))
}

// --- shared decl/equation parsing --------------------------------------------

/// Parse a `;`-separated declaration list (`a: int; b, c: real`). Each group is
/// `names : type`, where `names` is comma-separated (sharing the one type), so
/// both our own emit (`a: int; b: int`) and the type-sharing form import.
fn parse_decls(s: &str) -> Result<Vec<(String, Type)>, String> {
    let mut out = Vec::new();
    for group in split_top(s, ';') {
        let g = group.trim();
        if g.is_empty() {
            continue;
        }
        let (names, ty_str) = g
            .split_once(':')
            .ok_or_else(|| format!("`{g}` is missing a `: type`"))?;
        let ty = parse_lustre_type(ty_str.trim())?;
        for nm in names.split(',') {
            let nm = nm.trim();
            if nm.is_empty() {
                continue;
            }
            if !is_ident(nm) {
                return Err(format!("`{nm}` is not a valid name"));
            }
            out.push((nm.to_string(), ty.clone()));
        }
    }
    Ok(out)
}

fn parse_equations(s: &str) -> Result<Vec<Equation>, String> {
    let mut out = Vec::new();
    for stmt in split_top(s, ';') {
        let st = stmt.trim();
        if st.is_empty() {
            continue;
        }
        if starts_with_kw(st, "assert") {
            return Err(format!("`assert` is not supported on import (`{}`)", snippet(st)));
        }
        let eq_at = find_assign(st).ok_or_else(|| format!("equation `{}` has no `=`", snippet(st)))?;
        let lhs = parse_lhs(st[..eq_at].trim())?;
        let rhs = ol_stdlib::parse_expr(st[eq_at + 1..].trim())
            .map_err(|e| format!("equation `{}`: {e}", snippet(st)))?;
        out.push(Equation { lhs, rhs });
    }
    Ok(out)
}

fn parse_lhs(s: &str) -> Result<Vec<String>, String> {
    let inner = s
        .strip_prefix('(')
        .and_then(|x| x.strip_suffix(')'))
        .unwrap_or(s);
    let mut lhs = Vec::new();
    for nm in inner.split(',') {
        let nm = nm.trim();
        if nm.is_empty() {
            continue;
        }
        if !is_ident(nm) {
            return Err(format!("`{nm}` is not a valid left-hand-side name"));
        }
        lhs.push(nm.to_string());
    }
    if lhs.is_empty() {
        return Err("equation has an empty left-hand side".into());
    }
    Ok(lhs)
}

/// Map a Lustre type to IR, handling the `elem^len` array form (which our
/// emitter produces) before delegating to `ol_stdlib::parse_type` (which speaks
/// `int`/`real`/`bool`, the explicit widths, named types, and `elem[len]`).
fn parse_lustre_type(s: &str) -> Result<Type, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("missing type".into());
    }
    // `elem^len` (right-most `^` is the outermost array, matching `lustre_name`).
    if let Some(i) = s.rfind('^') {
        let elem = parse_lustre_type(&s[..i])?;
        let len: u32 = s[i + 1..]
            .trim()
            .parse()
            .map_err(|_| format!("array length in `{s}` is not a number"))?;
        return Ok(Type::Array { elem: Box::new(elem), len });
    }
    ol_stdlib::parse_type(s).map_err(|e| format!("type `{s}`: {e}"))
}

// --- low-level scanning helpers ----------------------------------------------

/// Remove `--` line comments and `(* … *)` block comments, preserving byte
/// fidelity outside comments (so non-ASCII inside expressions survives).
fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'-' && i + 1 < b.len() && b[i + 1] == b'-' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'(' && i + 1 < b.len() && b[i + 1] == b'*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b')') {
                i += 1;
            }
            i = (i + 2).min(b.len());
            out.push(b' '); // keep token separation
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}
fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next().is_some_and(is_ident_start) && s.chars().all(is_ident_char)
}

/// Does `s` begin (after leading whitespace) with the keyword `kw` on a word
/// boundary?
fn starts_with_kw(s: &str, kw: &str) -> bool {
    let s = s.trim_start();
    s.starts_with(kw) && !s[kw.len()..].chars().next().is_some_and(is_ident_char)
}

/// Read a leading identifier, returning it and the remainder.
fn read_ident(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if !s.chars().next().is_some_and(is_ident_start) {
        return None;
    }
    let end = s.find(|c: char| !is_ident_char(c)).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// After optional whitespace, expect `open`, then return the text up to the
/// matching `close` (respecting nesting) and the remainder after it.
fn read_balanced(s: &str, open: char, close: char) -> Result<(&str, &str), String> {
    let s = s.trim_start();
    let mut chars = s.char_indices();
    match chars.next() {
        Some((_, c)) if c == open => {}
        _ => return Err(format!("expected `{open}`")),
    }
    let start = open.len_utf8();
    let mut depth = 1i32;
    for (i, c) in s[start..].char_indices() {
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                let abs = start + i;
                return Ok((&s[start..abs], &s[abs + close.len_utf8()..]));
            }
        }
    }
    Err(format!("unbalanced `{open}`"))
}

/// Index of `kw` appearing as a whole word in `s`, if any.
fn find_keyword(s: &str, kw: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = s[from..].find(kw) {
        let i = from + rel;
        let before_ok = !s[..i].chars().next_back().is_some_and(is_ident_char);
        let after_ok = !s[i + kw.len()..].chars().next().is_some_and(is_ident_char);
        if before_ok && after_ok {
            return Some(i);
        }
        from = i + kw.len();
    }
    None
}

/// Index of the first `delim` at bracket depth 0.
fn find_top(s: &str, delim: char) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            d if d == delim && depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Split on every `delim` at bracket depth 0.
fn split_top(s: &str, delim: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            d if d == delim && depth == 0 => {
                parts.push(&s[start..i]);
                start = i + d.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// The byte index of the assignment `=` in an equation: the first `=` at depth
/// 0 that is not part of `<=`, `>=`, `<>`, `==`, `:=` (the left side of an
/// equation is only names, so the first standalone `=` is the assignment).
fn find_assign(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                let prev = if i > 0 { b[i - 1] } else { 0 };
                let next = if i + 1 < b.len() { b[i + 1] } else { 0 };
                if !matches!(prev, b'<' | b'>' | b'=' | b'!' | b':') && next != b'=' {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn snippet(s: &str) -> String {
    let t = s.trim();
    let cut = t.char_indices().nth(48).map(|(i, _)| i).unwrap_or(t.len());
    if cut < t.len() {
        format!("{}…", &t[..cut])
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_a_node_with_locals_and_tuple_lhs() {
        let src = r#"
            -- a small operator
            node Avg(a: int; b: int) returns (m: int);
            var sum: int;
            let
              sum = a + b;
              m = sum / 2;
            tel
        "#;
        let imp = parse_lustre(src).expect("parse");
        assert_eq!(imp.nodes.len(), 1);
        let n = &imp.nodes[0];
        assert_eq!(n.name, "Avg");
        assert_eq!(n.kind, NodeKind::Operator);
        assert_eq!(n.inputs.len(), 2);
        assert_eq!(n.outputs[0].name, "m");
        assert_eq!(n.locals[0].name, "sum");
        assert_eq!(n.equations.len(), 2);
    }

    #[test]
    fn imports_function_and_maps_lustre_types() {
        let src = "function Scale(x: real) returns (y: real); let y = x; tel";
        let imp = parse_lustre(src).unwrap();
        assert_eq!(imp.nodes[0].kind, NodeKind::Function);
        assert_eq!(imp.nodes[0].inputs[0].ty, Type::Float64); // `real` -> Float64
    }

    #[test]
    fn imports_array_type_caret_form() {
        // `int^4` (our emitter's form) becomes a 4-element int array.
        let src = "node N(v: int^4) returns (s: int); let s = 0; tel";
        let imp = parse_lustre(src).unwrap();
        match &imp.nodes[0].inputs[0].ty {
            Type::Array { elem, len } => {
                assert_eq!(**elem, Type::Int32);
                assert_eq!(*len, 4);
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn imports_types_and_constants() {
        let src = r#"
            type Mode = enum { OFF, ON };
            type Vec3 = real^3;
            const MAX : int = 32;
            node N(e: Mode) returns (o: bool); let o = true; tel
        "#;
        let imp = parse_lustre(src).unwrap();
        assert_eq!(imp.types.len(), 2);
        assert_eq!(imp.constants.len(), 1);
        assert_eq!(imp.constants[0].name, "MAX");
        assert_eq!(imp.nodes.len(), 1);
    }

    #[test]
    fn assert_is_rejected_loudly() {
        let src = "node N() returns (o: bool); let o = true; assert o; tel";
        let err = parse_lustre(src).unwrap_err();
        assert!(err.contains("assert"), "got: {err}");
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(parse_lustre("   -- just a comment\n").is_err());
    }

    #[test]
    fn layout_pragma_round_trips_the_drawing() {
        // A node with canvas geometry, emitted with layout pragmas…
        let mut project: ol_ir::Project = serde_json::from_value(serde_json::json!({
            "name": "p",
            "packages": [{
                "name": "user",
                "nodes": [{
                    "name": "Avg",
                    "kind": "Function",
                    "inputs": [{"name": "a", "ty": {"kind": "Int32"}}],
                    "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                    "equations": [{"lhs": ["y"],
                        "rhs": {"expr": "Binary", "op": "Add",
                                "lhs": {"expr": "Var", "name": "a"},
                                "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 1}}}}]
                }]
            }]
        }))
        .unwrap();
        let node = &mut project.packages[0].nodes[0];
        node.diagram.grid = Some(8);
        node.diagram.positions.insert(
            "eq0".into(),
            ol_ir::NodePos { x: 96.0, y: 48.0, w: Some(120.0), ..Default::default() },
        );
        node.diagram.positions.insert(
            "a".into(),
            ol_ir::NodePos { x: 16.0, y: 16.0, wrap: true, ..Default::default() },
        );
        let lus = ol_lustre_emit::emit_project_with_layout(&project);
        assert!(lus.contains("(*@layout Avg"), "{lus}");

        // …re-imports with the geometry intact, including sizes and wrap.
        let imp = parse_lustre(&lus).expect("round-trip import");
        let n = &imp.nodes[0];
        assert_eq!(n.diagram.grid, Some(8));
        let eq0 = &n.diagram.positions["eq0"];
        assert_eq!((eq0.x, eq0.y, eq0.w), (96.0, 48.0, Some(120.0)));
        assert!(n.diagram.positions["a"].wrap);

        // Files without pragmas import with an empty (automatic) layout.
        let plain = parse_lustre("node N(x: bool) returns (y: bool);\nlet\n  y = x;\ntel\n")
            .unwrap();
        assert!(plain.nodes[0].diagram.positions.is_empty());
    }

    #[test]
    fn malformed_and_misdirected_layout_pragmas_are_loud() {
        let base = "node N(x: bool) returns (y: bool);\nlet\n  y = x;\ntel\n";
        // Bad JSON payload.
        let e = parse_lustre(&format!("{base}(*@layout N {{not json}} @*)\n")).unwrap_err();
        assert!(e.contains("bad JSON payload"), "{e}");
        // Pragma naming an undeclared node.
        let e = parse_lustre(&format!(
            "{base}(*@layout Ghost {{\"positions\":{{}}}} @*)\n"
        ))
        .unwrap_err();
        assert!(e.contains("does not declare"), "{e}");
        // Unterminated pragma.
        let e = parse_lustre(&format!("{base}(*@layout N {{}}")).unwrap_err();
        assert!(e.contains("unterminated"), "{e}");
    }
}
