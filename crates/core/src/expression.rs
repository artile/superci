//! GitHub's expressions (`${{ … }}`), for what the control plane decides of a run from a computer before any runner
//! is involved: whether a job runs (its `if`), what it is called and where it runs when a matrix says, and what its
//! `needs` are. The runner reads the steps' own expressions itself.
//!
//! As GitHub's: literals (`null`, `true`, numbers, `'text'` with `''` for a quote), contexts (`needs.build.result`,
//! `matrix['node']`), `!`, `<`, `<=`, `>`, `>=`, `==`, `!=`, `&&`, `||` (which give one of their sides, not a bool),
//! comparison of text without regard to case, and the functions `contains`, `startsWith`, `endsWith`, `format`,
//! `join`, `toJSON`, `fromJSON`, `success`, `failure`, `cancelled`, `always`.
use serde_json::{json, Value};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, PartialEq)]
enum Tok { Num(f64), Str(String), Ident(String), Op(&'static str), LParen, RParen, LBracket, RBracket, Comma, Dot, Star }

fn lex(s: &str) -> Result<Vec<Tok>> {
    let (c, mut i, mut out) = (s.chars().collect::<Vec<_>>(), 0, vec![]);
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() { i += 1; continue }
        if ch == '\'' {
            let mut text = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return Err(format!("text not closed in {s}")),
                    Some('\'') if c.get(i + 1) == Some(&'\'') => { text.push('\''); i += 2 }
                    Some('\'') => { i += 1; break }
                    Some(x) => { text.push(*x); i += 1 }
                }
            }
            out.push(Tok::Str(text));
            continue
        }
        if ch.is_ascii_digit() || (ch == '-' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = i;
            i += 1;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.' || ((c[i] == '-' || c[i] == '+') && matches!(c[i - 1], 'e' | 'E'))) { i += 1 }
            let t: String = c[start..i].iter().collect();
            let n = if let Some(h) = t.strip_prefix("0x") { i64::from_str_radix(h, 16).map(|v| v as f64).ok() } else { t.parse::<f64>().ok() };
            out.push(Tok::Num(n.ok_or_else(|| format!("not a number: {t}"))?));
            continue
        }
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '_' || c[i] == '-') { i += 1 }
            out.push(Tok::Ident(c[start..i].iter().collect()));
            continue
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        if let Some(op) = ["==", "!=", "<=", ">=", "&&", "||"].iter().find(|o| **o == two) { out.push(Tok::Op(op)); i += 2; continue }
        out.push(match ch {
            '(' => Tok::LParen, ')' => Tok::RParen, '[' => Tok::LBracket, ']' => Tok::RBracket, ',' => Tok::Comma, '.' => Tok::Dot, '*' => Tok::Star,
            '!' => Tok::Op("!"), '<' => Tok::Op("<"), '>' => Tok::Op(">"),
            other => return Err(format!("unexpected {other} in {s}")),
        });
        i += 1;
    }
    Ok(out)
}

