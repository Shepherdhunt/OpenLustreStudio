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
    /// Bitwise AND on integer operands.
    BitAnd,
    /// Bitwise OR on integer operands.
    BitOr,
    /// Bitwise XOR on integer operands.
    BitXor,
    /// Left shift; operands must be integers.
    Shl,
    /// Right shift (logical for unsigned, arithmetic for signed).
    Shr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "lit")]
pub enum Literal {
    Bool { value: bool },
    Int { value: i64 },
    Float { value: f64 },
    /// A character literal `'a'`, stored as its byte value. Typed as
    /// [`crate::types::Type::Char`]; a string `"ab"` lowers to an
    /// [`Expr::Array`] of these.
    Char { value: u8 },
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
    pub fn char(v: u8) -> Self {
        Literal::Char { value: v }
    }
}

/// One `field: value` initializer in an [`Expr::Struct`] record literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldInit {
    pub field: String,
    pub value: Expr,
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
    /// Array literal `[e0; e1; …]`. All items share one element type; the
    /// array's length is the item count. A string literal `"ab"` is an
    /// `Array` of [`Literal::Char`] constants.
    Array {
        items: Vec<Expr>,
    },
    /// Record (struct) literal `Name { field: value, … }`, constructing a
    /// value of the named record type. Field order is normalized to the
    /// type's declared order by the type checker / emitters.
    Struct {
        /// The record type being constructed.
        ty: String,
        fields: Vec<FieldInit>,
    },
    /// Explicit numeric conversion — SCADE's `numeric_cast`. Surface syntax
    /// is function-style: `int16(x)`, `float64(x)`. Both the operand and the
    /// target must be numeric; semantics match a C cast for in-range values.
    Cast {
        to: crate::types::Type,
        arg: Box<Expr>,
    },
    /// `arg when clock` / `arg when not clock`: sample a stream on the cycles
    /// where the boolean variable `clock` is true (`on: true`) or false.
    /// Clock conditions are variable names — the classic Lustre restriction —
    /// so every backend can test them cheaply and statically.
    When {
        arg: Box<Expr>,
        clock: String,
        on: bool,
    },
    /// `merge(clock, on_true, on_false)`: join two complementary clocked
    /// streams (`on_true` on `clock`'s true cycles, `on_false` on its false
    /// cycles) back onto the clock `clock` itself runs on.
    Merge {
        clock: String,
        on_true: Box<Expr>,
        on_false: Box<Expr>,
    },
    /// An array iterator: `map(F, a₁…aₖ)` applies the named function `F`
    /// element-wise across same-length arrays to produce an array;
    /// `fold(F, init, a)` left-folds `F(acc, elem)` over an array to a
    /// scalar (`init` is the accumulator seed). `node` names a stateless
    /// function — the iterated body has no per-element state in this profile.
    Iterate {
        kind: IterKind,
        node: String,
        /// The fold accumulator seed; `None` for `map`.
        init: Option<Box<Expr>>,
        /// The array operands (one for `fold`, one or more for `map`).
        arrays: Vec<Expr>,
    },
    /// A floating-point intrinsic — SCADE-style math built-ins (`sqrt`, `sin`,
    /// `cos`, `abs`, `min`, `max`, …) that cannot be expressed with the core
    /// operators. Surface syntax is function-style: `sqrt(x)`, `min(a, b)`.
    /// Every operand and the result are floating-point of one width; the
    /// generated C calls `<math.h>` (the `f`-suffixed variant for `float32`),
    /// and the Kind 2 view emits the same call as a user-suppliable function.
    Intrinsic {
        func: FloatFn,
        args: Vec<Expr>,
    },
}

/// Which array iterator an [`Expr::Iterate`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IterKind {
    Map,
    Fold,
}

/// A floating-point intrinsic in [`Expr::Intrinsic`]. Each maps to a
/// `<math.h>` function in generated C; the variants are exactly the math
/// built-ins that the core operators cannot express.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FloatFn {
    Sqrt,
    Sin,
    Cos,
    Tan,
    Exp,
    /// Natural logarithm.
    Log,
    /// Absolute value (C `fabs`).
    Abs,
    /// Lesser of two operands (C `fmin`).
    Min,
    /// Greater of two operands (C `fmax`).
    Max,
    /// `pow(base, exponent)`.
    Pow,
}

