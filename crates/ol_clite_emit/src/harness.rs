//! Generates a tiny `main()` driver that reads a CSV input vector on stdin,
//! drives the generated `_step` function each cycle, and writes a CSV output
//! trace on stdout matching the format produced by the IR simulator
//! ([`ol_sim::Trace::to_csv`]) for the same node. The two traces are expected
//! to be byte-identical for any model in the Phase 0 profile — that is the
//! invariant Phase 6 trace comparison verifies.

use std::fmt::Write as _;

use std::collections::BTreeMap;

use ol_ir::{NodeDef, NodeKind, Project, Type, TypeBody};

/// Variant names of the enum types a driver reads or prints, keyed by the
/// type name on the port (an alias maps to its enum's variants).
pub type EnumNames = BTreeMap<String, Vec<String>>;

/// The enum types on `node`'s ports (array elements included), resolved
/// through aliases.
pub fn enum_names(project: &Project, node: &NodeDef) -> EnumNames {
    let find = |name: &str| project.packages.iter().find_map(|p| p.find_type(name));
    let mut out = EnumNames::new();
    for p in node.inputs.iter().chain(&node.outputs) {
        let mut ty = &p.ty;
        while let Type::Array { elem, .. } = ty {
            ty = elem;
        }
        let Type::Named { name } = ty else { continue };
        let mut cur = name.clone();
        for _ in 0..16 {
            match find(&cur).map(|t| &t.body) {
                Some(TypeBody::Enum(e)) => {
                    out.insert(name.clone(), e.variants.clone());
                    break;
                }
                Some(TypeBody::Alias { target: Type::Named { name: next }, .. }) => cur = next.clone(),
                _ => break,
            }
        }
    }
    out
}

/// A CSV driver that reads and prints enum values by variant name, exactly
/// as the simulator's traces do (it also reads a variant's number).
pub fn emit_csv_driver_for(project: &Project, node: &NodeDef, monitor_contract_name: Option<&str>) -> String {
    emit_driver(node, monitor_contract_name, &enum_names(project, node))
}

pub fn emit_csv_driver(node: &NodeDef) -> String {
    emit_csv_driver_with_monitor(node, None)
}

/// Generate a CSV driver. If `monitor_contract_name` is `Some(name)`, the
/// driver also wires in the matching contract monitor and emits
/// `active_mode` and `violations` columns after the outputs — the same shape
/// the IR simulator writes when the node has a contract. Enums are read and
/// printed as numbers; [`emit_csv_driver_for`] uses their names.
pub fn emit_csv_driver_with_monitor(
    node: &NodeDef,
    monitor_contract_name: Option<&str>,
) -> String {
    emit_driver(node, monitor_contract_name, &EnumNames::new())
}

/// The enum type an element of `ty` names, when `enums` knows it.
fn enum_of<'a>(ty: &'a Type, enums: &EnumNames) -> Option<&'a str> {
    match ty {
        Type::Named { name } if enums.contains_key(name) => Some(name),
        Type::Array { elem, .. } => enum_of(elem, enums),
        _ => None,
    }
}

/// `ol_enum_<T>(text)` and `ol_enum_name_<T>(value)` for the enum types the
/// driver reads (`parse`) or prints (`print`) — only those, as unused static
/// functions would trip `-Werror`.
fn emit_enum_helpers(s: &mut String, node: &NodeDef, enums: &EnumNames) {
    let used = |ports: &[ol_ir::Port]| -> std::collections::BTreeSet<String> {
        ports.iter().filter_map(|p| enum_of(&p.ty, enums)).map(str::to_string).collect()
    };
    for t in used(&node.inputs) {
        let _ = writeln!(s, "static int ol_enum_{}(const char* s) {{", crate::c_ident(&t));
        for v in &enums[&t] {
            let _ = writeln!(s, "  if (strcmp(s, \"{v}\") == 0) return {v};");
        }
        let _ = writeln!(s, "  return (int) strtol(s, NULL, 10);");
        let _ = writeln!(s, "}}\n");
    }
    for t in used(&node.outputs) {
        let _ = writeln!(s, "static const char* ol_enum_name_{}(int v) {{", crate::c_ident(&t));
        let _ = writeln!(s, "  switch (v) {{");
        for v in &enums[&t] {
            let _ = writeln!(s, "    case {v}: return \"{v}\";");
        }
        let _ = writeln!(s, "  }}");
        let _ = writeln!(s, "  return \"?\";");
        let _ = writeln!(s, "}}\n");
    }
}

