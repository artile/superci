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
    match v { Value::Null => false, Value::Bool(b) => *b, Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0), Value::String(s) => !s.is_empty(), _ => true }
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

/// One value of an object (by its key, in any case) or an array (by its place).
fn index(on: &Value, i: &Value) -> Value {
    match on {
        Value::Object(m) => { let k = text(i); m.get(&k).or_else(|| m.iter().find(|(x, _)| x.eq_ignore_ascii_case(&k)).map(|(_, v)| v)).cloned().unwrap_or(Value::Null) }
        Value::Array(a) => a.get(number(i) as usize).cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Whether an expression is a filter's items (`x.*`, and what is read of each after it).
fn filtered(e: &Expr) -> bool { match e { Expr::Filter(_) => true, Expr::Index(on, _) => filtered(on), _ => false } }

fn eval(e: &Expr, c: &Contexts) -> Result<Value> {
    Ok(match e {
        Expr::Lit(v) => v.clone(),
        Expr::Ctx(name) => c.values.get(&name.to_ascii_lowercase()).or_else(|| c.values.get(name)).cloned().unwrap_or(Value::Null),
        // After a filter (`labels.*.name`): each item's, those that have it.
        Expr::Index(on, i) if filtered(on) => {
            let (on, i) = (eval(on, c)?, eval(i, c)?);
            Value::Array(on.as_array().into_iter().flatten().map(|item| index(item, &i)).filter(|v| !v.is_null()).collect())
        }
        Expr::Index(on, i) => {
            let (on, i) = (eval(on, c)?, eval(i, c)?);
            index(&on, &i)
        }
        Expr::Filter(on) if filtered(on) => Value::Array(eval(on, c)?.as_array().into_iter().flatten().flat_map(|v| match v { Value::Object(m) => m.values().cloned().collect(), Value::Array(a) => a.clone(), _ => vec![] }).collect()),
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
        // A pull request's labels, as label conditions read them.
        let labelled = json!({ "github": { "event": { "pull_request": { "labels": [{ "name": "wip" }, { "name": "e2e-recordings" }] } } }, "inputs": {} }).as_object().unwrap().clone();
        let lc = Contexts { values: &labelled, status: Status::Success };
        assert_eq!(evaluate("github.event.pull_request.labels.*.name", &lc).unwrap(), json!(["wip", "e2e-recordings"]));
        assert!(holds("contains(github.event.pull_request.labels.*.name, 'e2e-recordings') || inputs.record", &lc).unwrap());
        assert_eq!(e("needs.missing.result"), Value::Null);
        assert_eq!(e("'it''s'"), json!("it's"));
        assert!(evaluate("nope(1)", &c).is_err() && evaluate("1 2", &c).is_err());
        // An `if:` holds only when all went well, unless it asks.
        let failed = Contexts { values: &values, status: Status::Failure };
        assert!(holds("${{ github.event_name == 'push' }}", &c).unwrap() && !holds("github.event_name == 'push'", &failed).unwrap());
        assert!(holds("always()", &failed).unwrap() && holds("failure() && needs.build.result == 'success'", &failed).unwrap() && !holds("success()", &failed).unwrap());
        assert_eq!(interpolate("Test ${{ matrix.node }} on ${{ matrix.os }}", &c).unwrap(), "Test 20 on linux");
    }

    fn eval_in(s: &str) -> Result<Value> {
        let values = json!({ "a": { "B": 1, "list": [1, 2], "items": [{ "n": 1 }, { "n": 2 }, { "m": 3 }], "nested": [[1], { "k": 2 }, 3] }, "Upper": "kept" }).as_object().unwrap().clone();
        evaluate(s, &Contexts { values: &values, status: Status::Cancelled })
    }

    #[test]
    fn every_literal_and_token_reads_as_github_reads_it() {
        let cases: &[(&str, Value)] = &[
            ("null", Value::Null), ("true", json!(true)), ("false", json!(false)),
            // JSON has no NaN or Infinity: both are null once read.
            ("NaN", Value::Null), ("Infinity", Value::Null),
            ("0x1F", json!(31.0)), ("-2", json!(-2.0)), ("1e-3", json!(0.001)), ("2E+2", json!(200.0)), ("1.5", json!(1.5)),
            ("'it''s'", json!("it's")), ("''", json!("")), ("  'x'  ", json!("x")),
            ("a.B", json!(1)), ("a.b", json!(1)), ("a['b']", json!(1)), ("a.list[1]", json!(2)), ("a.list[5]", Value::Null),
            ("a.list.*", json!([1, 2])), ("a.list[*]", json!([1, 2])), ("a.*", json!([1, [{ "n": 1 }, { "n": 2 }, { "m": 3 }], [1, 2], [[1], { "k": 2 }, 3]])),
            ("a.items.*.n", json!([1, 2])), ("a.nested.*.*", json!([1, 2])), ("'text'.*", json!([])), ("'text'.x", Value::Null),
            ("Upper", json!("kept")), ("A.B", json!(1)), ("missing", Value::Null), ("under_score-dash", Value::Null),
            ("(1)", json!(1.0)), ("!0", json!(true)), ("!!'x'", json!(true)), ("!fromJSON('[]')", json!(false)),
            ("1 < 2", json!(true)), ("2 < 1", json!(false)), ("1 <= 1", json!(true)), ("2 <= 1", json!(false)),
            ("2 > 1", json!(true)), ("1 > 2", json!(false)), ("1 >= 1", json!(true)), ("1 >= 2", json!(false)),
            ("'a' < 'B'", json!(true)), ("'abc' < 1", json!(false)), ("fromJSON('[1]') < 2", json!(false)), ("null < 1", json!(true)),
            ("true == 1", json!(true)), ("false == 0", json!(true)), ("'0x10' == 16", json!(true)), ("' ' == 0", json!(true)), ("null == null", json!(true)),
            ("fromJSON('[1]') == fromJSON('[1]')", json!(true)), ("fromJSON('{}') != 1", json!(true)), ("'A' != 'a'", json!(false)),
            ("0 && 1", json!(0.0)), ("1 && 2", json!(2.0)), ("0 || 'b'", json!("b")), ("'a' || 'b'", json!("a")),
        ];
        for (s, want) in cases { assert_eq!(eval_in(s).unwrap(), *want, "{s}"); }
    }

    #[test]
    fn every_function_answers_as_github_answers() {
        let cases: &[(&str, Value)] = &[
            ("success()", json!(false)), ("failure()", json!(false)), ("cancelled()", json!(true)), ("always()", json!(true)),
            ("contains('Hello', 'ELL')", json!(true)), ("contains(fromJSON('[1, 2]'), 3)", json!(false)),
            ("startsWith('Hello', 'he')", json!(true)), ("endsWith('Hello', 'LO')", json!(true)), ("endsWith('Hello', 'x')", json!(false)),
            ("join(a.list)", json!("1,2")), ("join('one', '-')", json!("one")), ("toJSON(a.list)", json!("[\n  1,\n  2\n]")),
            ("fromJSON('{\"x\": 1}').x", json!(1)),
            ("format('')", json!("")), ("format('{0} {1} {2}', true, 1.5, null)", json!("true 1.5 ")), ("format('a}b {{}}')", json!("a}b {}")),
            ("format('{0}', 1e15)", json!("1000000000000000")), ("format('{0}', fromJSON('{\"k\": 1}'))", json!("{\n  \"k\": 1\n}")),
            ("FORMAT('{0}', 'x')", json!("x")),
        ];
        for (s, want) in cases { assert_eq!(eval_in(s).unwrap(), *want, "{s}"); }
    }

    #[test]
    fn every_malformed_expression_says_what_is_wrong() {
        let cases: &[(&str, &str)] = &[
            ("", "an expression ends too soon"), ("'open", "text not closed in 'open"), ("0xZZ", "not a number: 0xZZ"), ("1.2.3", "not a number: 1.2.3"),
            ("a # b", "unexpected # in a # b"), ("(1", "expected RParen"), ("(", "an expression ends too soon"), (")", "unexpected RParen"),
            ("a.", "unexpected None after a dot"), ("a.1", "unexpected Some(Num(1.0)) after a dot"), ("a[*", "expected RBracket"),
            ("a[", "an expression ends too soon"), ("a[1", "expected RBracket"), ("!", "an expression ends too soon"),
            ("1 ==", "an expression ends too soon"), ("f(1", "expected RParen"), ("f(", "an expression ends too soon"), ("1 2", "more than one expression in 1 2"),
            ("nope()", "nope() is not a function an expression has here"), ("fromJSON('x')", "fromJSON: expected value at line 1 column 1"),
            ("format('{0')", "format: { not closed"), ("format('{x}')", "format: not a number in {}"),
        ];
        for (s, want) in cases { assert_eq!(eval_in(s).unwrap_err(), *want, "{s}"); }
    }

    #[test]
    fn an_error_inside_any_part_of_an_expression_is_the_whole_expressions_error() {
        for s in ["nope().x", "a[nope()]", "nope().*.x", "a.*[nope()]", "nope().*.*", "nope().*", "!nope()", "nope() && 1", "1 && nope()",
            "nope() || 1", "0 || nope()", "nope() == 1", "1 == nope()", "contains(nope())"] {
            assert_eq!(eval_in(s).unwrap_err(), "nope() is not a function an expression has here", "{s}");
        }
    }

    #[test]
    fn a_condition_or_text_with_a_bad_expression_says_so() {
        let values = ctx();
        let c = Contexts { values: &values, status: Status::Success };
        assert!(holds("${{ 'x' }}", &c).unwrap() && !holds("${{ '' }}", &c).unwrap());
        assert_eq!(holds("${{ ( }}", &c).unwrap_err(), "an expression ends too soon");
        assert_eq!(interpolate("", &c).unwrap(), "");
        assert_eq!(interpolate("a ${{ 1", &c).unwrap_err(), "${{ not closed in a ${{ 1");
        assert_eq!(interpolate("${{ nope() }}", &c).unwrap_err(), "nope() is not a function an expression has here");
        assert!(truthy(&json!([])) && !truthy(&json!(0)));
    }
}
