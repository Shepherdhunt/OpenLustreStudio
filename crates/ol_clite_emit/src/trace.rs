//! Model-to-code traceability for the generated C (what KCG users expect,
//! and what a DO-178C-style review walks through):
//!
//! - every equation's statements in the generated C are preceded by a
//!   one-line `@trace` comment naming the operator, the diagram element it
//!   came from (`eqN`, `sm:Name`, `act:Name`), the owned construct (and the
//!   activation branch) when it was lowered from one, and the equation in
//!   model syntax;
//! - a machine-readable trace matrix ([`TraceEntry`]) maps each of those to
//!   its file and line range — the Studio navigates model ↔ code with it;
//! - a generation report ([`GenerationReport`]) lists the generated files
//!   (lines, bytes, SHA-256), each operator's interface, state and step
//!   function, and the traceability coverage.

use std::fmt::Write as _;

use ol_ir::{ConstructKind, ConstructOrigin, Equation, NodeDef, NodeKind, Project};
use serde::Serialize;

/// The generated C file every step function lives in.
pub const SOURCE_FILE: &str = "openlustre_generated.c";

/// One traced equation: where it came from in the model and where it is in
/// the generated C.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceEntry {
    /// The operator whose step function contains it.
    pub operator: String,
    /// The equation's index in the operator (after constructs are lowered;
    /// the operator's own equations keep their indices).
    pub equation: usize,
    /// The diagram element: `eqN` for the operator's own equations, or the
    /// block of the construct it was lowered from (`sm:Name`, `act:Name`).
    pub element: String,
    /// The owned construct (and activation branch) it was lowered from.
    pub origin: Option<String>,
    pub lhs: Vec<String>,
    /// The equation in model (surface) syntax.
    pub source: String,
    pub file: String,
    /// 1-based, inclusive: the `@trace` comment line through the last line
    /// of the equation's statements.
    pub first_line: usize,
    pub last_line: usize,
}

/// A traced equation's byte span in the source being built (converted to
/// lines once the whole file is known).
pub(crate) struct Span {
    pub operator: String,
    pub equation: usize,
    pub start: usize,
    pub end: usize,
}

/// Where equation `index` of `node` came from: its diagram element, and the
/// construct it was lowered from (if any).
pub fn element_of(project: &Project, node: &NodeDef, index: usize) -> (String, Option<String>) {
    let Some(o) = project.origin_of(&node.name, index) else {
        return (format!("eq{index}"), None);
    };
    let element = format!("{}:{}", o.kind.id_prefix(), o.name);
    let lhs = node.equations.get(index).and_then(|e| e.lhs.first()).map(String::as_str).unwrap_or("");
    let origin = match o.kind {
        ConstructKind::StateMachine => format!("state machine {}", o.name),
        ConstructKind::Activation => format!("activation {}{}", o.name, activation_role(o, lhs)),
    };
    (element, Some(origin))
}

/// Which part of an activation a lowered equation is, from the lowering's
/// naming (`__act_A_b2`, `__act_A_g2`, `__act_A_s2_v`, `__act_A_x2_o`, …).
fn activation_role(o: &ConstructOrigin, lhs: &str) -> String {
    let branch = |tag: &str| -> String {
        if tag == "e" {
            return "else".into();
        }
        tag.parse::<usize>()
            .ok()
            .and_then(|k| o.branches.get(k.wrapping_sub(1)))
            .cloned()
            .unwrap_or_else(|| tag.to_string())
    };
    let Some(rest) = lhs.strip_prefix(&format!("__act_{}_", o.name)) else {
        return format!(", merges {lhs}");
    };
    let split = |r: &str| -> (String, String) {
        let tag: String = r.chars().take_while(|c| c.is_ascii_digit() || *c == 'e').collect();
        (tag.clone(), r[tag.len()..].to_string())
    };
    if let Some(v) = rest.strip_prefix("last_") {
        return format!(", last({v})");
    }
    match rest.chars().next() {
        Some('b') => format!(", selects branch {}", branch(&rest[1..])),
        Some('g') => format!(", guard of branch {}", branch(&rest[1..])),
        Some('s') => {
            let (tag, v) = split(&rest[1..]);
            format!(", branch {} reads {}", branch(&tag), v.trim_start_matches('_'))
        }
        Some('x') => {
            let (tag, v) = split(&rest[1..]);
            format!(", branch {} computes {}", branch(&tag), v.trim_start_matches('_'))
        }
        _ => String::new(),
    }
}

