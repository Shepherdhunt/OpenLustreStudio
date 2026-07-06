//! `openlustre doc` — render the model into a self-contained design-document
//! HTML: the SCADE Report Generator role. One file, no external assets, no
//! timestamps — the same model always produces byte-identical output, so the
//! document can live under configuration management next to the model.
//!
//! Per user package it renders types, constants, and one section per
//! operator: interface tables, a schematic SVG (stored canvas positions when
//! present, a column layout otherwise), the behavior as Lustre equations,
//! the owned state machine, the CoCoSpec contract, and the requirement
//! annotations; a project-wide traceability matrix closes the report.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ol_ir::{NodeDef, Project};

/// `base_dir` is where relative SysML association paths resolve (the model
/// file's directory); pass `None` to skip reading associated SysML models.
pub fn generate_html(project: &Project, base_dir: Option<&std::path::Path>) -> String {
    let mut body = String::new();
    let user_pkgs: Vec<_> = project.packages.iter().filter(|p| p.name != "stdlib").collect();
    // Associated SysML 2.0 models, read once per distinct path.
    let mut sysml_models: BTreeMap<String, Option<crate::sysml::SysmlModel>> = BTreeMap::new();
    if let Some(dir) = base_dir {
        for pkg in &user_pkgs {
            for n in &pkg.nodes {
                if let Some(sr) = &n.sysml {
                    sysml_models.entry(sr.model.clone()).or_insert_with(|| {
                        std::fs::read_to_string(dir.join(&sr.model))
                            .ok()
                            .map(|s| crate::sysml::parse(&s))
                    });
                }
            }
        }
    }

    // --- Header + table of contents --------------------------------------
    let _ = write!(
        body,
        "<h1>{} — design document</h1>\n<p class=\"meta\">main operator: <code>{}</code> · \
         {} package(s) · {} operator(s)</p>\n",
        esc(&project.name),
        esc(project.main.as_deref().unwrap_or("(none)")),
        user_pkgs.len(),
        user_pkgs.iter().map(|p| p.nodes.len()).sum::<usize>()
    );
    body.push_str("<h2>Contents</h2>\n<ul class=\"toc\">\n");
    for pkg in &user_pkgs {
        for n in &pkg.nodes {
            let _ = write!(body, "<li><a href=\"#op-{0}\">{0}</a></li>\n", esc(&n.name));
        }
    }
    body.push_str("<li><a href=\"#types\">Types &amp; constants</a></li>\n");
    body.push_str("<li><a href=\"#trace\">Requirements traceability</a></li>\n</ul>\n");

    // --- One section per operator -----------------------------------------
    for pkg in &user_pkgs {
        for n in &pkg.nodes {
            let sysml = n
                .sysml
                .as_ref()
                .and_then(|sr| sysml_models.get(&sr.model))
                .and_then(|m| m.as_ref());
            render_node(n, pkg, sysml, &mut body);
        }
    }

    // --- Types & constants --------------------------------------------------
    body.push_str("<h2 id=\"types\">Types &amp; constants</h2>\n");
    let mut any = false;
    for pkg in &user_pkgs {
        if !pkg.types.is_empty() {
            any = true;
            body.push_str("<table><tr><th>Type</th><th>Definition</th></tr>\n");
            for t in &pkg.types {
                let def = match &t.body {
                    ol_ir::TypeBody::Enum(e) => format!("enum {{ {} }}", e.variants.join(", ")),
                    ol_ir::TypeBody::Record { fields, .. } => format!(
                        "record {{ {} }}",
                        fields
                            .iter()
                            .map(|f| format!("{}: {}", f.name, f.ty.lustre_name()))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    ol_ir::TypeBody::Alias { target, .. } => {
                        format!("alias of {}", target.lustre_name())
                    }
                };
                let _ = write!(
                    body,
                    "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>\n",
                    esc(t.name()),
                    esc(&def)
                );
            }
            body.push_str("</table>\n");
        }
        if !pkg.constants.is_empty() {
            any = true;
            body.push_str(
                "<table><tr><th>Constant</th><th>Type</th><th>Value</th></tr>\n",
            );
            for c in &pkg.constants {
                let _ = write!(
                    body,
                    "<tr><td><code>{}</code></td><td><code>{}</code></td><td><code>{}</code></td></tr>\n",
                    esc(&c.name),
                    esc(&c.ty.lustre_name()),
                    esc(&ol_lustre_emit::format_expr(&c.value))
                );
            }
            body.push_str("</table>\n");
        }
    }
    if !any {
        body.push_str("<p class=\"meta\">No named types or constants.</p>\n");
    }

    // --- Traceability matrix -------------------------------------------------
    body.push_str("<h2 id=\"trace\">Requirements traceability</h2>\n");
    let mut by_req: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut untraced: Vec<&str> = Vec::new();
    for pkg in &user_pkgs {
        let (contracts, _) = ol_contract_ir::parse_contracts(&pkg.contracts);
        for n in &pkg.nodes {
            if n.requirements.is_empty() {
                untraced.push(&n.name);
            }
            for r in &n.requirements {
                by_req.entry(r.clone()).or_default().push(n.name.clone());
            }
            // Clause-level links: the rung below the operator.
            let Some(cname) = &n.contract else { continue };
            let Some(c) = contracts.iter().find(|c| &c.name == cname) else { continue };
            for (i, a) in c.assumptions.iter().enumerate() {
                let label = a.name.clone().unwrap_or_else(|| format!("#{i}"));
                for r in &a.requirements {
                    by_req.entry(r.clone()).or_default().push(format!("{} (assume {label})", n.name));
                }
            }
            for (i, g) in c.guarantees.iter().enumerate() {
                let label = g.name.clone().unwrap_or_else(|| format!("#{i}"));
                for r in &g.requirements {
                    by_req.entry(r.clone()).or_default().push(format!("{} (guarantee {label})", n.name));
                }
            }
            for m in &c.modes {
                for r in &m.requirements {
                    by_req.entry(r.clone()).or_default().push(format!("{} (mode {})", n.name, m.name));
                }
            }
        }
    }
    if by_req.is_empty() {
        body.push_str("<p class=\"meta\">No requirement annotations in this model.</p>\n");
    } else {
        body.push_str("<table><tr><th>Requirement</th><th>Implemented by</th></tr>\n");
        for (req, nodes) in &by_req {
            let links = nodes
                .iter()
                .map(|n| {
                    // Link the operator part; clause suffixes stay as text.
                    let (op, rest) = n.split_once(' ').unwrap_or((n.as_str(), ""));
                    format!("<a href=\"#op-{0}\">{0}</a> {rest}", esc(op))
                })
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(body, "<tr><td><code>{}</code></td><td>{links}</td></tr>\n", esc(req));
        }
        body.push_str("</table>\n");
    }
    if !untraced.is_empty() {
        let _ = write!(
            body,
            "<p class=\"warn\">Untraced operator(s): {}</p>\n",
            untraced.iter().map(|n| esc(n)).collect::<Vec<_>>().join(", ")
        );
    }

    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<title>{} — design document</title>\n\
         <style>{CSS}</style></head>\n<body>\n{body}\n\
         <p class=\"meta\">Generated by OpenLustre Studio (<code>openlustre doc</code>).</p>\n\
         </body></html>\n",
        esc(&project.name)
    )
}

