//! A small recursive-descent parser for the concise textual surface syntax used
//! by the OpenLustre standard-library YAML files.
//!
//! Library blocks are authored as one-liners such as `"x and not (false -> pre
//! x)"` rather than as hand-written IR trees. This module turns those strings
//! into [`ol_ir::Expr`] values and the `type:` fields into [`ol_ir::Type`].
//!
//! The grammar is a conservative subset of Lustre, in precedence order from
//! lowest to highest binding:
//!
//! ```text
//! arrow      ->            (right associative)
//! implies    =>            (right associative)
//! or / xor
//! and
//! compare    = <> < <= > >=
//! add / sub  + -
//! mul / div  * / mod div
//! unary      not  -  pre
//! postfix    .field  [index]
//! primary    literal | ident | ident(args) | (expr) | if/then/else
//! ```

use ol_ir::{BinOp, Expr, Literal, Type};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("unexpected character `{0}` at byte {1}")]
    BadChar(char, usize),
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("expected `{expected}` but found `{found}`")]
    Expected { expected: String, found: String },
    #[error("trailing tokens after expression: `{0}`")]
    Trailing(String),
    #[error("unknown type `{0}`")]
    UnknownType(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Arrow,    // ->
    FatArrow, // =>
    Le,       // <=
    Ge,       // >=
    Ne,       // <>
    Lt,       // <
    Gt,       // >
    Eq,       // =
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
}

impl Tok {
    fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => s.clone(),
            Tok::Int(n) => n.to_string(),
            Tok::Float(f) => f.to_string(),
            Tok::Arrow => "->".into(),
            Tok::FatArrow => "=>".into(),
            Tok::Le => "<=".into(),
            Tok::Ge => ">=".into(),
            Tok::Ne => "<>".into(),
            Tok::Lt => "<".into(),
            Tok::Gt => ">".into(),
            Tok::Eq => "=".into(),
            Tok::Plus => "+".into(),
            Tok::Minus => "-".into(),
            Tok::Star => "*".into(),
            Tok::Slash => "/".into(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBracket => "[".into(),
            Tok::RBracket => "]".into(),
            Tok::Comma => ",".into(),
            Tok::Dot => ".".into(),
        }
    }
}