fn emit_driver(node: &NodeDef, monitor_contract_name: Option<&str>, enums: &EnumNames) -> String {
    let mut s = String::new();
    let prefix = &node.name;

    let _ = writeln!(s, "/* CSV driver for {prefix}. */");
    let _ = writeln!(s, "#include \"openlustre_generated.h\"");
    if monitor_contract_name.is_some() {
        let _ = writeln!(s, "#include \"openlustre_monitors.h\"");
    }
    let _ = writeln!(s, "#include <stdio.h>");
    let _ = writeln!(s, "#include <stdlib.h>");
    let _ = writeln!(s, "#include <string.h>");
    s.push('\n');
    if node.outputs.iter().any(|p| is_real(&p.ty)) {
        s.push_str(PRINT_REAL);
        s.push('\n');
    }
    emit_enum_helpers(&mut s, node, enums);
    let _ = writeln!(s, "int main(void) {{");
    if node.kind != NodeKind::Function {
        let _ = writeln!(s, "  {prefix}_State state;");
        let _ = writeln!(s, "  {prefix}_init(&state);");
    }
    let _ = writeln!(s, "  {prefix}_Input in;");
    let _ = writeln!(s, "  {prefix}_Output out;");
    if let Some(contract_name) = monitor_contract_name {
        let _ = writeln!(s, "  {contract_name}_monitor_State mon;");
        let _ = writeln!(s, "  {contract_name}_monitor_reset(&mon);");
        let _ = writeln!(s, "  char mode_buf[256];");
        let _ = writeln!(s, "  char viol_buf[1024];");
    }
    let _ = writeln!(s, "  char line[4096];");
    let _ = writeln!(s, "  /* drop the header row */");
    let _ = writeln!(s, "  if (!fgets(line, sizeof(line), stdin)) return 0;");

    let mut header_parts: Vec<String> = std::iter::once("cycle".to_string())
        .chain(node.outputs.iter().map(|p| p.name.clone()))
        .collect();
    if monitor_contract_name.is_some() {
        header_parts.push("active_mode".into());
        header_parts.push("violations".into());
    }
    let _ = writeln!(s, "  printf(\"{}\\n\");", header_parts.join(","));
    // Flushed per line so a caller can drive the program one cycle at a
    // time over pipes (the Studio's C-in-the-loop stepping).
    let _ = writeln!(s, "  fflush(stdout);");

    let _ = writeln!(s, "  int cycle = 0;");
    let _ = writeln!(s, "  while (fgets(line, sizeof(line), stdin)) {{");
    let _ = writeln!(s, "    line[strcspn(line, \"\\r\\n\")] = 0;");
    let _ = writeln!(s, "    if (line[0] == 0) continue;");
    let _ = writeln!(s, "    char* tok = strtok(line, \",\");");
    for p in &node.inputs {
        let _ = writeln!(s, "    if (!tok) return 1;");
        match &p.ty {
            Type::Array { elem, len } => emit_array_parse(&mut s, &crate::c_ident(&p.name), elem, *len, enums),
            _ => {
                let _ = writeln!(
                    s,
                    "    in.{} = {};",
                    crate::c_ident(&p.name),
                    parse_expr(&p.ty, "tok", enums)
                );
            }
        }
        let _ = writeln!(s, "    tok = strtok(NULL, \",\");");
    }
    if node.kind != NodeKind::Function {
        let _ = writeln!(s, "    {prefix}_step(&state, &in, &out);");
    } else {
        let _ = writeln!(s, "    {prefix}_step(&in, &out);");
    }
    if let Some(contract_name) = monitor_contract_name {
        let _ = writeln!(
            s,
            "    {contract_name}_monitor_check(&mon, &in, &out, mode_buf, sizeof(mode_buf), viol_buf, sizeof(viol_buf));"
        );
    }
    let _ = writeln!(s, "    printf(\"%d\", cycle);");
    for p in &node.outputs {
        let _ = writeln!(s, "    printf(\",\");");
        match &p.ty {
            Type::Array { elem, len } => emit_array_print(&mut s, &crate::c_ident(&p.name), elem, *len, enums),
            _ => {
                let _ = writeln!(
                    s,
                    "    {}",
                    print_stmt(&p.ty, &format!("out.{}", crate::c_ident(&p.name)), enums)
                );
            }
        }
    }
    if monitor_contract_name.is_some() {
        let _ = writeln!(s, "    printf(\",%s,%s\", mode_buf, viol_buf);");
    }
    let _ = writeln!(s, "    printf(\"\\n\");");
    let _ = writeln!(s, "    fflush(stdout);");
    let _ = writeln!(s, "    cycle++;");
    let _ = writeln!(s, "  }}");
    let _ = writeln!(s, "  return 0;");
    let _ = writeln!(s, "}}");
    s
}

