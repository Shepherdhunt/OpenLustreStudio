use serde::{Deserialize, Serialize};

use crate::diag::SourceSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinOp {
    And,
    Or,
    Xor,
    Implies,
    Eq,
    Neq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "lit")]
pub enum Literal {
    Bool { value: bool },
    Int { value: i64 },
    Float { value: f64 },
}

impl Literal {
    pub fn bool(v: bool) -> Self {
        Literal::Bool { value: v }
    }
    pub fn int(v: i64) -> Self {
        Literal::Int { value: v }
    }
    pub fn float(v: f64) -> Self {
        Literal::Float { value: v }
    }
}

/// Strict expression IR.
///
/// All variants are struct-shaped so they round-trip through JSON/YAML with an
/// internally tagged discriminator. Helper constructors hide the verbosity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "expr")]
pub enum Expr {
    Const {
        lit: Literal,
    },
    Var {
        name: String,
    },
    Unary {
        op: UnaryOp,
        arg: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `if cond then a else b`. Both branches must agree in type.
    IfThenElse {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    /// Previous-value operator. Must always appear as the rhs of an `Arrow`
    /// so that the initial cycle has a value.
    Pre {
        arg: Box<Expr>,
    },
    /// `init -> body` — `init` on the first cycle, `body` thereafter.
    Arrow {
        init: Box<Expr>,
        body: Box<Expr>,
    },
    /// Node or function call. The `node` must resolve to a `NodeDef`.
    Call {
        node: String,
        args: Vec<Expr>,
    },
    /// Record field access.
    Field {
        base: Box<Expr>,
        field: String,
    },
    /// Array index.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
    },
    /// Tuple — used only as the rhs of multi-output equations.
    Tuple {
        items: Vec<Expr>,
    },
}

impl Expr {
    pub fn at(self, _span: SourceSpan) -> Self {
        self
    }

    pub fn bool_lit(v: bool) -> Self {
        Expr::Const { lit: Literal::Bool { value: v } }
    }
    pub fn int_lit(v: i64) -> Self {
        Expr::Const { lit: Literal::Int { value: v } }
    }
    pub fn var<S: Into<String>>(s: S) -> Self {
        Expr::Var { name: s.into() }
    }
    pub fn not(arg: Expr) -> Self {
        Expr::Unary {
            op: UnaryOp::Not,
            arg: Box::new(arg),
        }
    }
    pub fn neg(arg: Expr) -> Self {
        Expr::Unary {
            op: UnaryOp::Neg,
            arg: Box::new(arg),
        }
    }
    pub fn bin(op: BinOp, lhs: Expr, rhs: Expr) -> Self {
        Expr::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }
    pub fn and(lhs: Expr, rhs: Expr) -> Self {
        Self::bin(BinOp::And, lhs, rhs)
    }
    pub fn or(lhs: Expr, rhs: Expr) -> Self {
        Self::bin(BinOp::Or, lhs, rhs)
    }
    pub fn implies(lhs: Expr, rhs: Expr) -> Self {
        Self::bin(BinOp::Implies, lhs, rhs)
    }
    pub fn arrow(init: Expr, body: Expr) -> Self {
        Expr::Arrow {
            init: Box::new(init),
            body: Box::new(body),
        }
    }
    pub fn pre(arg: Expr) -> Self {
        Expr::Pre { arg: Box::new(arg) }
    }
    pub fn if_then_else(cond: Expr, then_branch: Expr, else_branch: Expr) -> Self {
        Expr::IfThenElse {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch: Box::new(else_branch),
        }
    }
    pub fn call<S: Into<String>>(node: S, args: Vec<Expr>) -> Self {
        Expr::Call { node: node.into(), args }
    }

    /// `false -> pre e` — the canonical "edge buffer" pattern.
    pub fn pre_with_init(init: Expr, body: Expr) -> Self {
        Expr::Arrow {
            init: Box::new(init),
            body: Box::new(Expr::Pre { arg: Box::new(body) }),
        }
    }

    /// Walk subexpressions in evaluation order.
    pub fn visit<F: FnMut(&Expr)>(&self, mut f: F) {
        fn walk<F: FnMut(&Expr)>(e: &Expr, f: &mut F) {
            f(e);
            match e {
                Expr::Const { .. } | Expr::Var { .. } => {}
                Expr::Unary { arg, .. } => walk(arg, f),
                Expr::Binary { lhs, rhs, .. } => {
                    walk(lhs, f);
                    walk(rhs, f);
                }
                Expr::IfThenElse {
                    cond,
                    then_branch,
                    else_branch,
                } => {
                    walk(cond, f);
                    walk(then_branch, f);
                    walk(else_branch, f);
                }
                Expr::Pre { arg } => walk(arg, f),
                Expr::Arrow { init, body } => {
                    walk(init, f);
                    walk(body, f);
                }
                Expr::Call { args, .. } => {
                    for a in args {
                        walk(a, f);
                    }
                }
                Expr::Field { base, .. } => walk(base, f),
                Expr::Index { base, index } => {
                    walk(base, f);
                    walk(index, f);
                }
                Expr::Tuple { items } => {
                    for item in items {
                        walk(item, f);
                    }
                }
            }
        }
        walk(self, &mut f);
    }

    /// True if the expression syntactically contains any temporal operator.
    pub fn contains_temporal(&self) -> bool {
        let mut found = false;
        self.visit(|e| {
            if matches!(e, Expr::Pre { .. } | Expr::Arrow { .. }) {
                found = true;
            }
        });
        found
    }

    /// Collect free variable names referenced by this expression.
    pub fn free_vars(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.visit(|e| {
            if let Expr::Var { name } = e {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            }
        });
        out
    }
}
