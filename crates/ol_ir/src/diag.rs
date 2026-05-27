use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SourceSpan {
    pub file: Option<String>,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub span: SourceSpan,
    #[serde(default)]
    pub context: Vec<String>,
}

impl Diagnostic {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            span: SourceSpan::default(),
            context: Vec::new(),
        }
    }
    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.into(),
            message: message.into(),
            span: SourceSpan::default(),
            context: Vec::new(),
        }
    }
    pub fn info(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            code: code.into(),
            message: message.into(),
            span: SourceSpan::default(),
            context: Vec::new(),
        }
    }

    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        self.context.push(ctx.into());
        self
    }

    pub fn render(&self) -> String {
        let tag = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        };
        let mut s = format!("{tag}[{}]: {}", self.code, self.message);
        for c in &self.context {
            s.push_str(&format!("\n    in {c}"));
        }
        s
    }
}
