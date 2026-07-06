//! A minimal reader for the SysML 2.0 **textual notation**, covering exactly
//! what the requirements-traceability story needs: requirement definitions
//! and usages (with their short-name IDs and `doc` bodies) and `satisfy`
//! relationships. Everything else in the file — parts, attributes, actions,
//! imports — is tolerated and skipped, so a real system model authored in a
//! SysML v2 tool reads fine as long as its requirements use the standard
//! forms:
//!
//! ```text
//! package Flight {
//!     requirement def <'SRS-042'> InterlockReq {
//!         doc /* The release chain shall require arm consent. */
//!     }
//!     requirement <'SRS-107'> stationReq;
//!     part def ReleaseFunction;
//!     satisfy InterlockReq by ReleaseFunction;
//! }
//! ```
//!
//! The requirement's **ID** is its short name (`<'SRS-042'>`, quotes
//! optional) when present, else its declared name. A `satisfy R by E;`
//! statement links requirement `R` (by ID or declared name) to element `E`
//! (qualified names are kept verbatim).

/// One requirement (definition or usage) found in the model.
#[derive(Debug, Clone, PartialEq)]
pub struct SysmlRequirement {
    /// The traceability ID: short name if declared, else the element name.
    pub id: String,
    /// The declared element name, if any (anonymous usages have none).
    pub name: Option<String>,
    /// First `doc` body inside the requirement, whitespace-normalized.
    pub doc: Option<String>,
}

/// A `satisfy R by E;` relationship.
#[derive(Debug, Clone, PartialEq)]
pub struct SysmlSatisfy {
    /// Requirement reference as written (matched against IDs and names).
    pub requirement: String,
    /// Satisfying element as written (possibly qualified, `Pkg::Part`).
    pub by: String,
}

/// The requirements-relevant slice of one `.sysml` file.
#[derive(Debug, Clone, Default)]
pub struct SysmlModel {
    pub requirements: Vec<SysmlRequirement>,
    pub satisfies: Vec<SysmlSatisfy>,
}

impl SysmlModel {
    /// Is `id` a known requirement (by ID or by declared name)?
    pub fn has_requirement(&self, id: &str) -> bool {
        self.requirements
            .iter()
            .any(|r| r.id == id || r.name.as_deref() == Some(id))
    }

    /// The ID a `satisfy` reference resolves to: the referenced requirement's
    /// short-name ID when the reference names a known requirement, else the
    /// reference verbatim.
    pub fn resolve_requirement_id(&self, reference: &str) -> String {
        self.requirements
            .iter()
            .find(|r| r.id == reference || r.name.as_deref() == Some(reference))
            .map(|r| r.id.clone())
            .unwrap_or_else(|| reference.to_string())
    }
}

/// Tokens: identifiers/qualified names, short names (`<...>` with the quotes
/// and brackets stripped), punctuation, and `doc` comment bodies.
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(String),
    Short(String),
    Punct(char),
    /// The `/* ... */` body following a `doc` keyword, normalized.
    Comment(String),
}

fn tokenize(src: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let b: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            let start = i + 2;
            let mut j = start;
            while j + 1 < b.len() && !(b[j] == '*' && b[j + 1] == '/') {
                j += 1;
            }
            let body: String = b[start..j.min(b.len())].iter().collect();
            let norm = body.split_whitespace().collect::<Vec<_>>().join(" ");
            toks.push(Tok::Comment(norm));
            i = (j + 2).min(b.len());
        } else if c == '<' {
            // Short name: <'SRS-042'> or <SRS_042>. Take through the '>'.
            let mut j = i + 1;
            while j < b.len() && b[j] != '>' {
                j += 1;
            }
            let inner: String = b[i + 1..j.min(b.len())].iter().collect();
            let trimmed = inner.trim().trim_matches('\'').to_string();
            toks.push(Tok::Short(trimmed));
            i = (j + 1).min(b.len());
        } else if c == '\'' {
            // A quoted (unrestricted) name outside brackets.
            let mut j = i + 1;
            while j < b.len() && b[j] != '\'' {
                j += 1;
            }
            let inner: String = b[i + 1..j.min(b.len())].iter().collect();
            toks.push(Tok::Word(inner));
            i = (j + 1).min(b.len());
        } else if c.is_alphanumeric() || c == '_' {
            let mut j = i;
            let mut word = String::new();
            while j < b.len() {
                let d = b[j];
                if d.is_alphanumeric() || d == '_' {
                    word.push(d);
                    j += 1;
                } else if d == ':' && j + 1 < b.len() && b[j + 1] == ':' {
                    // Keep qualified names (`Pkg::Part`) as one word.
                    word.push_str("::");
                    j += 2;
                } else {
                    break;
                }
            }
            toks.push(Tok::Word(word));
            i = j;
        } else {
            toks.push(Tok::Punct(c));
            i += 1;
        }
    }
    toks
}