fn is_real(ty: &Type) -> bool {
    match ty {
        Type::Array { elem, .. } => is_real(elem),
        t => t.is_float(),
    }
}

/// Prints a real exactly as the simulator writes it (`ol_sim::fmt_real`): the
/// fewest correctly rounded significant digits that read back as the same
/// `float` / `double`, written positionally — so model and code traces are
/// byte-identical for reals too.
const PRINT_REAL: &str = r#"#include <math.h>
static void ol_print_real(double x, int single) {
  char sci[48], digits[48];
  int p, nd = 0, exp = 0, k;
  const char* c;
  if (isnan(x)) { fputs("NaN", stdout); return; }
  if (isinf(x)) { fputs(x < 0 ? "-inf" : "inf", stdout); return; }
  for (p = 1; p <= (single ? 9 : 17); p++) {
    snprintf(sci, sizeof sci, "%.*e", p - 1, x);
    if (single ? (strtof(sci, NULL) == (float) x) : (strtod(sci, NULL) == x)) break;
  }
  c = sci;
  if (*c == '-') { putchar('-'); c++; }
  for (; *c && *c != 'e' && *c != 'E'; c++) if (*c != '.') digits[nd++] = *c;
  digits[nd] = 0;
  if (*c) exp = atoi(c + 1);
  if (exp >= 0) {
    if (nd > exp + 1) { fwrite(digits, 1, (size_t) (exp + 1), stdout); putchar('.'); fputs(digits + exp + 1, stdout); }
    else { fputs(digits, stdout); for (k = nd; k < exp + 1; k++) putchar('0'); }
  } else {
    fputs("0.", stdout);
    for (k = 0; k < -exp - 1; k++) putchar('0');
    fputs(digits, stdout);
  }
}
"#;