/// The `@trace` comment for an equation — one line, safe inside `/* */`.
pub(crate) fn comment(project: &Project, node: &NodeDef, index: usize, eq: &Equation) -> String {
    let (element, origin) = element_of(project, node, index);
    let mut text = format!("@trace {} {}", node.name, element);
    if let Some(o) = origin {
        let _ = write!(text, " ({o})");
    }
    let _ = write!(text, ": {}", equation_text(eq));
    // Plain ASCII, and never a comment terminator, whatever the model holds.
    let text: String = text
        .replace("*/", "* /")
        .chars()
        .map(|c| if c.is_ascii() && !c.is_ascii_control() { c } else { '?' })
        .collect();
    if text.chars().count() > 160 {
        let cut: String = text.chars().take(157).collect();
        format!("/* {cut}... */")
    } else {
        format!("/* {text} */")
    }
}

fn equation_text(eq: &Equation) -> String {
    let lhs = if eq.lhs.len() == 1 { eq.lhs[0].clone() } else { format!("({})", eq.lhs.join(", ")) };
    format!("{lhs} = {}", ol_lustre_emit::format_expr(&eq.rhs))
}

/// Turn recorded byte spans into trace entries with 1-based line ranges.
pub(crate) fn resolve(project: &Project, source: &str, spans: Vec<Span>) -> Vec<TraceEntry> {
    let newlines: Vec<usize> = source.match_indices('\n').map(|(i, _)| i).collect();
    // Line of a byte offset: newlines before it, plus one.
    let line_of = |off: usize| newlines.partition_point(|&n| n < off) + 1;
    spans
        .into_iter()
        .filter_map(|s| {
            let node = project.find_node(&s.operator)?;
            let eq = node.equations.get(s.equation)?;
            let (element, origin) = element_of(project, node, s.equation);
            Some(TraceEntry {
                operator: s.operator,
                equation: s.equation,
                element,
                origin,
                lhs: eq.lhs.clone(),
                source: equation_text(eq),
                file: SOURCE_FILE.to_string(),
                first_line: line_of(s.start),
                // `end` sits just past the last statement's newline.
                last_line: line_of(s.end.saturating_sub(1)),
            })
        })
        .collect()
}

