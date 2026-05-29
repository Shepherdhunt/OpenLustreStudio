//! Generates a tiny `main()` driver that reads a CSV input vector on stdin,
//! drives the generated `_step` function each cycle, and writes a CSV output
//! trace on stdout matching the format produced by the IR simulator
//! ([`ol_sim::Trace::to_csv`]) for the same node. The two traces are expected
//! to be byte-identical for any model in the Phase 0 profile — that is the
//! invariant Phase 6 trace comparison verifies.

use std::fmt::Write as _;

use ol_ir::{NodeDef, NodeKind, Type};

pub fn emit_csv_driver(node: &NodeDef) -> String {
    emit_csv_driver_with_monitor(node, None)
}

/// Generate a CSV driver. If `monitor_contract_name` is `Some(name)`, the
/// driver also wires in the matching contract monitor and emits
/// `active_mode` and `violations` columns after the outputs — the same shape
/// the IR simulator writes when the node has a contract.
pub fn emit_csv_driver_with_monitor(
    node: &NodeDef,
    monitor_contract_name: Option<&str>,
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
        let _ = writeln!(s, "    in.{} = {};", p.name, parse_expr(&p.ty, "tok"));
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
        let _ = writeln!(s, "    {}", print_stmt(&p.ty, &format!("out.{}", p.name)));
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

fn parse_expr(ty: &Type, tok: &str) -> String {
    match ty {
        Type::Bool => format!(
            "((strcmp({tok}, \"true\")==0 || strcmp({tok}, \"1\")==0 || strcmp({tok}, \"t\")==0) ? true : false)"
        ),
        Type::Float32 | Type::Float64 => format!("strtod({tok}, NULL)"),
        _ => format!("({}) strtoll({tok}, NULL, 10)", ty.c_name()),
    }
}

fn print_stmt(ty: &Type, expr: &str) -> String {
    match ty {
        Type::Bool => format!("printf({expr} ? \"true\" : \"false\");"),
        Type::Float32 | Type::Float64 => format!("printf(\"%g\", (double){expr});"),
        _ => format!("printf(\"%lld\", (long long){expr});"),
    }
}
