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

use ol_ir::{BinOp, Expr, FieldInit, Literal, Type};

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
    Amp,      // &
    Pipe,     // |
    Caret,    // ^
    Shl,      // <<
    Shr,      // >>
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,   // {
    RBrace,   // }
    Comma,
    Semi,     // ;
    Colon,    // :
    Dot,
    CharLit(u8), // 'a'
    Str(String), // "ab"
    TypedInt(i64, Type),   // 8_i32
    TypedFloat(f64, Type), // 2.5_f32
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
            Tok::Amp => "&".into(),
            Tok::Pipe => "|".into(),
            Tok::Caret => "^".into(),
            Tok::Shl => "<<".into(),
            Tok::Shr => ">>".into(),
            Tok::LParen => "(".into(),
            Tok::RParen => ")".into(),
            Tok::LBracket => "[".into(),
            Tok::RBracket => "]".into(),
            Tok::LBrace => "{".into(),
            Tok::RBrace => "}".into(),
            Tok::Comma => ",".into(),
            Tok::Semi => ";".into(),
            Tok::Colon => ":".into(),
            Tok::Dot => ".".into(),
            Tok::CharLit(b) => format!("'{}'", *b as char),
            Tok::Str(s) => format!("\"{s}\""),
            Tok::TypedInt(n, ty) => format!("{n}_{}", ty.lustre_name()),
            Tok::TypedFloat(f, ty) => format!("{f}_{}", ty.lustre_name()),
        }
    }
}