/// Read the requirements-relevant elements out of SysML 2.0 text. This never
/// fails: unrecognized constructs are skipped (the file is some other tool's
/// artifact — reading it must be safe).
pub fn parse(src: &str) -> SysmlModel {
    let toks = tokenize(src);
    let mut model = SysmlModel::default();
    let word = |t: &Tok| -> Option<String> {
        match t {
            Tok::Word(w) => Some(w.clone()),
            _ => None,
        }
    };
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Word(w) if w == "requirement" => {
                // requirement [def] [<short>] [Name] [: Def] [ { ...doc... } | ; ]
                let mut j = i + 1;
                if matches!(&toks.get(j), Some(Tok::Word(k)) if k == "def") {
                    j += 1;
                }
                let mut short = None;
                if let Some(Tok::Short(s)) = toks.get(j) {
                    short = Some(s.clone());
                    j += 1;
                }
                let mut name = None;
                if let Some(n) = toks.get(j).and_then(|t| word(t)) {
                    name = Some(n);
                    j += 1;
                }
                // Skip a `: Definition` specialization.
                if matches!(toks.get(j), Some(Tok::Punct(':'))) {
                    j += 1;
                    if toks.get(j).map(|t| word(t).is_some()).unwrap_or(false) {
                        j += 1;
                    }
                }
                // Body: capture the first `doc /* ... */` at THIS nesting
                // level; recurse-free brace skip for everything else.
                let mut doc = None;
                if matches!(toks.get(j), Some(Tok::Punct('{'))) {
                    let mut depth = 1;
                    j += 1;
                    while j < toks.len() && depth > 0 {
                        match &toks[j] {
                            Tok::Punct('{') => depth += 1,
                            Tok::Punct('}') => depth -= 1,
                            Tok::Word(k) if k == "doc" && depth == 1 && doc.is_none() => {
                                if let Some(Tok::Comment(c)) = toks.get(j + 1) {
                                    doc = Some(c.clone());
                                }
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                }
                let id = short.clone().or_else(|| name.clone());
                if let Some(id) = id {
                    model.requirements.push(SysmlRequirement { id, name, doc });
                }
                i = j.max(i + 1);
            }
            Tok::Word(w) if w == "satisfy" => {
                // satisfy [requirement] R by E ;
                let mut j = i + 1;
                if matches!(&toks.get(j), Some(Tok::Word(k)) if k == "requirement") {
                    j += 1;
                }
                let req = toks.get(j).and_then(|t| word(t));
                let by_kw = matches!(&toks.get(j + 1), Some(Tok::Word(k)) if k == "by");
                let elem = toks.get(j + 2).and_then(|t| word(t));
                if let (Some(requirement), true, Some(by)) = (req, by_kw, elem) {
                    model.satisfies.push(SysmlSatisfy { requirement, by });
                    i = j + 3;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definitions_usages_ids_and_docs() {
        let m = parse(
            r#"
            package Flight {
                // The release chain's requirements.
                requirement def <'SRS-042'> InterlockReq {
                    doc /* The release chain
                           shall require arm consent. */
                }
                requirement <'SRS-107'> stationReq;
                requirement def PlainReq;
                part def ReleaseFunction {
                    attribute mass : Real;
                }
            }
            "#,
        );
        assert_eq!(m.requirements.len(), 3, "{:?}", m.requirements);
        let ilk = &m.requirements[0];
        assert_eq!(ilk.id, "SRS-042");
        assert_eq!(ilk.name.as_deref(), Some("InterlockReq"));
        assert_eq!(ilk.doc.as_deref(), Some("The release chain shall require arm consent."));
        assert_eq!(m.requirements[1].id, "SRS-107");
        assert_eq!(m.requirements[2].id, "PlainReq");
        assert!(m.has_requirement("SRS-042"));
        assert!(m.has_requirement("InterlockReq"), "declared name matches too");
        assert!(!m.has_requirement("SRS-999"));
    }

    #[test]
    fn satisfy_statements_link_requirements_to_elements() {
        let m = parse(
            r#"
            requirement def <'SRS-042'> InterlockReq;
            satisfy InterlockReq by ReleaseFunction;
            satisfy requirement 'SRS-042' by Pkg::Station;
            "#,
        );
        assert_eq!(m.satisfies.len(), 2, "{:?}", m.satisfies);
        assert_eq!(m.satisfies[0].requirement, "InterlockReq");
        assert_eq!(m.satisfies[0].by, "ReleaseFunction");
        assert_eq!(m.satisfies[1].requirement, "SRS-042");
        assert_eq!(m.satisfies[1].by, "Pkg::Station");
        // Both references resolve to the short-name ID.
        assert_eq!(m.resolve_requirement_id("InterlockReq"), "SRS-042");
        assert_eq!(m.resolve_requirement_id("SRS-042"), "SRS-042");
        assert_eq!(m.resolve_requirement_id("Unknown"), "Unknown");
    }

    #[test]
    fn unrecognized_constructs_are_skipped_not_fatal(){
        let m = parse("part def X { action go; } junk %% tokens <'dangling");
        assert!(m.requirements.is_empty());
        assert!(m.satisfies.is_empty());
    }
}