#[derive(Debug, Clone)]
enum Expr { Lit(Value), Ctx(String), Index(Box<Expr>, Box<Expr>), Filter(Box<Expr>), Not(Box<Expr>), Bin(&'static str, Box<Expr>, Box<Expr>), Call(String, Vec<Expr>) }

struct P { t: Vec<Tok>, i: usize }

impl P {
    fn peek(&self) -> Option<&Tok> { self.t.get(self.i) }
    fn next(&mut self) -> Option<Tok> { let t = self.t.get(self.i).cloned(); self.i += 1; t }
    fn expect(&mut self, t: Tok) -> Result<()> { if self.next().as_ref() == Some(&t) { Ok(()) } else { Err(format!("expected {t:?}")) } }
    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: [&[&str]; 4] = [&["||"], &["&&"], &["==", "!="], &["<", "<=", ">", ">="]];
        if level == LEVELS.len() { return self.unary() }
        let mut left = self.binary(level + 1)?;
        while let Some(Tok::Op(op)) = self.peek().cloned() {
            if !LEVELS[level].contains(&op) { break }
            self.i += 1;
            left = Expr::Bin(op, Box::new(left), Box::new(self.binary(level + 1)?));
        }
        Ok(left)
    }
    fn unary(&mut self) -> Result<Expr> {
        if self.peek() == Some(&Tok::Op("!")) { self.i += 1; return Ok(Expr::Not(Box::new(self.unary()?))) }
        let mut e = match self.next().ok_or("an expression ends too soon")? {
            Tok::Num(n) => Expr::Lit(json!(n)),
            Tok::Str(s) => Expr::Lit(json!(s)),
            Tok::LParen => { let e = self.binary(0)?; self.expect(Tok::RParen)?; e }
            Tok::Ident(id) => match id.as_str() {
                "true" => Expr::Lit(json!(true)), "false" => Expr::Lit(json!(false)), "null" => Expr::Lit(Value::Null),
                "NaN" => Expr::Lit(json!(f64::NAN)), "Infinity" => Expr::Lit(json!(f64::INFINITY)),
                _ if self.peek() == Some(&Tok::LParen) => {
                    self.i += 1;
                    let mut args = vec![];
                    if self.peek() != Some(&Tok::RParen) { loop { args.push(self.binary(0)?); if self.peek() == Some(&Tok::Comma) { self.i += 1 } else { break } } }
                    self.expect(Tok::RParen)?;
                    Expr::Call(id.to_ascii_lowercase(), args)
                }
                _ => Expr::Ctx(id),
            },
            t => return Err(format!("unexpected {t:?}")),
        };
        loop {
            match self.peek() {
                Some(Tok::Dot) => {
                    self.i += 1;
                    e = match self.next() { Some(Tok::Ident(p)) => Expr::Index(Box::new(e), Box::new(Expr::Lit(json!(p)))), Some(Tok::Star) => Expr::Filter(Box::new(e)), t => return Err(format!("unexpected {t:?} after a dot")) };
                }
                Some(Tok::LBracket) => {
                    self.i += 1;
                    if self.peek() == Some(&Tok::Star) { self.i += 1; self.expect(Tok::RBracket)?; e = Expr::Filter(Box::new(e)); continue }
                    let i = self.binary(0)?;
                    self.expect(Tok::RBracket)?;
                    e = Expr::Index(Box::new(e), Box::new(i));
                }
                _ => return Ok(e),
            }
        }
    }
}

/// What a job's status functions answer: whether everything before went well (`success()`), something failed, or
/// the run was cancelled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Status { #[default] Success, Failure, Cancelled }

/// The contexts an expression reads (`needs`, `matrix`, `github`, …), by name, and how the job stands.
pub struct Contexts<'a> { pub values: &'a serde_json::Map<String, Value>, pub status: Status }

pub fn truthy(v: &Value) -> bool {
    match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()), Value::String(s) => !s.is_empty(), _ => true }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0, Value::Bool(b) => if *b { 1.0 } else { 0.0 }, Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) if s.trim().is_empty() => 0.0,
        Value::String(s) => { let t = s.trim(); t.strip_prefix("0x").and_then(|h| i64::from_str_radix(h, 16).ok()).map(|n| n as f64).unwrap_or_else(|| t.parse::<f64>().unwrap_or(f64::NAN)) }
        _ => f64::NAN,
    }
}