const CSS: &str = "\
body{font-family:'Segoe UI',system-ui,sans-serif;color:#1e1e1e;max-width:960px;margin:2em auto;padding:0 1em;line-height:1.45}\
h1{border-bottom:2px solid #2b579a;padding-bottom:.2em}\
h2{color:#2b579a;border-bottom:1px solid #d2dae2;padding-bottom:.15em;margin-top:2em}\
h3{margin-bottom:.3em}\
table{border-collapse:collapse;margin:.5em 0 1em}\
th,td{border:1px solid #c5cdd5;padding:.25em .6em;text-align:left;font-size:.92em}\
th{background:#eef2f6}\
pre{background:#f6f8fa;border:1px solid #d2dae2;padding:.6em .8em;overflow-x:auto;font-size:.9em}\
code{font-family:Consolas,monospace}\
.meta{color:#667;font-size:.9em}\
.warn{color:#a05a00}\
.req{display:inline-block;background:#eef4fb;border:1px solid #2b579a;border-radius:3px;\
padding:0 .45em;margin-right:.3em;font-size:.85em;color:#2b579a}\
svg{border:1px solid #d2dae2;background:#fff;max-width:100%;height:auto}\
ul.toc{columns:2}";

fn render_node(
    n: &NodeDef,
    pkg: &ol_ir::Package,
    sysml: Option<&crate::sysml::SysmlModel>,
    out: &mut String,
) {
    let _ = write!(
        out,
        "<h2 id=\"op-{0}\">{0} <span class=\"meta\">({1:?})</span></h2>\n",
        esc(&n.name),
        n.kind
    );
    if !n.requirements.is_empty() || n.sysml.is_some() {
        out.push_str("<p>");
        for r in &n.requirements {
            let _ = write!(out, "<span class=\"req\">{}</span>", esc(r));
        }
        if let Some(sr) = &n.sysml {
            let label = match &sr.element {
                Some(e) => format!("{}::{e}", sr.model),
                None => sr.model.clone(),
            };
            let _ = write!(
                out,
                "<span class=\"meta\">realizes SysML: <code>{}</code></span>",
                esc(&label)
            );
        }
        out.push_str("</p>\n");
    }

    // Requirements the associated SysML model records as satisfied by this
    // operator (`satisfy R by E`), with their doc text — the model file is
    // the requirements' source of truth.
    if let (Some(sr), Some(sm)) = (&n.sysml, sysml) {
        let last = |s: &str| s.rsplit("::").next().unwrap_or(s).to_string();
        let elem_last = sr.element.as_deref().map(last);
        let satisfied: Vec<_> = sm
            .satisfies
            .iter()
            .filter(|sat| {
                let by_last = last(&sat.by);
                by_last == n.name
                    || sr.element.as_deref() == Some(sat.by.as_str())
                    || elem_last.as_deref() == Some(by_last.as_str())
            })
            .collect();
        if !satisfied.is_empty() {
            out.push_str("<table><tr><th>SysML requirement</th><th>Text</th></tr>\n");
            for sat in satisfied {
                let id = sm.resolve_requirement_id(&sat.requirement);
                let doc = sm
                    .requirements
                    .iter()
                    .find(|r| r.id == id)
                    .and_then(|r| r.doc.as_deref())
                    .unwrap_or("");
                let _ = write!(
                    out,
                    "<tr><td><span class=\"req\">{}</span></td><td>{}</td></tr>\n",
                    esc(&id),
                    esc(doc)
                );
            }
            out.push_str("</table>\n");
        }
    }

    // Interface tables.
    let iface = |title: &str, rows: Vec<(String, String)>, out: &mut String| {
        if rows.is_empty() {
            return;
        }
        let _ = write!(out, "<table><tr><th colspan=\"2\">{title}</th></tr>\n");
        for (name, ty) in rows {
            let _ = write!(
                out,
                "<tr><td><code>{}</code></td><td><code>{}</code></td></tr>\n",
                esc(&name),
                esc(&ty)
            );
        }
        out.push_str("</table>\n");
    };
    iface(
        "Inputs",
        n.inputs.iter().map(|p| (p.name.clone(), p.ty.lustre_name())).collect(),
        out,
    );
    iface(
        "Outputs",
        n.outputs.iter().map(|p| (p.name.clone(), p.ty.lustre_name())).collect(),
        out,
    );
    iface(
        "Locals",
        n.locals.iter().map(|l| (l.name.clone(), l.ty.lustre_name())).collect(),
        out,
    );

    // Schematic.
    if !n.equations.is_empty() {
        out.push_str(&render_schematic(n));
    }

    // Behavior as Lustre.
    if !n.equations.is_empty() {
        out.push_str("<h3>Behavior</h3>\n<pre>");
        for eq in &n.equations {
            let lhs = if eq.lhs.len() == 1 {
                eq.lhs[0].clone()
            } else {
                format!("({})", eq.lhs.join(", "))
            };
            let _ = write!(out, "{} = {};\n", esc(&lhs), esc(&ol_lustre_emit::format_expr(&eq.rhs)));
        }
        out.push_str("</pre>\n");
    }

    // The operator-owned state machine, if any.
    for m in pkg.state_machines.iter().filter(|m| m.owner.as_deref() == Some(&n.name)) {
        let _ = write!(out, "<h3>State machine: {}</h3>\n", esc(&m.name));
        if !m.signals.is_empty() {
            let _ = write!(
                out,
                "<p><span class=\"meta\">signals: <code>{}</code></span></p>\n",
                esc(&m.signals.join(", "))
            );
        }
        out.push_str("<table><tr><th>State</th><th>Transitions</th><th>Equations</th></tr>\n");
        for s in &m.states {
            let marker = if s.name == m.initial_state { " <b>(initial)</b>" } else { "" };
            let trans = s
                .transitions
                .iter()
                .map(|t| {
                    format!(
                        "if {} → {}",
                        esc(&ol_lustre_emit::format_expr(&t.guard)),
                        esc(&t.target)
                    )
                })
                .collect::<Vec<_>>()
                .join("<br>");
            let eqs = s
                .equations
                .iter()
                .map(|e| {
                    format!(
                        "{} = {}",
                        esc(&e.lhs.join(", ")),
                        esc(&ol_lustre_emit::format_expr(&e.rhs))
                    )
                })
                .chain(s.emits.iter().map(|sig| format!("emit {}", esc(sig))))
                .collect::<Vec<_>>()
                .join("<br>");
            let _ = write!(
                out,
                "<tr><td><code>{}</code>{marker}</td><td><code>{trans}</code></td><td><code>{eqs}</code></td></tr>\n",
                esc(&s.name)
            );
        }
        out.push_str("</table>\n");
    }

    // The CoCoSpec contract, if the node references one.
    if let Some(cname) = &n.contract {
        let (contracts, _) = ol_contract_ir::parse_contracts(&pkg.contracts);
        if let Some(c) = contracts.iter().find(|c| &c.name == cname) {
            let _ = write!(out, "<h3>Contract: {}</h3>\n", esc(&c.name));
            out.push_str("<pre>");
            let tag = |reqs: &[String]| {
                if reqs.is_empty() { String::new() } else { format!("  -- [{}]", reqs.join(", ")) }
            };
            for a in &c.assumptions {
                let _ = write!(
                    out,
                    "assume {};{}\n",
                    esc(&ol_lustre_emit::format_expr(&a.expr)),
                    esc(&tag(&a.requirements))
                );
            }
            for g in &c.guarantees {
                let _ = write!(
                    out,
                    "guarantee {};{}\n",
                    esc(&ol_lustre_emit::format_expr(&g.expr)),
                    esc(&tag(&g.requirements))
                );
            }
            for m in &c.modes {
                let _ = write!(out, "mode {} ({}\n", esc(&m.name), esc(&tag(&m.requirements)));
                for r in &m.requires {
                    let _ = write!(out, "  require {};\n", esc(&ol_lustre_emit::format_expr(r)));
                }
                for e in &m.ensures {
                    let _ = write!(out, "  ensure {};\n", esc(&ol_lustre_emit::format_expr(e)));
                }
                out.push_str(");\n");
            }
            out.push_str("</pre>\n");
        }
    }
}

