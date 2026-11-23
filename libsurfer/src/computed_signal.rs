//! Computed/virtual signals: evaluate user expressions over existing waveform variables.

use std::collections::HashMap;

use num::{BigInt, BigUint, ToPrimitive, Zero};
use surfer_translation_types::VariableValue;

use crate::displayed_item::ComputedSignalSource;
use crate::wave_container::{QueryResult, VariableRef, WaveContainer};

#[derive(Debug, Clone)]
pub enum Expr {
    Name(String),
    Const(BigUint),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Not,
    BitNot,
    Neg,
}

#[derive(Debug, Clone, Copy)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    And, Or, Xor,
    Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge,
    LogicAnd, LogicOr,
}

pub fn parse_expression(input: &str) -> Result<Expr, String> {
    let tokens = tokenize(input)?;
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_ternary()?;
    if p.pos < p.tokens.len() {
        return Err(format!("Unexpected token {:?}", p.tokens[p.pos]));
    }
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Number(BigUint),
    LParen, RParen,
    Comma, Question, Colon,
    Plus, Minus, Star, Slash, Percent,
    Amp, Pipe, Caret, Tilde, Bang,
    Shl, Shr,
    Eq, Ne, Lt, Le, Gt, Ge,
    LogicAnd, LogicOr,
}

fn tokenize(input: &str) -> Result<Vec<Tok>, String> {
    let mut chars = input.chars().peekable();
    let mut out = Vec::new();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => { chars.next(); }
            '(' => { chars.next(); out.push(Tok::LParen); }
            ')' => { chars.next(); out.push(Tok::RParen); }
            ',' => { chars.next(); out.push(Tok::Comma); }
            '?' => { chars.next(); out.push(Tok::Question); }
            ':' => { chars.next(); out.push(Tok::Colon); }
            '+' => { chars.next(); out.push(Tok::Plus); }
            '-' => { chars.next(); out.push(Tok::Minus); }
            '*' => { chars.next(); out.push(Tok::Star); }
            '/' => { chars.next(); out.push(Tok::Slash); }
            '%' => { chars.next(); out.push(Tok::Percent); }
            '^' => { chars.next(); out.push(Tok::Caret); }
            '~' => { chars.next(); out.push(Tok::Tilde); }
            '&' => {
                chars.next();
                if chars.peek() == Some(&'&') { chars.next(); out.push(Tok::LogicAnd); }
                else { out.push(Tok::Amp); }
            }
            '|' => {
                chars.next();
                if chars.peek() == Some(&'|') { chars.next(); out.push(Tok::LogicOr); }
                else { out.push(Tok::Pipe); }
            }
            '=' => {
                chars.next();
                if chars.peek() == Some(&'=') { chars.next(); out.push(Tok::Eq); }
                else { return Err("Expected '=='".into()); }
            }
            '!' => {
                chars.next();
                if chars.peek() == Some(&'=') { chars.next(); out.push(Tok::Ne); }
                else { out.push(Tok::Bang); }
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'<') { chars.next(); out.push(Tok::Shl); }
                else if chars.peek() == Some(&'=') { chars.next(); out.push(Tok::Le); }
                else { out.push(Tok::Lt); }
            }
            '>' => {
                chars.next();
                if chars.peek() == Some(&'>') { chars.next(); out.push(Tok::Shr); }
                else if chars.peek() == Some(&'=') { chars.next(); out.push(Tok::Ge); }
                else { out.push(Tok::Gt); }
            }
            '0'..='9' => {
                let mut s = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_alphanumeric() || d == '_' { s.push(d); chars.next(); } else { break; }
                }
                let n = parse_number(&s)?;
                out.push(Tok::Number(n));
            }
            'a'..='z' | 'A'..='Z' | '_' | '$' | '.' | '[' | ']' => {
                let mut s = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_alphanumeric() || matches!(d, '_' | '$' | '.' | '[' | ']' | ':') {
                        s.push(d); chars.next();
                    } else { break; }
                }
                out.push(Tok::Ident(s));
            }
            _ => return Err(format!("Unexpected char '{c}'")),
        }
    }
    Ok(out)
}