/// A value as text, as GitHub writes one into text (`format`, a value's place in a string).
pub fn text(v: &Value) -> String {
    match v {
        Value::Null => String::new(), Value::Bool(b) => b.to_string(), Value::String(s) => s.clone(),
        Value::Number(n) => { let f = n.as_f64().unwrap_or_default(); if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { f.to_string() } }
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Loose equality, as GitHub's: text without regard to case; otherwise both as numbers.
fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x.to_lowercase() == y.to_lowercase(),
        (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => { let (x, y) = (number(a), number(b)); x == y }
    }
}

fn eval(e: &Expr, c: &Contexts) -> Result<Value> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Ctx(name) => c.values.get(&name.to_ascii_lowercase()).or_else(|| c.values.get(name)).cloned().unwrap_or(Value::Null),
        Expr::Index(on, i) => {
            let (on, i) = (eval(on, c)?, eval(i, c)?);
            match (&on, &i) {
                (Value::Object(m), _) => { let k = text(&i); m.get(&k).or_else(|| m.iter().find(|(x, _)| x.eq_ignore_ascii_case(&k)).map(|(_, v)| v)).cloned().unwrap_or(Value::Null) }
                (Value::Array(a), _) => a.get(number(&i) as usize).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            }
        }
        Expr::Filter(on) => match eval(on, c)? { Value::Object(m) => Value::Array(m.into_values().collect()), Value::Array(a) => Value::Array(a), _ => json!([]) },
        Expr::Not(x) => json!(!truthy(&eval(x, c)?)),
        Expr::Bin("&&", a, b) => { let a = eval(a, c)?; if truthy(&a) { eval(b, c)? } else { a } }
        Expr::Bin("||", a, b) => { let a = eval(a, c)?; if truthy(&a) { a } else { eval(b, c)? } }
        Expr::Bin(op, a, b) => {
            let (a, b) = (eval(a, c)?, eval(b, c)?);
            match *op {
                "==" => json!(equal(&a, &b)), "!=" => json!(!equal(&a, &b)),
                _ => {
                    let order = match (&a, &b) { (Value::String(x), Value::String(y)) => x.to_lowercase().partial_cmp(&y.to_lowercase()), _ => number(&a).partial_cmp(&number(&b)) };
                    json!(match (*op, order) { ("<", Some(o)) => o.is_lt(), ("<=", Some(o)) => o.is_le(), (">", Some(o)) => o.is_gt(), (">=", Some(o)) => o.is_ge(), _ => false })
                }
            }
        }
        Expr::Call(f, args) => {
            let a = args.iter().map(|x| eval(x, c)).collect::<Result<Vec<_>>>()?;
            let arg = |i: usize| a.get(i).cloned().unwrap_or(Value::Null);
            match f.as_str() {
                "success" => json!(c.status == Status::Success), "failure" => json!(c.status == Status::Failure), "cancelled" => json!(c.status == Status::Cancelled), "always" => json!(true),
                "contains" => json!(match arg(0) { Value::Array(items) => items.iter().any(|x| equal(x, &arg(1))), v => text(&v).to_lowercase().contains(&text(&arg(1)).to_lowercase()) }),
                "startswith" => json!(text(&arg(0)).to_lowercase().starts_with(&text(&arg(1)).to_lowercase())),
                "endswith" => json!(text(&arg(0)).to_lowercase().ends_with(&text(&arg(1)).to_lowercase())),
                "join" => { let sep = if a.len() > 1 { text(&arg(1)) } else { ",".into() }; json!(match arg(0) { Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join(&sep), v => text(&v) }) }
                "tojson" => json!(serde_json::to_string_pretty(&arg(0)).unwrap_or_default()),
                "fromjson" => serde_json::from_str(&text(&arg(0))).map_err(|e| format!("fromJSON: {e}"))?,
                "format" => {
                    let (f, mut out, mut chars) = (text(&arg(0)), String::new(), vec![]);
                    chars.extend(f.chars());
                    let mut i = 0;
                    while i < chars.len() {
                        match (chars[i], chars.get(i + 1)) {
                            ('{', Some('{')) => { out.push('{'); i += 2 }
                            ('}', Some('}')) => { out.push('}'); i += 2 }
                            ('{', _) => {
                                let end = chars[i..].iter().position(|x| *x == '}').ok_or("format: { not closed")? + i;
                                let n: usize = chars[i + 1..end].iter().collect::<String>().parse().map_err(|_| "format: not a number in {}")?;
                                out.push_str(&text(&arg(n + 1)));
                                i = end + 1;
                            }
                            (x, _) => { out.push(x); i += 1 }
                        }
                    }
                    json!(out)
                }
                other => return Err(format!("{other}() is not a function an expression has here")),
            }
        }
    })
}