/// A static schematic of the operator's dataflow: inputs on the left,
/// equation blocks in the middle (labeled by their defined names), outputs
/// on the right, Manhattan wires — canvas positions when the model carries
/// them, a plain column layout otherwise.
fn render_schematic(n: &NodeDef) -> String {
    const BW: f64 = 150.0; // box width
    const EW: f64 = 170.0; // equation box width
    const BH: f64 = 26.0;
    const VGAP: f64 = 42.0;

    // id -> (x, y, w, label)
    let mut boxes: BTreeMap<String, (f64, f64, f64, String)> = BTreeMap::new();
    let mut place = |id: &str, def_x: f64, def_y: f64, w: f64, label: String, n: &NodeDef| {
        let (x, y) = n
            .diagram
            .positions
            .get(id)
            .map(|p| (p.x, p.y))
            .unwrap_or((def_x, def_y));
        boxes.insert(id.to_string(), (x, y, w, label));
    };
    for (i, p) in n.inputs.iter().enumerate() {
        place(&p.name, 16.0, 16.0 + i as f64 * VGAP, BW,
              format!("{} : {}", p.name, p.ty.lustre_name()), n);
    }
    for (i, eq) in n.equations.iter().enumerate() {
        let text = format!("{}", eq.lhs.join(", "));
        place(&format!("eq{i}"), 230.0, 16.0 + i as f64 * VGAP, EW, text, n);
    }
    for (i, l) in n.locals.iter().enumerate() {
        // Locals that are some equation's lhs live inside that block's label;
        // only free-standing ones (never defined) get their own box.
        if n.equations.iter().any(|e| e.lhs.contains(&l.name)) {
            continue;
        }
        place(&l.name, 460.0, 16.0 + i as f64 * VGAP, BW,
              format!("{} : {}", l.name, l.ty.lustre_name()), n);
    }
    for (i, p) in n.outputs.iter().enumerate() {
        place(&p.name, 640.0, 16.0 + i as f64 * VGAP, BW,
              format!("{} : {}", p.name, p.ty.lustre_name()), n);
    }

    // Wires: reads into each equation, writes out of it — same derivation the
    // Studio canvas uses (free variables + lhs).
    let mut wires: Vec<(String, String)> = Vec::new();
    let defined_by: BTreeMap<&str, usize> = n
        .equations
        .iter()
        .enumerate()
        .flat_map(|(i, e)| e.lhs.iter().map(move |l| (l.as_str(), i)))
        .collect();
    for (i, eq) in n.equations.iter().enumerate() {
        let me = format!("eq{i}");
        for v in eq.rhs.free_vars() {
            // A read of another equation's result draws from that block.
            let from = match defined_by.get(v.as_str()) {
                Some(j) if *j != i => format!("eq{j}"),
                _ => v.clone(),
            };
            if boxes.contains_key(&from) && from != me {
                wires.push((from, me.clone()));
            }
        }
        for l in &eq.lhs {
            if boxes.contains_key(l.as_str()) {
                wires.push((me.clone(), l.clone()));
            }
        }
    }
    wires.sort();
    wires.dedup();

    let mut maxx: f64 = 0.0;
    let mut maxy: f64 = 0.0;
    for (x, y, w, _) in boxes.values() {
        maxx = maxx.max(x + w);
        maxy = maxy.max(y + BH);
    }
    let (w, h) = (maxx + 24.0, maxy + 16.0);
    let mut svg = format!(
        "<svg viewBox=\"0 0 {w} {h}\" width=\"{w}\" xmlns=\"http://www.w3.org/2000/svg\" \
         font-family=\"Consolas,monospace\" font-size=\"11\">\n"
    );
    for (from, to) in &wires {
        let (fx, fy, fw, _) = &boxes[from];
        let (tx, ty, _, _) = &boxes[to];
        let (x1, y1) = (fx + fw, fy + BH / 2.0);
        let (x2, y2) = (*tx, ty + BH / 2.0);
        let mx = (x1 + x2) / 2.0;
        let _ = write!(
            svg,
            "<path d=\"M {x1} {y1} L {mx} {y1} L {mx} {y2} L {x2} {y2}\" \
             fill=\"none\" stroke=\"#4a4a4a\" stroke-width=\"1.1\"/>\n"
        );
    }
    for (id, (x, y, w, label)) in &boxes {
        let eq = id.starts_with("eq");
        let fill = if eq { "#eef4fb" } else { "#fff" };
        let _ = write!(
            svg,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{BH}\" rx=\"2\" \
             fill=\"{fill}\" stroke=\"#2b579a\"/>\n<text x=\"{}\" y=\"{}\">{}</text>\n",
            x + 7.0,
            y + BH / 2.0 + 4.0,
            esc(label)
        );
    }
    svg.push_str("</svg>\n");
    format!("<h3>Schematic</h3>\n{svg}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_is_deterministic_and_self_contained() {
        let project: Project = serde_json::from_value(serde_json::json!({
            "name": "demo",
            "main": "Top",
            "packages": [{
                "name": "user",
                "nodes": [{
                    "name": "Top",
                    "kind": "Operator",
                    "inputs": [{"name": "x", "ty": {"kind": "Int32"}}],
                    "outputs": [{"name": "y", "ty": {"kind": "Int32"}}],
                    "requirements": ["SRS-1"],
                    "equations": [{"lhs": ["y"],
                        "rhs": {"expr": "Binary", "op": "Add",
                                "lhs": {"expr": "Var", "name": "x"},
                                "rhs": {"expr": "Const", "lit": {"lit": "Int", "value": 1}}}}]
                }]
            }]
        }))
        .unwrap();
        let a = generate_html(&project, None);
        let b = generate_html(&project, None);
        assert_eq!(a, b, "same model must produce byte-identical documents");
        for needle in ["<h2 id=\"op-Top\">", "SRS-1", "y = x + 1;", "<svg"] {
            assert!(a.contains(needle), "missing {needle}:\n{a}");
        }
        // Self-contained: no fetched external resources (the SVG xmlns is a
        // namespace identifier, not a network request).
        let stripped = a.replace("http://www.w3.org/2000/svg", "");
        assert!(
            !stripped.contains("http://") && !stripped.contains("https://"),
            "external link found"
        );
    }
}