impl FloatFn {
    /// The surface (and Kind 2) name, e.g. `sqrt`. Round-trips through
    /// [`FloatFn::from_name`].
    pub fn name(self) -> &'static str {
        match self {
            FloatFn::Sqrt => "sqrt",
            FloatFn::Sin => "sin",
            FloatFn::Cos => "cos",
            FloatFn::Tan => "tan",
            FloatFn::Exp => "exp",
            FloatFn::Log => "log",
            FloatFn::Abs => "abs",
            FloatFn::Min => "min",
            FloatFn::Max => "max",
            FloatFn::Pow => "pow",
        }
    }

    /// Parse a surface name into an intrinsic, or `None` if it is not one.
    pub fn from_name(name: &str) -> Option<FloatFn> {
        Some(match name {
            "sqrt" => FloatFn::Sqrt,
            "sin" => FloatFn::Sin,
            "cos" => FloatFn::Cos,
            "tan" => FloatFn::Tan,
            "exp" => FloatFn::Exp,
            "log" => FloatFn::Log,
            "abs" => FloatFn::Abs,
            "min" => FloatFn::Min,
            "max" => FloatFn::Max,
            "pow" => FloatFn::Pow,
            _ => return None,
        })
    }

    /// How many operands the intrinsic takes (1 for unary, 2 for binary).
    pub fn arity(self) -> usize {
        match self {
            FloatFn::Min | FloatFn::Max | FloatFn::Pow => 2,
            _ => 1,
        }
    }

    /// The `<math.h>` function name to emit in C. `float32` selects the
    /// single-precision (`f`-suffixed) variant so the generated code computes
    /// in the operand's own width.
    pub fn c_name(self, float32: bool) -> &'static str {
        match (self, float32) {
            (FloatFn::Sqrt, false) => "sqrt",
            (FloatFn::Sqrt, true) => "sqrtf",
            (FloatFn::Sin, false) => "sin",
            (FloatFn::Sin, true) => "sinf",
            (FloatFn::Cos, false) => "cos",
            (FloatFn::Cos, true) => "cosf",
            (FloatFn::Tan, false) => "tan",
            (FloatFn::Tan, true) => "tanf",
            (FloatFn::Exp, false) => "exp",
            (FloatFn::Exp, true) => "expf",
            (FloatFn::Log, false) => "log",
            (FloatFn::Log, true) => "logf",
            (FloatFn::Abs, false) => "fabs",
            (FloatFn::Abs, true) => "fabsf",
            (FloatFn::Min, false) => "fmin",
            (FloatFn::Min, true) => "fminf",
            (FloatFn::Max, false) => "fmax",
            (FloatFn::Max, true) => "fmaxf",
            (FloatFn::Pow, false) => "pow",
            (FloatFn::Pow, true) => "powf",
        }
    }
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
    pub fn cast(to: crate::types::Type, arg: Expr) -> Self {
        Expr::Cast { to, arg: Box::new(arg) }
    }
    pub fn when<S: Into<String>>(arg: Expr, clock: S, on: bool) -> Self {
        Expr::When { arg: Box::new(arg), clock: clock.into(), on }
    }
    pub fn merge<S: Into<String>>(clock: S, on_true: Expr, on_false: Expr) -> Self {
        Expr::Merge {
            clock: clock.into(),
            on_true: Box::new(on_true),
            on_false: Box::new(on_false),
        }
    }
    pub fn map<S: Into<String>>(node: S, arrays: Vec<Expr>) -> Self {
        Expr::Iterate { kind: IterKind::Map, node: node.into(), init: None, arrays }
    }
    pub fn fold<S: Into<String>>(node: S, init: Expr, array: Expr) -> Self {
        Expr::Iterate {
            kind: IterKind::Fold,
            node: node.into(),
            init: Some(Box::new(init)),
            arrays: vec![array],
        }
    }
    pub fn intrinsic(func: FloatFn, args: Vec<Expr>) -> Self {
        Expr::Intrinsic { func, args }
    }
    pub fn array(items: Vec<Expr>) -> Self {
        Expr::Array { items }
    }
    pub fn structure<S: Into<String>>(ty: S, fields: Vec<FieldInit>) -> Self {
        Expr::Struct { ty: ty.into(), fields }
    }
    /// A string literal: an array of `char` constants, one per byte.
    pub fn string(s: &str) -> Self {
        Expr::Array {
            items: s
                .bytes()
                .map(|b| Expr::Const { lit: Literal::Char { value: b } })
                .collect(),
        }
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
                Expr::Array { items } => {
                    for item in items {
                        walk(item, f);
                    }
                }
                Expr::Struct { fields, .. } => {
                    for fi in fields {
                        walk(&fi.value, f);
                    }
                }
                Expr::Cast { arg, .. } => walk(arg, f),
                Expr::When { arg, .. } => walk(arg, f),
                Expr::Merge { on_true, on_false, .. } => {
                    walk(on_true, f);
                    walk(on_false, f);
                }
                Expr::Iterate { init, arrays, .. } => {
                    if let Some(i) = init {
                        walk(i, f);
                    }
                    for a in arrays {
                        walk(a, f);
                    }
                }
                Expr::Intrinsic { args, .. } => {
                    for a in args {
                        walk(a, f);
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

    /// Rename every occurrence of variable `from` to `to` — the IR-level
    /// support for renaming a port/local without breaking its readers.
    pub fn rename_var(&mut self, from: &str, to: &str) {
        match self {
            Expr::Var { name } => {
                if name == from {
                    *name = to.to_string();
                }
            }
            Expr::Const { .. } => {}
            Expr::Unary { arg, .. } | Expr::Pre { arg } | Expr::Cast { arg, .. } => {
                arg.rename_var(from, to)
            }
            Expr::Binary { lhs, rhs, .. } => {
                lhs.rename_var(from, to);
                rhs.rename_var(from, to);
            }
            Expr::IfThenElse {
                cond,
                then_branch,
                else_branch,
            } => {
                cond.rename_var(from, to);
                then_branch.rename_var(from, to);
                else_branch.rename_var(from, to);
            }
            Expr::Arrow { init, body } => {
                init.rename_var(from, to);
                body.rename_var(from, to);
            }
            Expr::Call { args, .. } => {
                for a in args {
                    a.rename_var(from, to);
                }
            }
            Expr::Field { base, .. } => base.rename_var(from, to),
            Expr::Index { base, index } => {
                base.rename_var(from, to);
                index.rename_var(from, to);
            }
            Expr::Tuple { items } => {
                for item in items {
                    item.rename_var(from, to);
                }
            }
            Expr::Array { items } => {
                for item in items {
                    item.rename_var(from, to);
                }
            }
            // Field names are part of the record type, not variables.
            Expr::Struct { fields, .. } => {
                for fi in fields {
                    fi.value.rename_var(from, to);
                }
            }
            Expr::When { arg, clock, .. } => {
                if clock == from {
                    *clock = to.to_string();
                }
                arg.rename_var(from, to);
            }
            Expr::Merge { clock, on_true, on_false } => {
                if clock == from {
                    *clock = to.to_string();
                }
                on_true.rename_var(from, to);
                on_false.rename_var(from, to);
            }
            // `node` is a called function name, not a variable — leave it,
            // exactly as `Call` does.
            Expr::Iterate { init, arrays, .. } => {
                if let Some(i) = init {
                    i.rename_var(from, to);
                }
                for a in arrays {
                    a.rename_var(from, to);
                }
            }
            Expr::Intrinsic { args, .. } => {
                for a in args {
                    a.rename_var(from, to);
                }
            }
        }
    }

    /// Collect free variable names referenced by this expression. Clock
    /// conditions of `when`/`merge` are variable reads and count too.
    pub fn free_vars(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.visit(|e| {
            let mut push = |name: &String| {
                if !out.contains(name) {
                    out.push(name.clone());
                }
            };
            match e {
                Expr::Var { name } => push(name),
                Expr::When { clock, .. } | Expr::Merge { clock, .. } => push(clock),
                _ => {}
            }
        });
        out
    }
}