fn parse_number(s: &str) -> Result<BigUint, String> {
    let clean: String = s.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = clean.strip_prefix("0x").or_else(|| clean.strip_prefix("0X")) {
        BigUint::parse_bytes(hex.as_bytes(), 16).ok_or_else(|| format!("Bad hex {s}"))
    } else if let Some(bin) = clean.strip_prefix("0b").or_else(|| clean.strip_prefix("0B")) {
        BigUint::parse_bytes(bin.as_bytes(), 2).ok_or_else(|| format!("Bad bin {s}"))
    } else {
        BigUint::parse_bytes(clean.as_bytes(), 10).ok_or_else(|| format!("Bad number {s}"))
    }
}

struct Parser { tokens: Vec<Tok>, pos: usize }
impl Parser {
    fn peek(&self) -> Option<&Tok> { self.tokens.get(self.pos) }
    fn eat(&mut self) -> Option<Tok> { let t = self.tokens.get(self.pos).cloned()?; self.pos += 1; Some(t) }
    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let mut cond = self.parse_logic_or()?;
        if matches!(self.peek(), Some(Tok::Question)) {
            self.eat();
            let then_e = self.parse_ternary()?;
            match self.eat() { Some(Tok::Colon) => {}, _ => return Err("Expected ':'".into()) }
            let else_e = self.parse_ternary()?;
            cond = Expr::Ternary(Box::new(cond), Box::new(then_e), Box::new(else_e));
        }
        Ok(cond)
    }
    fn parse_logic_or(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_logic_and()?;
        while matches!(self.peek(), Some(Tok::LogicOr)) { self.eat(); let r = self.parse_logic_and()?; e = Expr::Binary(BinaryOp::LogicOr, Box::new(e), Box::new(r)); }
        Ok(e)
    }
    fn parse_logic_and(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_bitor()?;
        while matches!(self.peek(), Some(Tok::LogicAnd)) { self.eat(); let r = self.parse_bitor()?; e = Expr::Binary(BinaryOp::LogicAnd, Box::new(e), Box::new(r)); }
        Ok(e)
    }
    fn parse_bitor(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_xor()?;
        while matches!(self.peek(), Some(Tok::Pipe)) { self.eat(); let r = self.parse_xor()?; e = Expr::Binary(BinaryOp::Or, Box::new(e), Box::new(r)); }
        Ok(e)
    }
    fn parse_xor(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_bitand()?;
        while matches!(self.peek(), Some(Tok::Caret)) { self.eat(); let r = self.parse_bitand()?; e = Expr::Binary(BinaryOp::Xor, Box::new(e), Box::new(r)); }
        Ok(e)
    }
    fn parse_bitand(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_eq()?;
        while matches!(self.peek(), Some(Tok::Amp)) { self.eat(); let r = self.parse_eq()?; e = Expr::Binary(BinaryOp::And, Box::new(e), Box::new(r)); }
        Ok(e)
    }
    fn parse_eq(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_cmp()?;
        loop {
            let op = match self.peek() { Some(Tok::Eq) => BinaryOp::Eq, Some(Tok::Ne) => BinaryOp::Ne, _ => break };
            self.eat(); let r = self.parse_cmp()?; e = Expr::Binary(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }
    fn parse_cmp(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_shift()?;
        loop {
            let op = match self.peek() { Some(Tok::Lt) => BinaryOp::Lt, Some(Tok::Le) => BinaryOp::Le, Some(Tok::Gt) => BinaryOp::Gt, Some(Tok::Ge) => BinaryOp::Ge, _ => break };
            self.eat(); let r = self.parse_shift()?; e = Expr::Binary(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }
    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_add()?;
        loop {
            let op = match self.peek() { Some(Tok::Shl) => BinaryOp::Shl, Some(Tok::Shr) => BinaryOp::Shr, _ => break };
            self.eat(); let r = self.parse_add()?; e = Expr::Binary(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }
    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_mul()?;
        loop {
            let op = match self.peek() { Some(Tok::Plus) => BinaryOp::Add, Some(Tok::Minus) => BinaryOp::Sub, _ => break };
            self.eat(); let r = self.parse_mul()?; e = Expr::Binary(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }
    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut e = self.parse_unary()?;
        loop {
            let op = match self.peek() { Some(Tok::Star) => BinaryOp::Mul, Some(Tok::Slash) => BinaryOp::Div, Some(Tok::Percent) => BinaryOp::Mod, _ => break };
            self.eat(); let r = self.parse_unary()?; e = Expr::Binary(op, Box::new(e), Box::new(r));
        }
        Ok(e)
    }
    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Tok::Minus) => { self.eat(); Ok(Expr::Unary(UnaryOp::Neg, Box::new(self.parse_unary()?))) }
            Some(Tok::Tilde) => { self.eat(); Ok(Expr::Unary(UnaryOp::BitNot, Box::new(self.parse_unary()?))) }
            Some(Tok::Bang) => { self.eat(); Ok(Expr::Unary(UnaryOp::Not, Box::new(self.parse_unary()?))) }
            _ => self.parse_primary(),
        }
    }
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.eat() {
            Some(Tok::Number(n)) => Ok(Expr::Const(n)),
            Some(Tok::Ident(s)) => Ok(Expr::Name(s)),
            Some(Tok::LParen) => { let e = self.parse_ternary()?; match self.eat() { Some(Tok::RParen) => Ok(e), _ => Err("Expected ')'".into()) } }
            Some(t) => Err(format!("Unexpected token {t:?}")),
            None => Err("Unexpected end of expression".into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalValue {
    pub value: BigUint,
    pub bits: Option<u32>,
}

pub fn eval_expr(expr: &Expr, env: &HashMap<String, EvalValue>) -> Option<EvalValue> {
    match expr {
        Expr::Const(v) => Some(EvalValue { value: v.clone(), bits: None }),
        Expr::Name(n) => env.get(n).cloned().or_else(|| {
            // try without source alias prefix issues by exact match only
            None
        }),
        Expr::Unary(op, inner) => {
            let v = eval_expr(inner, env)?;
            let out = match op {
                UnaryOp::Neg => {
                    // two's complement-ish in biguint space not ideal; clamp to 0
                    EvalValue { value: BigUint::zero(), bits: v.bits }
                }
                UnaryOp::Not => EvalValue { value: if v.value.is_zero() { BigUint::from(1u8) } else { BigUint::zero() }, bits: Some(1) },
                UnaryOp::BitNot => {
                    if let Some(bits) = v.bits {
                        let mask = (BigUint::from(1u8) << bits) - 1u8;
                        EvalValue { value: (!v.value) & mask, bits: Some(bits) }
                    } else {
                        EvalValue { value: BigUint::zero(), bits: v.bits }
                    }
                }
            };
            Some(out)
        }
        Expr::Binary(op, l, r) => {
            let a = eval_expr(l, env)?;
            let b = eval_expr(r, env)?;
            let out = match op {
                BinaryOp::Add => a.value + b.value,
                BinaryOp::Sub => if a.value >= b.value { a.value - b.value } else { BigUint::zero() },
                BinaryOp::Mul => a.value * b.value,
                BinaryOp::Div => if b.value.is_zero() { BigUint::zero() } else { a.value / b.value },
                BinaryOp::Mod => if b.value.is_zero() { BigUint::zero() } else { a.value % b.value },
                BinaryOp::And => a.value & b.value,
                BinaryOp::Or => a.value | b.value,
                BinaryOp::Xor => a.value ^ b.value,
                BinaryOp::Shl => { let sh = b.value.to_u32().unwrap_or(0).min(4096); a.value << sh },
                BinaryOp::Shr => { let sh = b.value.to_u32().unwrap_or(0).min(4096); a.value >> sh },
                BinaryOp::Eq => BigUint::from((a.value == b.value) as u8),
                BinaryOp::Ne => BigUint::from((a.value != b.value) as u8),
                BinaryOp::Lt => BigUint::from((a.value < b.value) as u8),
                BinaryOp::Le => BigUint::from((a.value <= b.value) as u8),
                BinaryOp::Gt => BigUint::from((a.value > b.value) as u8),
                BinaryOp::Ge => BigUint::from((a.value >= b.value) as u8),
                BinaryOp::LogicAnd => BigUint::from((!a.value.is_zero() && !b.value.is_zero()) as u8),
                BinaryOp::LogicOr => BigUint::from((!a.value.is_zero() || !b.value.is_zero()) as u8),
            };
            let bits = match op {
                BinaryOp::Eq|BinaryOp::Ne|BinaryOp::Lt|BinaryOp::Le|BinaryOp::Gt|BinaryOp::Ge|BinaryOp::LogicAnd|BinaryOp::LogicOr => Some(1),
                _ => a.bits.or(b.bits),
            };
            Some(EvalValue { value: out, bits })
        }
        Expr::Ternary(c, t, e) => {
            let cv = eval_expr(c, env)?;
            if !cv.value.is_zero() { eval_expr(t, env) } else { eval_expr(e, env) }
        }
    }
}

pub fn query_computed_variable(
    wave_container: &WaveContainer,
    sources: &[ComputedSignalSource],
    expr: &Expr,
    time: &BigUint,
) -> Option<QueryResult> {
    let mut env = HashMap::new();
    let mut next_times: Vec<BigUint> = Vec::new();
    let mut any_current = false;
    let mut current_time: Option<BigUint> = None;

    for src in sources {
        let qr = wave_container.query_variable(&src.variable_ref, time).ok().flatten()?;
        if let Some(nt) = qr.next.clone() { next_times.push(nt); }
        if let Some((ct, val)) = qr.current {
            any_current = true;
            current_time = Some(match current_time { Some(prev) => prev.max(ct.clone()), None => ct.clone() });
            if let Some(bu) = variable_value_to_biguint(&val) {
                let bits = wave_container.variable_meta(&src.variable_ref).ok().and_then(|m| m.num_bits);
                env.insert(src.name.clone(), EvalValue { value: bu, bits });
            } else {
                // X/Z/etc - cannot compute numerically
                return Some(QueryResult { current: None, next: next_times.into_iter().min() });
            }
        } else {
            return Some(QueryResult { current: None, next: next_times.into_iter().min() });
        }
    }

    if !any_current { return Some(QueryResult { current: None, next: next_times.into_iter().min() }); }
    let result = eval_expr(expr, &env)?;
    let value = VariableValue::BigUint(result.value);
    Some(QueryResult {
        current: Some((current_time.unwrap_or_else(|| time.clone()), value)),
        next: next_times.into_iter().min(),
    })
}

pub fn variable_value_to_biguint(v: &VariableValue) -> Option<BigUint> {
    match v {
        VariableValue::BigUint(b) => Some(b.clone()),
        VariableValue::String(s) => {
            let t = s.trim();
            if t.is_empty() { return Some(BigUint::zero()); }
            if t.chars().any(|c| matches!(c, 'x'|'X'|'z'|'Z'|'u'|'U'|'w'|'W'|'h'|'H'|'l'|'L'|'-')) {
                return None;
            }
            if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                return BigUint::parse_bytes(h.as_bytes(), 16);
            }
            if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
                return BigUint::parse_bytes(b.as_bytes(), 2);
            }
            if t.chars().all(|c| c=='0' || c=='1') && t.len() > 1 {
                return BigUint::parse_bytes(t.as_bytes(), 2);
            }
            BigUint::parse_bytes(t.as_bytes(), 10)
        }
    }
}