/// Parse a bracketed `[e0;e1;…]` token into `in.<name>[k]`. `strtoll`/`strtod`
/// advance a cursor past each element; we skip the `[` and `;` separators by
/// hand (strtok is already in use on the outer comma split, so no nesting).
fn emit_array_parse(s: &mut String, name: &str, elem: &Type, len: u32, enums: &EnumNames) {
    // Booleans and enum names are words: copy each element out, then read it.
    if *elem == Type::Bool || enum_of(elem, enums).is_some() {
        let _ = writeln!(s, "    {{");
        let _ = writeln!(s, "      char* __p = tok;");
        let _ = writeln!(s, "      for (int __k = 0; __k < {len}; __k++) {{");
        let _ = writeln!(s, "        char __w[64]; int __n = 0;");
        let _ = writeln!(s, "        while (*__p=='[' || *__p==';' || *__p==' ') __p++;");
        let _ = writeln!(s, "        while (*__p && *__p!=';' && *__p!=']' && __n < 63) __w[__n++] = *__p++;");
        let _ = writeln!(s, "        __w[__n] = 0;");
        let _ = writeln!(s, "        in.{name}[__k] = {};", parse_expr(elem, "__w", enums));
        let _ = writeln!(s, "      }}");
        let _ = writeln!(s, "    }}");
        return;
    }
    // float32 elements parse straight to single precision, as the simulator
    // reads them (decimal → double → float could round twice).
    let read = match elem {
        Type::Float32 => "strtof(__p, &__e)",
        Type::Float64 => "strtod(__p, &__e)",
        _ => "strtoll(__p, &__e, 10)",
    };
    let _ = writeln!(s, "    {{");
    let _ = writeln!(s, "      char* __p = tok; char* __e;");
    let _ = writeln!(s, "      for (int __k = 0; __k < {len}; __k++) {{");
    let _ = writeln!(s, "        while (*__p=='[' || *__p==';' || *__p==' ') __p++;");
    let _ = writeln!(s, "        in.{name}[__k] = ({}) {read};", elem.c_name());
    let _ = writeln!(s, "        __p = __e;");
    let _ = writeln!(s, "      }}");
    let _ = writeln!(s, "    }}");
}

/// Print `out.<name>` as `[e0;e1;…]`, matching `Value::to_csv` for arrays.
fn emit_array_print(s: &mut String, name: &str, elem: &Type, len: u32, enums: &EnumNames) {
    let item = if elem.is_float() {
        format!("ol_print_real((double) out.{name}[__k], {});", (*elem == Type::Float32) as u8)
    } else {
        print_stmt(elem, &format!("out.{name}[__k]"), enums)
    };
    let _ = writeln!(s, "    printf(\"[\");");
    let _ = writeln!(s, "    for (int __k = 0; __k < {len}; __k++) {{");
    let _ = writeln!(s, "      if (__k) printf(\";\");");
    let _ = writeln!(s, "      {item}");
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s, "    printf(\"]\");");
}

/// A free-running DEBUG driver: no CSV; inputs are held at the values the user
/// set in the simulation watch table (`held`, keyed by input name) or their
/// type defaults when unset. Prints a start banner, the held inputs, and the
/// outputs + any log-message probes every `STRIDE` cycles. Compiled with
/// `-DOL_DEBUG`, this is the "run it and watch it tick" build the GUI launches.
pub fn emit_debug_driver(
    node: &NodeDef,
    held: &std::collections::BTreeMap<String, String>,
) -> String {
    const STRIDE: u32 = 50;
    const STEPS: u32 = 500;
    let mut s = String::new();
    let prefix = &node.name;
    let _ = writeln!(s, "/* OpenLustre DEBUG driver for {prefix}. */");
    let _ = writeln!(s, "#include \"openlustre_generated.h\"");
    let _ = writeln!(s, "#include <stdio.h>");
    let _ = writeln!(s, "#include <string.h>");
    // `_step` reads this flag (under OL_DEBUG) to decide when to print probes.
    let _ = writeln!(s, "int ol_dbg_print = 0;");
    s.push('\n');
    let _ = writeln!(s, "int main(void) {{");
    let _ = writeln!(
        s,
        "  printf(\"=== OpenLustre debug run: {prefix} (held inputs, every {STRIDE} steps) ===\\n\");"
    );
    if node.kind != NodeKind::Function {
        let _ = writeln!(s, "  {prefix}_State state;");
        let _ = writeln!(s, "  {prefix}_init(&state);");
    }
    let _ = writeln!(s, "  {prefix}_Input in;");
    let _ = writeln!(s, "  {prefix}_Output out;");
    let _ = writeln!(s, "  memset(&in, 0, sizeof(in));");

    // Hold each input at the user's watch-table value (parsed to a safe C
    // literal — never the raw string, so the run can't inject code). Unset or
    // unparseable inputs stay at the memset-zero default.
    for p in &node.inputs {
        if let Some(lit) = held.get(&p.name).and_then(|raw| c_literal(&p.ty, raw)) {
            let _ = writeln!(s, "  in.{} = {lit};", crate::c_ident(&p.name));
        }
    }

    // Banner: the top operator's (held) input values.
    let _ = writeln!(s, "  printf(\"initial inputs: \");");
    for p in &node.inputs {
        let _ = writeln!(s, "  {}", dbg_field(&p.ty, &format!("in.{}", crate::c_ident(&p.name)), &p.name));
    }
    if node.inputs.is_empty() {
        let _ = writeln!(s, "  printf(\"(none)\");");
    }
    let _ = writeln!(s, "  printf(\"\\n\");");

    let _ = writeln!(s, "  for (int step = 0; step < {STEPS}; step++) {{");
    let _ = writeln!(s, "    ol_dbg_print = (step % {STRIDE} == 0);");
    if node.kind != NodeKind::Function {
        let _ = writeln!(s, "    {prefix}_step(&state, &in, &out);");
    } else {
        let _ = writeln!(s, "    {prefix}_step(&in, &out);");
    }
    let _ = writeln!(s, "    if (ol_dbg_print) {{");
    let _ = writeln!(s, "      printf(\"step %d | \", step);");
    for p in &node.outputs {
        let _ = writeln!(s, "      {}", dbg_field(&p.ty, &format!("out.{}", crate::c_ident(&p.name)), &p.name));
    }
    let _ = writeln!(s, "      printf(\"\\n\");");
    let _ = writeln!(s, "    }}");
    let _ = writeln!(s, "  }}");
    let _ = writeln!(s, "  printf(\"done after {STEPS} steps.\\n\");");
    let _ = writeln!(s, "  return 0;");
    let _ = writeln!(s, "}}");
    s
}