/// The value of an expression (what is inside `${{ }}`).
pub fn evaluate(expr: &str, c: &Contexts) -> Result<Value> {
    let mut p = P { t: lex(expr)?, i: 0 };
    let e = p.binary(0)?;
    if p.i != p.t.len() { return Err(format!("more than one expression in {expr}")) }
    eval(&e, c)
}

/// Whether an `if:` holds. Written with or without `${{ }}`; one that does not ask how the job stands itself holds
/// only when everything before went well.
pub fn holds(condition: &str, c: &Contexts) -> Result<bool> {
    let t = condition.trim();
    let inner = t.strip_prefix("${{").and_then(|x| x.strip_suffix("}}")).map(str::trim).unwrap_or(t);
    let asks = ["success", "failure", "cancelled", "always"].iter().any(|f| inner.to_ascii_lowercase().contains(&format!("{f}(")));
    let v = truthy(&evaluate(inner, c)?);
    Ok(if asks { v } else { v && c.status == Status::Success })
}

/// A text with `${{ }}` in it, each replaced by its value as text (a job's name, its `runs-on`).
pub fn interpolate(s: &str, c: &Contexts) -> Result<String> {
    let (mut out, mut rest) = (String::new(), s);
    while let Some(at) = rest.find("${{") {
        out.push_str(&rest[..at]);
        let end = rest[at..].find("}}").ok_or_else(|| format!("${{{{ not closed in {s}"))? + at;
        out.push_str(&text(&evaluate(&rest[at + 3..end], c)?));
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> serde_json::Map<String, Value> {
        json!({ "needs": { "build": { "result": "success", "outputs": { "version": "1.2", "list": "[\"a\",\"b\"]" } } }, "matrix": { "node": 20, "os": "linux" }, "github": { "event_name": "push", "ref": "refs/heads/main" } }).as_object().unwrap().clone()
    }

    #[test]
    fn expressions_are_read_as_github_reads_them() {
        let values = ctx();
        let c = Contexts { values: &values, status: Status::Success };
        let e = |s: &str| evaluate(s, &c).unwrap();
        assert_eq!(e("needs.build.outputs.version"), json!("1.2"));
        assert_eq!(e("needs['build'].result == 'SUCCESS'"), json!(true));
        assert_eq!(e("matrix.node == '20'"), json!(true));
        assert_eq!(e("github.event_name == 'push' && 'yes' || 'no'"), json!("yes"));
        assert_eq!(e("!contains(github.ref, 'tags') && startsWith(github.ref, 'refs/heads/')"), json!(true));
        assert_eq!(e("format('node {0} on {1}, {{literal}}', matrix.node, matrix.os)"), json!("node 20 on linux, {literal}"));
        assert_eq!(e("fromJSON(needs.build.outputs.list)[1]"), json!("b"));
        assert_eq!(e("join(fromJSON(needs.build.outputs.list), '+')"), json!("a+b"));
        assert_eq!(e("contains(fromJSON('[1, 2]'), 2) && 3 >= 2.5 && 'b' > 'A'"), json!(true));
        assert_eq!(e("needs.missing.result"), Value::Null);
        assert_eq!(e("'it''s'"), json!("it's"));
        assert!(evaluate("nope(1)", &c).is_err() && evaluate("1 2", &c).is_err());
        // An `if:` holds only when all went well, unless it asks.
        let failed = Contexts { values: &values, status: Status::Failure };
        assert!(holds("${{ github.event_name == 'push' }}", &c).unwrap() && !holds("github.event_name == 'push'", &failed).unwrap());
        assert!(holds("always()", &failed).unwrap() && holds("failure() && needs.build.result == 'success'", &failed).unwrap() && !holds("success()", &failed).unwrap());
        assert_eq!(interpolate("Test ${{ matrix.node }} on ${{ matrix.os }}", &c).unwrap(), "Test 20 on linux");
    }
}