// --- Generation report --------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub name: String,
    pub lines: usize,
    pub bytes: usize,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OperatorReport {
    pub name: String,
    pub kind: String,
    pub step_function: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub locals: usize,
    pub equations: usize,
    pub traced_equations: usize,
    /// Memory between cycles: `pre` values, held clocked variables, and
    /// clock tick flags in the operator's `_State` struct.
    pub state_fields: usize,
    /// Stateful operator instances embedded in the state.
    pub sub_instances: usize,
    /// Owned constructs lowered into it (`sm:Name`, `act:Name`).
    pub constructs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerationReport {
    pub tool: String,
    pub version: String,
    pub project: String,
    pub root: Option<String>,
    pub files: Vec<FileReport>,
    pub operators: Vec<OperatorReport>,
    pub equations: usize,
    pub traced: usize,
}

/// The report for a generation: `files` are the generated files as written
/// (name, contents); `trace` the matrix of the main source.
pub fn report(project: &Project, root: Option<&str>, files: &[(&str, &str)], trace: &[TraceEntry]) -> GenerationReport {
    let files = files
        .iter()
        .map(|(name, text)| FileReport {
            name: name.to_string(),
            lines: text.lines().count(),
            bytes: text.len(),
            sha256: sha256_hex(text.as_bytes()),
        })
        .collect();
    let mut operators = Vec::new();
    let mut equations = 0;
    for node in crate::topo_sort_nodes(project) {
        if node.is_imported() {
            continue;
        }
        let traced = trace.iter().filter(|t| t.operator == node.name).count();
        equations += node.equations.len();
        let call_sites = crate::compute_call_sites(node);
        let clocks = crate::node_clocks(node);
        let held = node
            .equations
            .iter()
            .zip(&clocks.info.equation_clocks)
            .filter(|(_, ck)| !ck.is_base())
            .map(|(e, _)| e.lhs.len())
            .sum::<usize>();
        let stateful = node.kind != NodeKind::Function;
        let mut constructs: Vec<String> = project
            .origins
            .iter()
            .filter(|o| o.node == node.name)
            .map(|o| format!("{}:{}", o.kind.id_prefix(), o.name))
            .collect();
        constructs.dedup();
        operators.push(OperatorReport {
            name: node.name.clone(),
            kind: format!("{:?}", node.kind),
            step_function: format!("{}_step", node.name),
            inputs: node.inputs.iter().map(|p| format!("{}: {}", p.name, p.ty.c_name())).collect(),
            outputs: node.outputs.iter().map(|p| format!("{}: {}", p.name, p.ty.c_name())).collect(),
            locals: node.locals.len(),
            equations: node.equations.len(),
            traced_equations: traced,
            state_fields: if stateful {
                crate::node_state_fields(node).len() + held + clocks.info.chains.len()
            } else {
                0
            },
            sub_instances: if stateful { crate::stateful_subs(node, &call_sites, project).len() } else { 0 },
            constructs,
        });
    }
    GenerationReport {
        tool: "OpenLustre Studio C-Lite generator".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        project: project.name.clone(),
        root: root.map(str::to_string),
        files,
        operators,
        equations,
        traced: trace.len(),
    }
}

impl GenerationReport {
    /// The report as Markdown — for review packages and the Studio viewer.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# C-Lite generation report — {}\n", self.project);
        let _ = writeln!(s, "- Generator: {} {}", self.tool, self.version);
        let _ = writeln!(s, "- Root operator: {}", self.root.as_deref().unwrap_or("(none)"));
        let _ = writeln!(
            s,
            "- Traceability: {} of {} equations traced to the model{}\n",
            self.traced,
            self.equations,
            if self.traced == self.equations { " (all)" } else { "" }
        );
        let _ = writeln!(s, "## Files\n");
        let _ = writeln!(s, "| file | lines | bytes | SHA-256 |");
        let _ = writeln!(s, "|---|---:|---:|---|");
        for f in &self.files {
            let _ = writeln!(s, "| `{}` | {} | {} | `{}` |", f.name, f.lines, f.bytes, f.sha256);
        }
        let _ = writeln!(s, "\n## Operators\n");
        let _ = writeln!(s, "| operator | kind | step function | inputs | outputs | equations (traced) | state fields | sub-instances | constructs |");
        let _ = writeln!(s, "|---|---|---|---|---|---:|---:|---:|---|");
        for o in &self.operators {
            let _ = writeln!(
                s,
                "| {} | {} | `{}` | {} | {} | {} ({}) | {} | {} | {} |",
                o.name,
                o.kind,
                o.step_function,
                o.inputs.join(", "),
                o.outputs.join(", "),
                o.equations,
                o.traced_equations,
                o.state_fields,
                o.sub_instances,
                if o.constructs.is_empty() { "—".into() } else { o.constructs.join(", ") }
            );
        }
        s
    }
}

// --- SHA-256 (FIPS 180-4), for the report's file fingerprints ------------------

pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_matches_the_fips_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