/// A numeric type suffix on a literal — `8_i32`, `2.5_f32`. Returns `None` for
/// anything that isn't a recognized suffix, so a stray `_x` is left to tokenize
/// as its own identifier.
fn numeric_suffix_type(s: &str) -> Option<Type> {
    Some(match s {
        "i8" => Type::Int8,
        "i16" => Type::Int16,
        "i32" => Type::Int32,
        "i64" => Type::Int64,
        "u8" => Type::Uint8,
        "u16" => Type::Uint16,
        "u32" => Type::Uint32,
        "u64" => Type::Uint64,
        "f32" => Type::Float32,
        "f64" => Type::Float64,
        _ => return None,
    })
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
            '{' => { out.push(Tok::LBrace); i += 1; }
            '}' => { out.push(Tok::RBrace); i += 1; }
            ';' => { out.push(Tok::Semi); i += 1; }
            ':' => { out.push(Tok::Colon); i += 1; }
            ',' => { out.push(Tok::Comma); i += 1; }
            '.' => { out.push(Tok::Dot); i += 1; }
            // Character literal `'a'` (with the usual escapes).
            '\'' => {
                i += 1;
                let val = read_escaped(bytes, &mut i, b'\'')?;
                if bytes.get(i) != Some(&b'\'') {
                    return Err(ParseError::Expected {
                        expected: "closing ' for a character literal".into(),
                        found: bytes.get(i).map(|b| (*b as char).to_string()).unwrap_or_else(|| "<eof>".into()),
                    });
                }
                i += 1;
                out.push(Tok::CharLit(val));
            }
            // String literal `"abc"` — lowers to an array of char.
            '"' => {
                i += 1;
                let mut s: Vec<u8> = Vec::new();
                loop {
                    match bytes.get(i) {
                        None => return Err(ParseError::UnexpectedEof),
                        Some(&b'"') => { i += 1; break; }
                        Some(_) => s.push(read_escaped(bytes, &mut i, b'"')?),
                    }
                }
                out.push(Tok::Str(String::from_utf8_lossy(&s).into_owned()));
            }
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
                Some(&b'<') => { out.push(Tok::Shl); i += 2; }
                _ => { out.push(Tok::Lt); i += 1; }
            },
            '>' => match bytes.get(i + 1) {
                Some(&b'=') => { out.push(Tok::Ge); i += 2; }
                Some(&b'>') => { out.push(Tok::Shr); i += 2; }
                _ => { out.push(Tok::Gt); i += 1; }
            },
            '&' => { out.push(Tok::Amp); i += 1; }
            '|' => { out.push(Tok::Pipe); i += 1; }
            '^' => { out.push(Tok::Caret); i += 1; }
            _ if c.is_ascii_digit() => {
                let start = i;
                // 0x... hex literal.
                if c == '0'
                    && matches!(bytes.get(i + 1), Some(b'x') | Some(b'X'))
                {
                    i += 2;
                    let hex_start = i;
                    while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                        i += 1;
                    }
                    let n = i64::from_str_radix(&src[hex_start..i], 16)
                        .map_err(|_| ParseError::BadChar(c, start))?;
                    out.push(Tok::Int(n));
                    continue;
                }
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
                // Optional numeric type suffix: `8_i32`, `2.5_f32`. Only a
                // recognized suffix is consumed; otherwise `_…` tokenizes on its
                // own (and the bare number stands).
                let mut suffix_ty = None;
                if bytes.get(i) == Some(&b'_') {
                    let s = i + 1;
                    let mut j = s;
                    while j < bytes.len() && (bytes[j] as char).is_alphanumeric() {
                        j += 1;
                    }
                    if let Some(ty) = numeric_suffix_type(&src[s..j]) {
                        suffix_ty = Some(ty);
                        i = j;
                    }
                }
                match (is_float, suffix_ty) {
                    (true, Some(ty)) => out.push(Tok::TypedFloat(
                        text.parse().map_err(|_| ParseError::BadChar(c, start))?,
                        ty,
                    )),
                    (false, Some(ty)) => out.push(Tok::TypedInt(
                        text.parse().map_err(|_| ParseError::BadChar(c, start))?,
                        ty,
                    )),
                    (true, None) => out.push(Tok::Float(
                        text.parse().map_err(|_| ParseError::BadChar(c, start))?,
                    )),
                    (false, None) => out.push(Tok::Int(
                        text.parse().map_err(|_| ParseError::BadChar(c, start))?,
                    )),
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

/// Read one (possibly backslash-escaped) byte from inside a `'…'` / `"…"`
/// literal, advancing `i` past it. The opening quote has already been consumed
/// and the closing quote is detected by the caller, so this only ever sees
/// payload bytes or an escape sequence.
fn read_escaped(bytes: &[u8], i: &mut usize, _delim: u8) -> Result<u8, ParseError> {
    let b = *bytes.get(*i).ok_or(ParseError::UnexpectedEof)?;
    if b == b'\\' {
        *i += 1;
        let e = *bytes.get(*i).ok_or(ParseError::UnexpectedEof)?;
        *i += 1;
        Ok(match e {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'0' => 0,
            // `\\`, `\'`, `\"`, and any other escaped byte are taken literally.
            other => other,
        })
    } else {
        *i += 1;
        Ok(b)
    }
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
        let init = self.parse_when()?;
        if matches!(self.peek(), Some(Tok::Arrow)) {
            self.bump();
            let body = self.parse_arrow()?;
            Ok(Expr::arrow(init, body))
        } else {
            Ok(init)
        }
    }

    /// `e when c` / `e when not c` — clock sampling. Left-associative so
    /// `x when c when d` nests the sampling. The condition must be a plain
    /// variable name (the classic Lustre restriction).
    fn parse_when(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_implies()?;
        while self.eat_kw("when") {
            let on = !self.eat_kw("not");
            match self.bump() {
                Some(Tok::Ident(clock)) => {
                    e = Expr::when(e, clock, on);
                }
                other => {
                    return Err(ParseError::Expected {
                        expected: "clock variable name after `when`".into(),
                        found: other.map(|t| t.describe()).unwrap_or_else(|| "<eof>".into()),
                    })
                }
            }
        }
        Ok(e)
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
        let mut lhs = self.parse_bit_or()?;
        while self.is_kw("and") {
            self.bump();
            let rhs = self.parse_bit_or()?;
            lhs = Expr::bin(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bit_xor()?;
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.bump();
            let rhs = self.parse_bit_xor()?;
            lhs = Expr::bin(BinOp::BitOr, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bit_and()?;
        while matches!(self.peek(), Some(Tok::Caret)) {
            self.bump();
            let rhs = self.parse_bit_and()?;
            lhs = Expr::bin(BinOp::BitXor, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_bit_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_cmp()?;
        while matches!(self.peek(), Some(Tok::Amp)) {
            self.bump();
            let rhs = self.parse_cmp()?;
            lhs = Expr::bin(BinOp::BitAnd, lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let lhs = self.parse_shift()?;
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
        let rhs = self.parse_shift()?;
        Ok(Expr::bin(op, lhs, rhs))
    }

    fn parse_shift(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_add()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Shl) => BinOp::Shl,
                Some(Tok::Shr) => BinOp::Shr,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_add()?;
            lhs = Expr::bin(op, lhs, rhs);
        }
        Ok(lhs)
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
            // A typed literal `8_i32` / `2.5_f32` is the value cast to that type.
            Some(Tok::TypedInt(n, ty)) => {
                self.bump();
                Ok(Expr::Cast { to: ty, arg: Box::new(Expr::Const { lit: Literal::int(n) }) })
            }
            Some(Tok::TypedFloat(f, ty)) => {
                self.bump();
                Ok(Expr::Cast { to: ty, arg: Box::new(Expr::Const { lit: Literal::float(f) }) })
            }
            Some(Tok::CharLit(b)) => {
                self.bump();
                Ok(Expr::Const { lit: Literal::char(b) })
            }
            // A string literal is sugar for an array of `char` constants.
            Some(Tok::Str(s)) => {
                self.bump();
                Ok(Expr::string(&s))
            }
            // Array literal `[e0; e1; …]` (semicolon-separated, Lustre style).
            // A leading `[` can only start an array; an index `[…]` is parsed in
            // `parse_postfix` after a primary.
            Some(Tok::LBracket) => {
                self.bump();
                let mut items = Vec::new();
                if !matches!(self.peek(), Some(Tok::RBracket)) {
                    loop {
                        items.push(self.parse_expr()?);
                        if matches!(self.peek(), Some(Tok::Semi)) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(&Tok::RBracket)?;
                Ok(Expr::array(items))
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
                    // `map(F, a, …)` / `fold(F, init, a)` are array iterators:
                    // the first argument is the iterated function's name.
                    if name == "map" || name == "fold" {
                        let iter_node = match args.first() {
                            Some(Expr::Var { name }) => name.clone(),
                            _ => {
                                return Err(ParseError::Expected {
                                    expected: format!(
                                        "a function name as {name}'s first argument"
                                    ),
                                    found: args
                                        .first()
                                        .map(ol_ir_expr_describe)
                                        .unwrap_or_else(|| "no arguments".into()),
                                })
                            }
                        };
                        let rest: Vec<Expr> = args.into_iter().skip(1).collect();
                        if name == "map" {
                            if rest.is_empty() {
                                return Err(ParseError::Expected {
                                    expected: "map(F, array, …) with at least one array".into(),
                                    found: "no arrays".into(),
                                });
                            }
                            return Ok(Expr::map(iter_node, rest));
                        }
                        // fold(F, init, array)
                        if rest.len() != 2 {
                            return Err(ParseError::Expected {
                                expected: "fold(F, init, array)".into(),
                                found: format!("{} arguments after F", rest.len()),
                            });
                        }
                        let mut it = rest.into_iter();
                        let init = it.next().unwrap();
                        let array = it.next().unwrap();
                        return Ok(Expr::fold(iter_node, init, array));
                    }
                    // `merge(c, a, b)` joins two complementary clocked
                    // streams; the clock must be a variable name.
                    if name == "merge" {
                        if args.len() != 3 {
                            return Err(ParseError::Expected {
                                expected: "merge(clock, on_true, on_false)".into(),
                                found: format!("{} arguments", args.len()),
                            });
                        }
                        let on_false = args.pop().unwrap();
                        let on_true = args.pop().unwrap();
                        let clock = match args.pop().unwrap() {
                            Expr::Var { name } => name,
                            other => {
                                return Err(ParseError::Expected {
                                    expected: "a clock variable name as merge's first argument"
                                        .into(),
                                    found: ol_ir_expr_describe(&other),
                                })
                            }
                        };
                        return Ok(Expr::merge(clock, on_true, on_false));
                    }
                    // A "call" to a float-intrinsic name is SCADE's math
                    // built-in: `sqrt(x)`, `sin(x)`, `min(a, b)`.
                    if let Some(func) = ol_ir::FloatFn::from_name(&name) {
                        if args.len() != func.arity() {
                            return Err(ParseError::Expected {
                                expected: format!(
                                    "exactly {} argument(s) for `{name}(...)`",
                                    func.arity()
                                ),
                                found: format!("{} arguments", args.len()),
                            });
                        }
                        return Ok(Expr::Intrinsic { func, args });
                    }
                    // A "call" to a numeric type name is SCADE's numeric_cast:
                    // `int16(x)`, `float64(x)`.
                    if let Some(ty) = numeric_type_name(&name) {
                        if args.len() != 1 {
                            return Err(ParseError::Expected {
                                expected: format!("exactly one argument for cast `{name}(...)`"),
                                found: format!("{} arguments", args.len()),
                            });
                        }
                        return Ok(Expr::Cast {
                            to: ty,
                            arg: Box::new(args.pop().unwrap()),
                        });
                    }
                    Ok(Expr::call(name, args))
                } else if matches!(self.peek(), Some(Tok::LBrace)) {
                    // Record literal `Name { field: value, … }`.
                    self.bump();
                    let mut fields = Vec::new();
                    if !matches!(self.peek(), Some(Tok::RBrace)) {
                        loop {
                            let field = match self.bump() {
                                Some(Tok::Ident(f)) => f,
                                other => {
                                    return Err(ParseError::Expected {
                                        expected: "field name in record literal".into(),
                                        found: other
                                            .map(|t| t.describe())
                                            .unwrap_or_else(|| "<eof>".into()),
                                    })
                                }
                            };
                            self.expect(&Tok::Colon)?;
                            let value = self.parse_expr()?;
                            fields.push(FieldInit { field, value });
                            if matches!(self.peek(), Some(Tok::Comma)) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(&Tok::RBrace)?;
                    Ok(Expr::structure(name, fields))
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

/// A short description of an expression for parse-error messages.
fn ol_ir_expr_describe(e: &Expr) -> String {
    match e {
        Expr::Const { .. } => "a literal".into(),
        Expr::Call { node, .. } => format!("a call to `{node}`"),
        _ => "a compound expression".into(),
    }
}

/// Numeric type names usable as casts (`int16(x)`). Booleans and named
/// types are deliberately excluded — numeric_cast converts representations,
/// not meanings.
fn numeric_type_name(name: &str) -> Option<Type> {
    Some(match name {
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
        // `sfix<bits>_<frac>` / `ufix<bits>_<frac>` are valid cast targets too.
        _ => return fixed_type_name(name),
    })
}

/// Parse a fixed-point type name `sfix<bits>_<frac>` (signed) or
/// `ufix<bits>_<frac>` (unsigned), e.g. `sfix32_16`. `bits` must be a storable
/// width (8/16/32/64) and `frac` must leave at least one integer/sign bit
/// (`frac < bits`); anything else returns `None` so a malformed name never
/// slips through as a valid type.
fn fixed_type_name(name: &str) -> Option<Type> {
    let (signed, rest) = match name.strip_prefix("sfix") {
        Some(r) => (true, r),
        None => (false, name.strip_prefix("ufix")?),
    };
    let (bits, frac) = rest.split_once('_')?;
    let bits: u32 = bits.parse().ok()?;
    let frac: u32 = frac.parse().ok()?;
    if matches!(bits, 8 | 16 | 32 | 64) && frac < bits {
        Some(Type::Fixed { signed, bits, frac })
    } else {
        None
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
    // Fixed-point: `sfix<bits>_<frac>` / `ufix<bits>_<frac>`. Caught before the
    // named-type fallback so a malformed spelling is a clear error, not a
    // dangling type reference.
    if src.starts_with("sfix") || src.starts_with("ufix") {
        return fixed_type_name(src).ok_or_else(|| ParseError::UnknownType(src.to_string()));
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
        "char" => Type::Char,
        "" => return Err(ParseError::UnknownType(src.to_string())),
        // Anything else is treated as a reference to a named record/enum type;
        // a leading lowercase letter is a strong hint of a typo, but the type
        // checker resolves names, so we defer that judgement to it.
        other => Type::named(other),
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
        assert_eq!(parse_type("MyRecord").unwrap(), Type::named("MyRecord"));
        assert_eq!(parse_type("char").unwrap(), Type::Char);
        assert_eq!(
            parse_type("char[4]").unwrap(),
            Type::Array { elem: Box::new(Type::Char), len: 4 }
        );
    }

    #[test]
    fn char_and_string_literals() {
        assert_eq!(p("'a'"), Expr::Const { lit: Literal::char(b'a') });
        assert_eq!(p("'\\n'"), Expr::Const { lit: Literal::char(b'\n') });
        // A string is an array of char constants.
        assert_eq!(p("\"hi\""), Expr::string("hi"));
        if let Expr::Array { items } = p("\"hi\"") {
            assert_eq!(items.len(), 2);
        } else {
            panic!("string should parse to an array");
        }
    }

    #[test]
    fn array_literal() {
        assert_eq!(
            p("[1; 2; 3]"),
            Expr::array(vec![
                Expr::Const { lit: Literal::int(1) },
                Expr::Const { lit: Literal::int(2) },
                Expr::Const { lit: Literal::int(3) },
            ])
        );
        assert_eq!(p("[]"), Expr::array(vec![]));
    }

    #[test]
    fn array_index_still_parses_after_a_primary() {
        // A `[` following a primary is indexing, not an array literal.
        assert_eq!(
            p("xs[2]"),
            Expr::Index {
                base: Box::new(Expr::var("xs")),
                index: Box::new(Expr::Const { lit: Literal::int(2) }),
            }
        );
    }

    #[test]
    fn record_literal() {
        assert_eq!(
            p("Point { x: 1, y: 2 }"),
            Expr::structure(
                "Point",
                vec![
                    FieldInit { field: "x".into(), value: Expr::Const { lit: Literal::int(1) } },
                    FieldInit { field: "y".into(), value: Expr::Const { lit: Literal::int(2) } },
                ]
            )
        );
    }

    #[test]
    fn typed_literal_suffix() {
        assert_eq!(
            p("8_i32"),
            Expr::Cast { to: Type::Int32, arg: Box::new(Expr::Const { lit: Literal::int(8) }) }
        );
        assert_eq!(
            p("2.5_f32"),
            Expr::Cast { to: Type::Float32, arg: Box::new(Expr::Const { lit: Literal::float(2.5) }) }
        );
        // A typed literal composes in a larger expression: `x > 8_i32`.
        assert_eq!(
            p("x > 8_i32"),
            Expr::bin(
                BinOp::Gt,
                Expr::var("x"),
                Expr::Cast { to: Type::Int32, arg: Box::new(Expr::Const { lit: Literal::int(8) }) }
            )
        );
        // An unrecognized suffix is not consumed (so `8_foo` is a parse error).
        assert!(parse_expr("8_foo").is_err());
    }
}
