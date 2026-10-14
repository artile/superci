//! A workflow file's job as the message GitHub's runner is handed for it. GitHub's own service writes this message
//! when a job starts; for a run that does not go through GitHub (started from a computer, with the working tree as
//! it is), the control plane writes it, and GitHub's runner on the machine runs the steps as it runs any job's.
//!
//! What is written here is held to what was recorded of real jobs (`tests/fixtures/runner`): the message GitHub
//! sent the runner for a workflow, and the one an open-source stand-in for GitHub's service made for another.
//! The runner reads the steps' own expressions itself (`with:`, `env:`, `if:`): they are passed on as they are
//! written, each value as a "template token" that says what it is and where in the file it stands.
use serde_json::{json, Value};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

type Result<T> = std::result::Result<T, String>;

/// A workflow file as it is written: every value with its own text (a number or a `true` is kept as typed, the
/// runner is given text) and its place in the file.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Scalar { text: String, plain: bool, line: usize, col: usize },
    Seq { items: Vec<Node>, line: usize, col: usize },
    Map { pairs: Vec<(Node, Node)>, line: usize, col: usize },
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self { Node::Map { pairs, .. } => pairs.iter().find(|(k, _)| k.text() == Some(key)).map(|(_, v)| v), _ => None }
    }
    pub fn text(&self) -> Option<&str> { match self { Node::Scalar { text, .. } => Some(text), _ => None } }
    pub fn items(&self) -> &[Node] { match self { Node::Seq { items, .. } => items, _ => &[] } }
    pub fn pairs(&self) -> &[(Node, Node)] { match self { Node::Map { pairs, .. } => pairs, _ => &[] } }
    fn place(&self) -> (usize, usize) { match self { Node::Scalar { line, col, .. } | Node::Seq { line, col, .. } | Node::Map { line, col, .. } => (*line, *col) } }
    /// Nothing written where a value could be (`env:` with nothing under it).
    fn empty(&self) -> bool { matches!(self, Node::Scalar { text, plain: true, .. } if text.is_empty() || text == "~" || text == "null") }
}

struct Build { stack: Vec<Node>, keys: Vec<Vec<Node>>, done: Option<Node>, error: Option<String>,
    /// The scalar being put is a block (`|`, `>`): it stands where its mark is, after its key, not where its text begins.
    block: bool }

impl Build {
    fn put(&mut self, node: Node) {
        match self.stack.last_mut() {
            None => self.done = Some(node),
            Some(Node::Seq { items, .. }) => items.push(node),
            Some(Node::Map { pairs, .. }) => {
                let keys = self.keys.last_mut().expect("a key list for every mapping");
                match keys.pop() {
                    Some(key) => {
                        let node = match (node, &key) {
                            (Node::Scalar { text, plain, .. }, Node::Scalar { text: k, line, col, .. }) if self.block => Node::Scalar { text, plain, line: *line, col: col + k.chars().count() + 2 },
                            (node, _) => node,
                        };
                        pairs.push((key, node))
                    }
                    None => keys.push(node),
                }
            }
            Some(Node::Scalar { .. }) => {}
        }
    }
}

impl MarkedEventReceiver for Build {
    fn on_event(&mut self, event: Event, mark: Marker) {
        // (Lines from 1 and columns from 1, as GitHub counts them.)
        let (line, col) = (mark.line(), mark.col() + 1);
        match event {
            Event::Scalar(text, style, ..) => {
                self.block = matches!(style, TScalarStyle::Literal | TScalarStyle::Folded);
                self.put(Node::Scalar { text, plain: style == TScalarStyle::Plain, line, col });
                self.block = false;
            }
            Event::SequenceStart(..) => self.stack.push(Node::Seq { items: vec![], line, col }),
            Event::MappingStart(..) => { self.stack.push(Node::Map { pairs: vec![], line, col }); self.keys.push(vec![]) }
            Event::SequenceEnd => if let Some(n) = self.stack.pop() { self.put(n) },
            Event::MappingEnd => { self.keys.pop(); if let Some(n) = self.stack.pop() { self.put(n) } }
            Event::Alias(_) => self.error = Some(format!("line {line}: anchors and aliases are not read here yet")),
            _ => {}
        }
    }
}

/// Reads a workflow file.
pub fn parse(yaml: &str) -> Result<Node> {
    let mut build = Build { stack: vec![], keys: vec![], done: None, error: None, block: false };
    Parser::new_from_str(yaml).load(&mut build, false).map_err(|e| format!("the workflow file is not YAML: {e}"))?;
    if let Some(e) = build.error { return Err(e) }
    build.done.ok_or_else(|| "the workflow file is empty".to_string())
}

