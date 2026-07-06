//! Generates a tiny `main()` driver that reads a CSV input vector on stdin,
//! drives the generated `_step` function each cycle, and writes a CSV output
//! trace on stdout matching the format produced by the IR simulator
//! ([`ol_sim::Trace::to_csv`]) for the same node. The two traces are expected
//! to be byte-identical for any model in the Phase 0 profile — that is the
//! invariant Phase 6 trace comparison verifies.

use std::fmt::Write as _;

use ol_ir::{NodeDef, NodeKind, Project, Type};

pub fn emit_csv_driver(node: &NodeDef, project: &Project) -> String {
    emit_csv_driver_with_monitor(node, None, project)
}

/// Generate a CSV driver. If `monitor_contract_name` is `Some(name)`, the
/// driver also wires in the matching contract monitor and emits
/// `active_mode` and `violations` columns after the outputs — the same shape
/// the IR simulator writes when the node has a contract. The project supplies
/// enum definitions so enum-typed I/O reads and writes variant *names*,
/// matching the IR trace byte for byte.
pub fn emit_csv_driver_with_monitor(
    node: &NodeDef,
    monitor_contract_name: Option<&str>,
    project: &Project,
) -> String {
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

    let _ = writeln!(s, "  int cycle = 0;");
    let _ = writeln!(s, "  while (fgets(line, sizeof(line), stdin)) {{");
    let _ = writeln!(s, "    line[strcspn(line, \"\\r\\n\")] = 0;");
    let _ = writeln!(s, "    if (line[0] == 0) continue;");
    let _ = writeln!(s, "    char* tok = strtok(line, \",\");");
    for p in &node.inputs {
        let _ = writeln!(s, "    if (!tok) return 1;");
        match &p.ty {
            Type::Array { elem, len } => emit_array_parse(&mut s, &crate::c_ident(&p.name), elem, *len),
            _ => {
                let _ = writeln!(
                    s,
                    "    in.{} = {};",
                    crate::c_ident(&p.name),
                    parse_expr(&p.ty, "tok", project)
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
            Type::Array { elem, len } => emit_array_print(&mut s, &crate::c_ident(&p.name), elem, *len),
            _ => {
                let _ = writeln!(
                    s,
                    "    {}",
                    print_stmt(&p.ty, &format!("out.{}", crate::c_ident(&p.name)), project)
                );
            }
        }
    }
    if monitor_contract_name.is_some() {
        let _ = writeln!(s, "    printf(\",%s,%s\", mode_buf, viol_buf);");
    }
    let _ = writeln!(s, "    printf(\"\\n\");");
    let _ = writeln!(s, "    cycle++;");
    let _ = writeln!(s, "  }}");
    let _ = writeln!(s, "  return 0;");
    let _ = writeln!(s, "}}");
    s
}

/// Parse a bracketed `[e0;e1;…]` token into `in.<name>[k]`. `strtoll`/`strtod`
/// advance a cursor past each element; we skip the `[` and `;` separators by
/// hand (strtok is already in use on the outer comma split, so no nesting).
fn emit_array_parse(s: &mut String, name: &str, elem: &Type, len: u32) {
    let is_float = elem.is_float();
    let read = if is_float { "strtod(__p, &__e)" } else { "strtoll(__p, &__e, 10)" };
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
fn emit_array_print(s: &mut String, name: &str, elem: &Type, len: u32) {
    let item = if elem.is_float() {
        format!("printf(\"%g\", (double) out.{name}[__k]);")
    } else {
        format!("printf(\"%lld\", (long long) out.{name}[__k]);")
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

fn parse_expr(ty: &Type, tok: &str, project: &Project) -> String {
    match ty {
        Type::Bool => format!(
            "((strcmp({tok}, \"true\")==0 || strcmp({tok}, \"1\")==0 || strcmp({tok}, \"t\")==0) ? true : false)"
        ),
        Type::Float32 | Type::Float64 => format!("strtod({tok}, NULL)"),
        // An enum column carries the variant name; a strcmp chain maps it to
        // the C enum constant. The IR simulator validates every input vector
        // first, so an unknown token cannot reach a recorded scenario; the
        // final fallback keeps the driver total.
        Type::Named { name } => match enum_variants(project, name) {
            Some(vs) if !vs.is_empty() => {
                let mut t = vs[0].clone();
                for v in vs.iter().rev() {
                    t = format!("(strcmp({tok}, \"{v}\")==0 ? {v} : {t})");
                }
                t
            }
            _ => format!("({}) strtoll({tok}, NULL, 10)", ty.c_name()),
        },
        _ => format!("({}) strtoll({tok}, NULL, 10)", ty.c_name()),
    }
}

fn print_stmt(ty: &Type, expr: &str, project: &Project) -> String {
    match ty {
        Type::Bool => format!("printf({expr} ? \"true\" : \"false\");"),
        Type::Float32 | Type::Float64 => format!("printf(\"%g\", (double){expr});"),
        // Enum outputs print their variant name — exactly what the IR trace
        // writes, so the equivalence comparison stays byte-for-byte.
        Type::Named { name } => match enum_variants(project, name) {
            Some(vs) if !vs.is_empty() => {
                let mut t = format!("\"{}\"", vs[vs.len() - 1]);
                for v in &vs[..vs.len() - 1] {
                    t = format!("({expr} == {v} ? \"{v}\" : {t})");
                }
                format!("printf(\"%s\", {t});")
            }
            _ => format!("printf(\"%lld\", (long long){expr});"),
        },
        _ => format!("printf(\"%lld\", (long long){expr});"),
    }
}

/// The variant list of a named enum, when the name resolves to one.
fn enum_variants<'a>(project: &'a Project, name: &str) -> Option<&'a Vec<String>> {
    project.packages.iter().flat_map(|p| &p.types).find_map(|t| match &t.body {
        ol_ir::TypeBody::Enum(e) if e.name == name => Some(&e.variants),
        _ => None,
    })
}