fn tokenize(src: &str) -> Result<Vec<Tok>, ParseError> {
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => { out.push(Tok::LParen); i += 1; }
            ')' => { out.push(Tok::RParen); i += 1; }
            '[' => { out.push(Tok::LBracket); i += 1; }
            ']' => { out.push(Tok::RBracket); i += 1; }
            ',' => { out.push(Tok::Comma); i += 1; }
            '.' => { out.push(Tok::Dot); i += 1; }
            '+' => { out.push(Tok::Plus); i += 1; }
            '*' => { out.push(Tok::Star); i += 1; }
            '/' => { out.push(Tok::Slash); i += 1; }
            '-' => {
                if bytes.get(i + 1) == Some(&b'>') {
                    out.push(Tok::Arrow);
                    i += 2;
                } else {
                    out.push(Tok::Minus);
                    i += 1;
                }
            }
            '=' => {
                if bytes.get(i + 1) == Some(&b'>') {
                    out.push(Tok::FatArrow);
                    i += 2;
                } else {
                    out.push(Tok::Eq);
                    i += 1;
                }
            }
            '<' => match bytes.get(i + 1) {
                Some(&b'=') => { out.push(Tok::Le); i += 2; }
                Some(&b'>') => { out.push(Tok::Ne); i += 2; }
                _ => { out.push(Tok::Lt); i += 1; }
            },
            '>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            _ if c.is_ascii_digit() => {
                let start = i;
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                let mut is_float = false;
                if i < bytes.len() && bytes[i] == b'.' {
                    is_float = true;
                    i += 1;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                        i += 1;
                    }
                }
                let text = &src[start..i];
                if is_float {
                    out.push(Tok::Float(text.parse().map_err(|_| ParseError::BadChar(c, start))?));
                } else {
                    out.push(Tok::Int(text.parse().map_err(|_| ParseError::BadChar(c, start))?));
                }
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < bytes.len()
                    && ((bytes[i] as char).is_alphanumeric() || bytes[i] == b'_')
                {
                    i += 1;
                }
                out.push(Tok::Ident(src[start..i].to_string()));
            }
            _ => return Err(ParseError::BadChar(c, i)),
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn eat_kw(&mut self, kw: &str) -> bool {
        if matches!(self.peek(), Some(Tok::Ident(s)) if s == kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s == kw)
    }
    fn expect(&mut self, t: &Tok) -> Result<(), ParseError> {
        match self.peek() {
            Some(found) if found == t => {
                self.pos += 1;
                Ok(())
            }
            Some(found) => Err(ParseError::Expected {
                expected: t.describe(),
                found: found.describe(),
            }),
            None => Err(ParseError::UnexpectedEof),
        }
    }
    fn expect_kw(&mut self, kw: &str) -> Result<(), ParseError> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(ParseError::Expected {
                expected: kw.to_string(),
                found: self
                    .peek()
                    .map(Tok::describe)
                    .unwrap_or_else(|| "<eof>".into()),
            })
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_arrow()
    }

    fn parse_arrow(&mut self) -> Result<Expr, ParseError> {
        let init = self.parse_implies()?;
        if matches!(self.peek(), Some(Tok::Arrow)) {
            self.bump();
            let body = self.parse_arrow()?;
            Ok(Expr::arrow(init, body))
        } else {
            Ok(init)
        }
    }

    fn parse_implies(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_or()?;
        if matches!(self.peek(), Some(Tok::FatArrow)) {
            self.bump();
            let rhs = self.parse_implies()?;
            Ok(Expr::implies(lhs, rhs))
        } else {
            Ok(lhs)
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        loop {
            let op = if self.is_kw("or") {
                BinOp::Or
            } else if self.is_kw("xor") {
                BinOp::Xor
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_cmp()?;
        while self.is_kw("and") {
            self.bump();
            let rhs = self.parse_cmp()?;
            lhs = Expr::bin(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_add()?;
        let op = match self.peek() {
            Some(Tok::Eq) => BinOp::Eq,
            Some(Tok::Ne) => BinOp::Neq,
            Some(Tok::Lt) => BinOp::Lt,
            Some(Tok::Le) => BinOp::Le,
            Some(Tok::Gt) => BinOp::Gt,
            Some(Tok::Ge) => BinOp::Ge,
            _ => return Ok(lhs),
        };
        self.bump();
        let rhs = self.parse_add()?;
        Ok(Expr::bin(op, lhs, rhs))
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Plus) => BinOp::Add,
                Some(Tok::Minus) => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_mul()?;
            lhs = Expr::bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = if matches!(self.peek(), Some(Tok::Star)) {
                BinOp::Mul
            } else if matches!(self.peek(), Some(Tok::Slash)) {
                BinOp::Div
            } else if self.is_kw("div") {
                BinOp::Div
            } else if self.is_kw("mod") {
                BinOp::Mod
            } else {
                break;
            };
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::bin(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat_kw("not") {
            return Ok(Expr::not(self.parse_unary()?));
        }
        if self.eat_kw("pre") {
            return Ok(Expr::pre(self.parse_unary()?));
        }
        if matches!(self.peek(), Some(Tok::Minus)) {
            self.bump();
            return Ok(Expr::neg(self.parse_unary()?));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.bump();
                    match self.bump() {
                        Some(Tok::Ident(field)) => {
                            e = Expr::Field {
                                base: Box::new(e),
                                field,
                            };
                        }
                        other => {
                            return Err(ParseError::Expected {
                                expected: "field name".into(),
                                found: other.map(|t| t.describe()).unwrap_or_else(|| "<eof>".into()),
                            })
                        }
                    }
                }
                Some(Tok::LBracket) => {
                    self.bump();
                    let index = self.parse_expr()?;
                    self.expect(&Tok::RBracket)?;
                    e = Expr::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                    };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().cloned() {
            Some(Tok::LParen) => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Some(Tok::Int(n)) => {
                self.bump();
                Ok(Expr::Const { lit: Literal::int(n) })
            }
            Some(Tok::Float(f)) => {
                self.bump();
                Ok(Expr::Const { lit: Literal::float(f) })
            }
            Some(Tok::Ident(name)) => {
                if name == "true" {
                    self.bump();
                    return Ok(Expr::bool_lit(true));
                }
                if name == "false" {
                    self.bump();
                    return Ok(Expr::bool_lit(false));
                }
                if name == "if" {
                    self.bump();
                    let cond = self.parse_expr()?;
                    self.expect_kw("then")?;
                    let then_branch = self.parse_expr()?;
                    self.expect_kw("else")?;
                    let else_branch = self.parse_expr()?;
                    return Ok(Expr::if_then_else(cond, then_branch, else_branch));
                }
                self.bump();
                // Function/operator call?
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.bump();
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Tok::RParen)) {
                        loop {
                            args.push(self.parse_expr()?);
                            if matches!(self.peek(), Some(Tok::Comma)) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::call(name, args))
                } else {
                    Ok(Expr::var(name))
                }
            }
            Some(other) => Err(ParseError::Expected {
                expected: "expression".into(),
                found: other.describe(),
            }),
            None => Err(ParseError::UnexpectedEof),
        }
    }
}

/// Parse a textual OpenLustre expression into IR.
pub fn parse_expr(src: &str) -> Result<Expr, ParseError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_expr()?;
    if p.pos != p.toks.len() {
        let rest: Vec<String> = p.toks[p.pos..].iter().map(Tok::describe).collect();
        return Err(ParseError::Trailing(rest.join(" ")));
    }
    Ok(e)
}

/// Parse a textual type annotation (`bool`, `int32`, `uint8[32]`, `MyRecord`, …).
pub fn parse_type(src: &str) -> Result<Type, ParseError> {
    let src = src.trim();
    // Array form: `elem[len]`.
    if let Some(open) = src.find('[') {
        if !src.ends_with(']') {
            return Err(ParseError::UnknownType(src.to_string()));
        }
        let elem = &src[..open];
        let len_str = &src[open + 1..src.len() - 1];
        let len: u32 = len_str
            .trim()
            .parse()
            .map_err(|_| ParseError::UnknownType(src.to_string()))?;
        return Ok(Type::Array {
            elem: Box::new(parse_type(elem)?),
            len,
        });
    }
    Ok(match src {
        "bool" => Type::Bool,
        "int8" => Type::Int8,
        "int16" => Type::Int16,
        "int" | "int32" => Type::Int32,
        "int64" => Type::Int64,
        "uint8" => Type::Uint8,
        "uint16" => Type::Uint16,
        "uint32" => Type::Uint32,
        "uint64" => Type::Uint64,
        "float32" => Type::Float32,
        "real" | "float64" => Type::Float64,
        "" => return Err(ParseError::UnknownType(src.to_string())),
        // Anything else is treated as a reference to a named record/enum type;
        // a leading lowercase letter is a strong hint of a typo, but the type
        // checker resolves names, so we defer that judgement to it.
        other => Type::Named(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Expr {
        parse_expr(s).unwrap_or_else(|e| panic!("parse `{s}` failed: {e}"))
    }

    #[test]
    fn logical_and_precedence() {
        // `a and b or c` => (a and b) or c
        assert_eq!(
            p("a and b or c"),
            Expr::or(Expr::and(Expr::var("a"), Expr::var("b")), Expr::var("c"))
        );
    }

    #[test]
    fn not_binds_tighter_than_and() {
        assert_eq!(
            p("not a and b"),
            Expr::and(Expr::not(Expr::var("a")), Expr::var("b"))
        );
    }

    #[test]
    fn arrow_and_pre_edge_pattern() {
        // x and not (false -> pre x)
        let expected = Expr::and(
            Expr::var("x"),
            Expr::not(Expr::arrow(Expr::bool_lit(false), Expr::pre(Expr::var("x")))),
        );
        assert_eq!(p("x and not (false -> pre x)"), expected);
    }

    #[test]
    fn pre_x_is_an_identifier_not_an_operator() {
        assert_eq!(p("pre_x"), Expr::var("pre_x"));
        assert_eq!(p("pre x"), Expr::pre(Expr::var("x")));
    }

    #[test]
    fn nested_if_then_else() {
        let e = p("if set then true else if reset then false else (false -> pre q)");
        match e {
            Expr::IfThenElse { else_branch, .. } => {
                assert!(matches!(*else_branch, Expr::IfThenElse { .. }));
            }
            _ => panic!("expected if/then/else"),
        }
    }

    #[test]
    fn arithmetic_precedence() {
        // a + b * c => a + (b * c)
        assert_eq!(
            p("a + b * c"),
            Expr::bin(
                BinOp::Add,
                Expr::var("a"),
                Expr::bin(BinOp::Mul, Expr::var("b"), Expr::var("c"))
            )
        );
    }

    #[test]
    fn comparison_and_equality() {
        assert_eq!(
            p("a <= b"),
            Expr::bin(BinOp::Le, Expr::var("a"), Expr::var("b"))
        );
        assert_eq!(
            p("q = pre q"),
            Expr::bin(BinOp::Eq, Expr::var("q"), Expr::pre(Expr::var("q")))
        );
    }

    #[test]
    fn implies_is_right_associative() {
        // a => b => c parses as a => (b => c)
        assert_eq!(
            p("a => b => c"),
            Expr::implies(Expr::var("a"), Expr::implies(Expr::var("b"), Expr::var("c")))
        );
    }

    #[test]
    fn function_call() {
        assert_eq!(
            p("Max(a, b)"),
            Expr::call("Max", vec![Expr::var("a"), Expr::var("b")])
        );
        assert_eq!(p("f()"), Expr::call("f", vec![]));
    }

    #[test]
    fn unary_minus() {
        assert_eq!(
            p("x < -limit"),
            Expr::bin(BinOp::Lt, Expr::var("x"), Expr::neg(Expr::var("limit")))
        );
    }

    #[test]
    fn trailing_tokens_error() {
        assert!(matches!(parse_expr("a b"), Err(ParseError::Trailing(_))));
    }

    #[test]
    fn types() {
        assert_eq!(parse_type("bool").unwrap(), Type::Bool);
        assert_eq!(parse_type("int32").unwrap(), Type::Int32);
        assert_eq!(
            parse_type("uint8[32]").unwrap(),
            Type::Array { elem: Box::new(Type::Uint8), len: 32 }
        );
        assert_eq!(parse_type("MyRecord").unwrap(), Type::Named("MyRecord".into()));
    }
}