/// The parts of a text between its `${{ … }}`: text as it is, and expressions (their inside, trimmed). A `}}`
/// inside a quoted string of an expression does not end it.
fn parts(text: &str) -> Vec<(bool, String)> {
    let (mut out, mut rest) = (vec![], text);
    while let Some(at) = rest.find("${{") {
        let inside = &rest[at + 3..];
        let (mut quoted, mut end) = (false, None);
        let bytes = inside.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\'' => quoted = !quoted,
                b'}' if !quoted && bytes.get(i + 1) == Some(&b'}') => { end = Some(i); break }
                _ => {}
            }
            i += 1;
        }
        let Some(end) = end else { break };
        if at > 0 { out.push((false, rest[..at].to_string())) }
        out.push((true, inside[..end].trim().to_string()));
        rest = &inside[end + 2..];
    }
    if !rest.is_empty() { out.push((false, rest.to_string())) }
    out
}

/// A value's text as the runner is given it: text as it is, or the expression it is. Text with expressions in it
/// is one expression, `format('before {0} after', the.expression)`.
fn text_token(text: &str, at: Value) -> Value {
    let parts = parts(text);
    let mut token = at;
    match parts.as_slice() {
        [(true, expr)] => { token["type"] = 3.into(); token["expr"] = expr.clone().into() }
        p if p.iter().any(|(is, _)| *is) => {
            let (mut format, mut args) = (String::new(), vec![]);
            for (is, s) in p {
                if *is { format.push_str(&format!("{{{}}}", args.len())); args.push(s.clone()) }
                else { format.push_str(&s.replace('\'', "''").replace('{', "{{").replace('}', "}}")) }
            }
            token["type"] = 3.into();
            token["expr"] = format!("format('{format}', {})", args.join(", ")).into();
        }
        _ => { token["type"] = 0.into(); token["lit"] = text.into() }
    }
    token
}

/// The order GitHub writes a token's fields in: what it is, where it stands, then its value.
fn at(kind: u8, file: u32, node: &Node) -> Value {
    let (line, col) = node.place();
    json!({ "type": kind, "file": file, "line": line, "col": col })
}

/// A value as a template token: text, a list, or a mapping, each with its place in the file.
pub fn token(node: &Node, file: u32) -> Value {
    match node {
        Node::Scalar { text, .. } => text_token(text, at(0, file, node)),
        Node::Seq { items, .. } => { let mut t = at(1, file, node); t["seq"] = items.iter().map(|n| token(n, file)).collect(); t }
        Node::Map { pairs, .. } => {
            // (A mapping stands where its first key stands.)
            let mut t = at(2, file, pairs.first().map(|(k, _)| k).unwrap_or(node));
            t["map"] = pairs.iter().map(|(k, v)| json!({ "key": token(k, file), "value": token(v, file) })).collect();
            t
        }
    }
}