/// Parse a user-typed value into a safe C literal for a scalar input, or
/// `None` (leave the memset-zero default) for unparseable or non-scalar
/// types. Only literals derived from a successful parse are emitted, so no
/// user text reaches the generated source verbatim.
fn c_literal(ty: &Type, raw: &str) -> Option<String> {
    let raw = raw.trim();
    match ty {
        Type::Bool => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "t" => Some("true".into()),
            "false" | "0" | "f" => Some("false".into()),
            _ => None,
        },
        t if t.is_float() => raw.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| format!("{f}")),
        t if t.is_integer() => raw.parse::<i64>().ok().map(|i| i.to_string()),
        _ => None,
    }
}

/// One `printf` that labels and prints a scalar struct field by type.
fn dbg_field(ty: &Type, access: &str, name: &str) -> String {
    match ty {
        Type::Bool => format!("printf(\"{name}=%s \", {access} ? \"true\" : \"false\");"),
        t if t.is_float() => format!("printf(\"{name}=%g \", (double) {access});"),
        t if t.is_integer() => format!("printf(\"{name}=%lld \", (long long) {access});"),
        _ => format!("printf(\"{name}=? \");"),
    }
}

fn parse_expr(ty: &Type, tok: &str, enums: &EnumNames) -> String {
    if let Some(t) = enum_of(ty, enums) {
        return format!("({}) ol_enum_{}({tok})", ty.c_name(), crate::c_ident(t));
    }
    match ty {
        Type::Bool => format!(
            "((strcmp({tok}, \"true\")==0 || strcmp({tok}, \"1\")==0 || strcmp({tok}, \"t\")==0) ? true : false)"
        ),
        Type::Float32 => format!("strtof({tok}, NULL)"),
        Type::Float64 => format!("strtod({tok}, NULL)"),
        _ => format!("({}) strtoll({tok}, NULL, 10)", ty.c_name()),
    }
}

fn print_stmt(ty: &Type, expr: &str, enums: &EnumNames) -> String {
    if let Some(t) = enum_of(ty, enums) {
        return format!("printf(\"%s\", ol_enum_name_{}((int) {expr}));", crate::c_ident(t));
    }
    match ty {
        Type::Bool => format!("printf({expr} ? \"true\" : \"false\");"),
        Type::Float32 => format!("ol_print_real((double){expr}, 1);"),
        Type::Float64 => format!("ol_print_real({expr}, 0);"),
        _ => format!("printf(\"%lld\", (long long){expr});"),
    }
}