/// A step's `if:` as the runner is given it: the expression alone (its `${{ }}` taken off), and, unless it asks
/// about how the job is going itself (`success()`, `failure()`, `always()`, `cancelled()`), only when all went well.
fn condition(step: &Node) -> String {
    let Some(text) = step.get("if").and_then(Node::text) else { return "success()".into() };
    let expr = match parts(text).as_slice() { [(true, e)] => e.clone(), _ => text.trim().to_string() };
    let asks = ["success", "failure", "always", "cancelled"].iter().any(|f| {
        expr.match_indices(f).any(|(i, _)| expr[i + f.len()..].trim_start().starts_with('(') && !expr[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    });
    if asks { expr } else { format!("success() && ({expr})") }
}

/// What a step is called where its outputs are kept, when it has no `id` of its own: `__run` for a script,
/// `__actions_checkout` for an action, with `_2`, `_3` for the next of the same.
fn context_name(base: &str, seen: &mut Vec<String>, own: bool) -> String {
    // (A step with an id of its own is called that, and is not counted among the others.)
    if own { return String::new() }
    let base: String = format!("__{}", base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>());
    let n = seen.iter().filter(|s| **s == base).count();
    seen.push(base.clone());
    if n == 0 { base } else { format!("{base}_{}", n + 1) }
}

/// One step as the runner is given it.
fn step(node: &Node, file: u32, seen: &mut Vec<String>, id: String) -> Result<Value> {
    let (line, _) = node.place();
    let own_id = node.get("id").and_then(Node::text).map(str::to_string);
    let own = own_id.is_some();
    let mut out = json!({ "type": "action" });
    let name = if let Some(run) = node.get("run") {
        out["reference"] = json!({ "type": "script" });
        let mut inputs = vec![json!({ "key": "script", "value": token(run, file) })];
        if let Some(d) = node.get("working-directory") { inputs.push(json!({ "key": "workingDirectory", "value": token(d, file) })) }
        if let Some(s) = node.get("shell") { inputs.push(json!({ "key": "shell", "value": token(s, file) })) }
        out["inputs"] = json!({ "type": 2, "map": inputs });
        context_name("run", seen, own)
    } else if let Some(uses) = node.get("uses").and_then(Node::text) {
        if let Some(image) = uses.strip_prefix("docker://") {
            out["reference"] = json!({ "type": "containerRegistry", "image": image });
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            context_name(image, seen, own)
        } else if uses.starts_with("./") || uses.starts_with(".\\") {
            out["reference"] = json!({ "type": "repository", "path": uses, "repositoryType": "self" });
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            context_name("self", seen, own)
        } else {
            let (what, reference) = uses.rsplit_once('@').ok_or_else(|| format!("line {line}: `uses: {uses}` names no version (owner/name@version)"))?;
            let mut names = what.splitn(3, '/');
            let (owner, repo, path) = (names.next().unwrap_or_default(), names.next().ok_or_else(|| format!("line {line}: `uses: {uses}` is not owner/name@version"))?, names.next().unwrap_or_default());
            out["reference"] = json!({ "type": "repository", "name": format!("{owner}/{repo}"), "ref": reference, "repositoryType": "GitHub" });
            // (An action in a folder of its repository. GitHub leaves the field out for one at the top.)
            if !path.is_empty() { out["reference"]["path"] = path.into() }
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            context_name(&format!("{owner}/{repo}"), seen, own)
        }
    } else {
        return Err(format!("line {line}: a step has `run` or `uses`"))
    };
    let name = own_id.unwrap_or(name);
    if let Some(n) = node.get("name") { out["displayNameToken"] = token(n, file) }
    out["contextName"] = name.clone().into();
    if let Some(env) = node.get("env").filter(|e| !e.empty()) { out["environment"] = token(env, file) }
    out["condition"] = condition(node).into();
    // (`true`, `3`: what they are, when written so; an expression is left to the runner.)
    for (key, field, kind, value) in [("continue-on-error", "continueOnError", 5, "bool"), ("timeout-minutes", "timeoutInMinutes", 6, "num")] {
        let Some(n) = node.get(key) else { continue };
        let text = n.text().unwrap_or_default();
        let mut t = at(kind, file, n);
        match (kind, text.parse::<bool>(), text.parse::<f64>()) {
            (5, Ok(b), _) => t[value] = b.into(),
            (6, _, Ok(n)) => t[value] = n.into(),
            _ => t = token(n, file),
        }
        out[field] = t;
    }
    out["id"] = id.into();
    out["name"] = name.into();
    Ok(out)
}

/// What a workflow file decides of a job's message: its steps, the environment and defaults of the workflow and of
/// the job, its outputs, its container and its service containers. `ids`: an id for each step, in order.
pub fn job_parts(workflow: &Node, path: &str, job: &str, ids: &mut dyn FnMut() -> String) -> Result<Value> {
    let found = workflow.get("jobs").and_then(|j| j.get(job)).ok_or_else(|| format!("{path} has no job named {job}"))?;
    // (The file table: the workflow file is the first and only file.)
    let file = 1;
    let mut out = json!({ "fileTable": [path] });
    let both = |key: &str| -> Vec<Value> { [workflow.get(key), found.get(key)].into_iter().flatten().filter(|n| !n.empty()).map(|n| token(n, file)).collect() };
    let env = both("env");
    if !env.is_empty() { out["environmentVariables"] = env.into() }
    let defaults = both("defaults");
    if !defaults.is_empty() { out["defaults"] = defaults.into() }
    let mut seen = vec![];
    let steps = found.get("steps").map(Node::items).unwrap_or_default();
    if steps.is_empty() { return Err(format!("the job {job} in {path} has no steps (a job that calls another workflow is not run here yet)")) }
    out["steps"] = steps.iter().map(|s| step(s, file, &mut seen, ids())).collect::<Result<Vec<_>>>()?.into();
    for (key, field) in [("outputs", "jobOutputs"), ("services", "jobServiceContainers"), ("container", "jobContainer")] {
        if let Some(n) = found.get(key).filter(|n| !n.empty()) { out[field] = token(n, file) }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_with_expressions_is_one_expression() {
        let t = |s: &str| text_token(s, json!({}));
        assert_eq!(t("plain"), json!({ "type": 0, "lit": "plain" }));
        assert_eq!(t("${{ github.event_name }}"), json!({ "type": 3, "expr": "github.event_name" }));
        assert_eq!(t("before ${{ vars.A }} it's {braces} ${{ secrets.B }} after"), json!({ "type": 3, "expr": "format('before {0} it''s {{braces}} {1} after', vars.A, secrets.B)" }));
        // A `}}` inside a quoted string does not end the expression; one never closed is text.
        assert_eq!(t("${{ format('{0}}}', 1) }}"), json!({ "type": 3, "expr": "format('{0}}}', 1)" }));
        assert_eq!(t("${{ open"), json!({ "type": 0, "lit": "${{ open" }));
    }

    #[test]
    fn a_step_runs_when_all_went_well_unless_it_asks_how_the_job_is_going() {
        let c = |s: &str| condition(&parse(&format!("if: {s}")).unwrap());
        assert_eq!(condition(&parse("run: x").unwrap()), "success()");
        assert_eq!((c("always()"), c("${{ failure() || true }}"), c("github.ref == 'refs/heads/main'")), ("always()".to_string(), "failure() || true".to_string(), "success() && (github.ref == 'refs/heads/main')".to_string()));
        // (A name that only ends like one of them is no such question.)
        assert_eq!(c("steps.x.outputs.success == 'yes'"), "success() && (steps.x.outputs.success == 'yes')");
    }
}
