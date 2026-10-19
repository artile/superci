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
    block: bool,
    /// Anchors (`&name`): each value by its id, once read; and the id of each sequence and mapping being read.
    anchors: std::collections::HashMap<usize, Node>, anchored: Vec<usize> }

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
                    // (A merge key, `<<: *defaults`, is YAML 1.1's: GitHub Actions does not read it, nor is it read here.)
                    None if matches!(&node, Node::Scalar { text, plain: true, .. } if text == "<<") => {
                        let (line, _) = node.place();
                        self.error.get_or_insert(format!("line {line}: merge keys (<<) are not supported in GitHub Actions workflows (anchors and aliases are)"));
                        keys.push(node)
                    }
                    None => keys.push(node),
                }
            }
            Some(Node::Scalar { .. }) => {}
        }
    }
}

impl Build {
    /// A sequence or mapping read to its end: kept by its anchor, if it has one, then put where it stands.
    fn end(&mut self, node: Node) {
        if let Some(anchor) = self.anchored.pop().filter(|a| *a > 0) { self.anchors.insert(anchor, node.clone()); }
        self.put(node)
    }
}

impl MarkedEventReceiver for Build {
    fn on_event(&mut self, event: Event, mark: Marker) {
        // (Lines from 1 and columns from 1, as GitHub counts them.)
        let (line, col) = (mark.line(), mark.col() + 1);
        match event {
            Event::Scalar(text, style, anchor, ..) => {
                self.block = matches!(style, TScalarStyle::Literal | TScalarStyle::Folded);
                let node = Node::Scalar { text, plain: style == TScalarStyle::Plain, line, col };
                if anchor > 0 { self.anchors.insert(anchor, node.clone()); }
                self.put(node);
                self.block = false;
            }
            Event::SequenceStart(anchor, ..) => { self.stack.push(Node::Seq { items: vec![], line, col }); self.anchored.push(anchor) }
            Event::MappingStart(anchor, ..) => { self.stack.push(Node::Map { pairs: vec![], line, col }); self.keys.push(vec![]); self.anchored.push(anchor) }
            // (The parser ends only what it began.)
            Event::SequenceEnd => { let n = self.stack.pop().expect("a sequence begun"); self.end(n) }
            Event::MappingEnd => { self.keys.pop(); let n = self.stack.pop().expect("a mapping begun"); self.end(n) }
            // An alias (`*name`): the value its anchor named, as if written here (as GitHub Actions reads it).
            Event::Alias(anchor) => match self.anchors.get(&anchor).cloned() {
                Some(node) => self.put(node),
                None => { self.error.get_or_insert(format!("line {line}: an alias names no anchor before it")); }
            },
            _ => {}
        }
    }
}

/// Reads a workflow file.
pub fn parse(yaml: &str) -> Result<Node> {
    let mut build = Build { stack: vec![], keys: vec![], done: None, error: None, block: false, anchors: Default::default(), anchored: vec![] };
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
fn context_name(base: &str, seen: &mut Vec<String>) -> String {
    let base: String = format!("__{}", base.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect::<String>());
    let n = seen.iter().filter(|s| **s == base).count();
    seen.push(base.clone());
    if n == 0 { base } else { format!("{base}_{}", n + 1) }
}

/// One step as the runner is given it. `named`: what it is called when it has no `id` of its own, when that is not
/// counted with the others (a step of a parallel group).
fn step(node: &Node, file: u32, seen: &mut Vec<String>, id: String, named: Option<String>) -> Result<Value> {
    let (line, _) = node.place();
    let own_id = node.get("id").and_then(Node::text).map(str::to_string);
    let mut out = json!({ "type": "action" });
    let base = if let Some(run) = node.get("run") {
        out["reference"] = json!({ "type": "script" });
        let mut inputs = vec![json!({ "key": "script", "value": token(run, file) })];
        if let Some(d) = node.get("working-directory") { inputs.push(json!({ "key": "workingDirectory", "value": token(d, file) })) }
        if let Some(s) = node.get("shell") { inputs.push(json!({ "key": "shell", "value": token(s, file) })) }
        out["inputs"] = json!({ "type": 2, "map": inputs });
        "run".to_string()
    } else if let Some(uses) = node.get("uses").and_then(Node::text) {
        if let Some(image) = uses.strip_prefix("docker://") {
            out["reference"] = json!({ "type": "containerRegistry", "image": image });
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            image.to_string()
        } else if uses.starts_with("./") || uses.starts_with(".\\") {
            out["reference"] = json!({ "type": "repository", "path": uses, "repositoryType": "self" });
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            "self".to_string()
        } else {
            let (what, reference) = uses.rsplit_once('@').ok_or_else(|| format!("line {line}: `uses: {uses}` names no version (owner/name@version)"))?;
            let mut names = what.splitn(3, '/');
            let (owner, repo, path) = (names.next().unwrap_or_default(), names.next().ok_or_else(|| format!("line {line}: `uses: {uses}` is not owner/name@version"))?, names.next().unwrap_or_default());
            out["reference"] = json!({ "type": "repository", "name": format!("{owner}/{repo}"), "ref": reference, "repositoryType": "GitHub" });
            // (An action in a folder of its repository. GitHub leaves the field out for one at the top.)
            if !path.is_empty() { out["reference"]["path"] = path.into() }
            if let Some(with) = node.get("with").filter(|w| !w.empty()) { out["inputs"] = token(with, file) }
            format!("{owner}/{repo}")
        }
    } else {
        return Err(format!("line {line}: a step has `run`, `uses`, `parallel`, `wait`, `wait-all` or `cancel`"))
    };
    // (A step with an id of its own is called that, and is not counted among the others.)
    let name = own_id.or(named).unwrap_or_else(|| context_name(&base, seen));
    if let Some(n) = node.get("name") { out["displayNameToken"] = token(n, file) }
    out["contextName"] = name.clone().into();
    if let Some(env) = node.get("env").filter(|e| !e.empty()) { out["environment"] = token(env, file) }
    // (Run while the next steps run: `true` or `false` as written, not an expression, as GitHub reads it.)
    if let Some(b) = node.get("background") {
        match b.text() {
            Some("true") => out["background"] = true.into(),
            Some("false") => {}
            _ => return Err(format!("line {}: `background` is true or false", b.place().0)),
        }
    }
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

/// A step that waits for steps run in the background (`wait`, `wait-all`), or ends one (`cancel`), as the runner is
/// given it. `known`: what the steps before it are called.
fn control(node: &Node, file: u32, known: &[String], id: String) -> Result<Option<Value>> {
    let Some((key, value)) = ["wait", "wait-all", "cancel"].into_iter().find_map(|k| node.get(k).map(|v| (k, v))) else { return Ok(None) };
    let (line, _) = node.place();
    if node.get("run").is_some() || node.get("uses").is_some() { return Err(format!("line {line}: a step that has `{key}` runs nothing itself")) }
    if node.get("if").is_some() { return Err(format!("line {line}: a `{key}` step always runs; it takes no `if`")) }
    let ids: Vec<String> = match (key, value) {
        ("wait-all", _) => vec![],
        ("wait", Node::Seq { items, .. }) => items.iter().map(|i| i.text().unwrap_or_default().to_string()).collect(),
        (_, v) => vec![v.text().filter(|t| !t.is_empty()).ok_or_else(|| format!("line {line}: `{key}` names the id of a step"))?.to_string()],
    };
    if let Some(missing) = ids.iter().find(|i| !known.contains(i)) { return Err(format!("line {line}: `{key}: {missing}` names no step before it")) }
    let fallback = match key { "wait" => "Wait", "wait-all" => "Wait for all", _ => "Cancel" };
    let mut out = json!({ "type": "backgroundStepControl", "controlType": key });
    if !ids.is_empty() { out["stepIds"] = ids.into() }
    let named = node.get("name");
    out["displayNameToken"] = named.map(|n| token(n, file)).unwrap_or(fallback.into());
    out["condition"] = "".into();
    out["id"] = id.into();
    out["name"] = named.and_then(Node::text).unwrap_or(fallback).into();
    Ok(Some(out))
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
    let (mut written, mut known, mut groups) = (vec![], vec![], 0);
    for s in steps {
        if let Some(c) = control(s, file, &known, ids())? { written.push(c); continue }
        let Some(group) = s.get("parallel") else {
            let one = step(s, file, &mut seen, ids(), None)?;
            known.push(one["contextName"].as_str().unwrap_or_default().to_string());
            written.push(one);
            continue
        };
        // A parallel group: each of its steps run in the background, then waited for together.
        let (line, _) = s.place();
        if s.pairs().len() > 1 { return Err(format!("line {line}: a `parallel` group has only its steps")) }
        let group_id = format!("__parallel_{groups}");
        groups += 1;
        let mut names = vec![];
        for (i, inner) in group.items().iter().enumerate() {
            let (at, _) = inner.place();
            if ["parallel", "wait", "wait-all", "cancel"].iter().any(|k| inner.get(k).is_some()) { return Err(format!("line {at}: a step of a `parallel` group runs something")) }
            let mut one = step(inner, file, &mut seen, ids(), Some(format!("{group_id}_{i}")))?;
            one["background"] = true.into();
            one["parallelGroupId"] = group_id.clone().into();
            let name = one["contextName"].as_str().unwrap_or_default().to_string();
            known.push(name.clone());
            names.push(name);
            written.push(one);
        }
        if names.is_empty() { return Err(format!("line {line}: a `parallel` group has steps")) }
        written.push(json!({ "type": "backgroundStepControl", "controlType": "wait", "stepIds": names, "displayNameToken": "Parallel group", "condition": "", "parallelGroupId": group_id, "id": ids(), "name": "Parallel group" }));
    }
    out["steps"] = written.into();
    for (key, field) in [("outputs", "jobOutputs"), ("services", "jobServiceContainers"), ("container", "jobContainer")] {
        if let Some(n) = found.get(key).filter(|n| !n.empty()) { out[field] = token(n, file) }
    }
    Ok(out)
}

/// A value of a matrix as GitHub reads it: a number, `true`, `null` as such, else text.
/// What reads an expression of a matrix (a scalar written with `${{ }}`): its value, once the jobs the job needs have
/// ended (none: the matrix is not read yet).
pub type Reader<'a> = Option<&'a dyn Fn(&str) -> std::result::Result<Value, String>>;

fn matrix_value(n: &Node, read: Reader) -> std::result::Result<Value, String> {
    match n {
        Node::Scalar { text, plain, line, .. } => {
            if text.contains("${{") { return read.ok_or_else(|| format!("line {line}: a matrix made by an expression is read once the jobs it needs have ended"))?(text) }
            if !*plain { return Ok(Value::String(text.clone())) }
            Ok(match text.as_str() { "true" => json!(true), "false" => json!(false), "null" | "~" | "" => Value::Null, t => t.parse::<f64>().map(|f| json!(f)).unwrap_or_else(|_| json!(t)) })
        }
        Node::Seq { items, .. } => items.iter().map(|i| matrix_value(i, read)).collect::<std::result::Result<Vec<_>, _>>().map(Value::Array),
        Node::Map { pairs, .. } => pairs.iter().map(|(k, v)| Ok((k.text().unwrap_or_default().to_string(), matrix_value(v, read)?))).collect::<std::result::Result<serde_json::Map<_, _>, String>>().map(Value::Object),
    }
}

/// Whether a job's matrix is made, in whole or in part, by an expression (`${{ fromJSON(needs.a.outputs.m) }}`): it
/// is read once the jobs it needs have ended (`matrix_with`).
pub fn matrix_later(job: &Node) -> bool {
    fn has(n: &Node) -> bool { match n { Node::Scalar { text, .. } => text.contains("${{"), Node::Seq { items, .. } => items.iter().any(has), Node::Map { pairs, .. } => pairs.iter().any(|(_, v)| has(v)) } }
    job.get("strategy").and_then(|s| s.get("matrix")).is_some_and(has)
}

/// A job's matrix: every combination its `strategy.matrix` makes, with `exclude` taken out and `include` added as
/// GitHub adds it (to each combination it does not change, else as one of its own). None: the job has no matrix.
pub fn matrix(job: &Node) -> std::result::Result<Option<Vec<serde_json::Map<String, Value>>>, String> { matrix_with(job, None) }

/// The same, its expressions read by `read`.
pub fn matrix_with(job: &Node, read: Reader) -> std::result::Result<Option<Vec<serde_json::Map<String, Value>>>, String> {
    let Some(node) = job.get("strategy").and_then(|s| s.get("matrix")) else { return Ok(None) };
    let Value::Object(m) = matrix_value(node, read)? else { return Err("the matrix is not a mapping".into()) };
    // (In the order its keys are written, as GitHub makes the combinations.)
    let order: Vec<String> = match node { Node::Map { pairs, .. } => pairs.iter().filter_map(|(k, _)| k.text().map(str::to_string)).collect(), _ => m.keys().cloned().collect() };
    let mut combos: Vec<serde_json::Map<String, Value>> = vec![serde_json::Map::new()];
    let mut keys = vec![];
    for key in &order {
        let Some(v) = m.get(key) else { continue };
        if key == "include" || key == "exclude" { continue }
        let Value::Array(values) = v else { return Err(format!("the matrix's {key} is not a list")) };
        keys.push(key.clone());
        combos = combos.into_iter().flat_map(|c| { let key = key.clone(); values.iter().map(move |v| { let mut c = c.clone(); c.insert(key.clone(), v.clone()); c }) }).collect();
    }
    if keys.is_empty() { combos.clear() }
    let entries = |name: &str| -> std::result::Result<Vec<serde_json::Map<String, Value>>, String> {
        match m.get(name) { Some(Value::Array(a)) => Ok(a.iter().filter_map(|v| v.as_object().cloned()).collect()), Some(Value::Null) | None => Ok(vec![]), Some(_) => Err(format!("the matrix's {name} is not a list")) }
    };
    for ex in entries("exclude")? { combos.retain(|c| !ex.iter().all(|(k, v)| c.get(k) == Some(v))) }
    let original = combos.len();
    for inc in entries("include")? {
        let mut matched = false;
        for c in combos.iter_mut().take(original) {
            if inc.iter().filter(|(k, _)| keys.contains(k)).all(|(k, v)| c.get(k) == Some(v)) { for (k, v) in &inc { if !keys.contains(k) { c.insert(k.clone(), v.clone()); } } matched = true }
        }
        if !matched { combos.push(inc) }
    }
    Ok(Some(combos))
}

/// Whether a workflow may be called by another (`on: workflow_call`, in any of its forms).
pub fn callable(workflow: &Node) -> bool {
    match workflow.get("on").or_else(|| workflow.get("true")) {
        Some(Node::Scalar { text, .. }) => text == "workflow_call",
        Some(Node::Seq { items, .. }) => items.iter().any(|i| i.text() == Some("workflow_call")),
        Some(m @ Node::Map { .. }) => m.get("workflow_call").is_some(),
        None => false,
    }
}

/// A workflow's `on.workflow_dispatch` inputs as GitHub gives them to a manual start with `given` (NAME, VALUE): typed
/// for `inputs` (a boolean true or false, a number a number), and as text for `github.event.inputs`. Each one not
/// given takes its default. A workflow not started by hand, an input it does not have, one required and not given, a
/// boolean not true or false, a number not a number, a choice not among its options: refused.
pub fn dispatch_inputs(workflow: &Node, given: &[(String, String)]) -> Result<(serde_json::Value, serde_json::Value)> {
    let on = workflow.get("on").or_else(|| workflow.get("true"));
    let started = match on {
        Some(Node::Scalar { text, .. }) => text == "workflow_dispatch",
        Some(Node::Seq { items, .. }) => items.iter().any(|i| i.text() == Some("workflow_dispatch")),
        Some(m @ Node::Map { .. }) => m.get("workflow_dispatch").is_some(),
        None => false,
    };
    if !started { return Err("the workflow has no `on: workflow_dispatch`: GitHub would not start it by hand".into()) }
    let defs = on.and_then(|o| o.get("workflow_dispatch")).and_then(|d| d.get("inputs"));
    let names: Vec<String> = defs.map(|d| d.pairs().iter().filter_map(|(k, _)| k.text().map(str::to_string)).collect()).unwrap_or_default();
    if let Some((n, _)) = given.iter().find(|(n, _)| !names.contains(n)) { return Err(format!("the workflow has no input {n} ({})", if names.is_empty() { "it has none".to_string() } else { format!("its inputs: {}", names.join(", ")) })) }
    let (mut typed, mut text) = (serde_json::Map::new(), serde_json::Map::new());
    for name in &names {
        let d = defs.and_then(|d| d.get(name));
        let field = |k: &str| d.and_then(|d| d.get(k)).and_then(|v| v.text()).map(str::to_string);
        let kind = field("type").unwrap_or_else(|| "string".into());
        let value = match given.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone()).or_else(|| field("default")) {
            Some(v) => v,
            None if field("required").as_deref() == Some("true") => return Err(format!("the workflow's input {name} is required: --input={name}=VALUE")),
            None if kind == "boolean" => "false".into(),
            None => String::new(),
        };
        let t = match kind.as_str() {
            "boolean" => match value.as_str() { "true" => true.into(), "false" => false.into(), _ => return Err(format!("the input {name} is true or false, not “{value}”")) },
            "number" => match value.trim().parse::<f64>() { Ok(f) => serde_json::json!(f), Err(_) if value.is_empty() => value.clone().into(), Err(_) => return Err(format!("the input {name} is a number, not “{value}”")) },
            "choice" => {
                let options: Vec<String> = d.and_then(|d| d.get("options")).map(|o| o.items().iter().filter_map(|i| i.text().map(str::to_string)).collect()).unwrap_or_default();
                if !value.is_empty() && !options.contains(&value) { return Err(format!("the input {name} is one of {}, not “{value}”", options.join(", "))) }
                value.clone().into()
            }
            _ => value.clone().into(),
        };
        typed.insert(name.clone(), t);
        text.insert(name.clone(), value.into());
    }
    Ok((typed.into(), text.into()))
}

/// A called workflow's `on.workflow_call` part (its inputs, secrets and outputs), when written as a mapping.
pub fn call_part(workflow: &Node) -> Option<&Node> { workflow.get("on").or_else(|| workflow.get("true")).and_then(|o| o.get("workflow_call")) }

/// The jobs a job needs, by name.
pub fn needs(job: &Node) -> Vec<String> {
    match job.get("needs") { Some(Node::Scalar { text, .. }) => vec![text.clone()], Some(n) => n.items().iter().filter_map(|i| i.text().map(str::to_string)).collect(), None => vec![] }
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
    fn steps_run_in_the_background_are_refused_where_github_refuses_them() {
        // Each refused by GitHub itself (a probe of each, on GitHub), and accepted where it accepts them.
        let read = |steps: &str| job_parts(&parse(&format!("jobs:\n  test:\n    runs-on: x\n    steps:\n{steps}")).unwrap(), "ci.yml", "test", &mut String::new);
        for (steps, said) in [
            ("      - run: a\n        background: ${{ true }}\n", "line 6: `background` is true or false"),
            ("      - id: a\n        run: a\n        background: true\n      - wait: a\n        if: always()\n", "line 8: a `wait` step always runs; it takes no `if`"),
            ("      - parallel:\n          - parallel:\n              - run: a\n", "line 6: a step of a `parallel` group runs something"),
            ("      - wait: nowhere\n", "line 5: `wait: nowhere` names no step before it"),
            ("      - wait: a\n      - id: a\n        run: a\n        background: true\n", "line 5: `wait: a` names no step before it"),
            ("      - id: a\n        run: a\n        background: true\n      - parallel:\n          - wait: a\n", "line 9: a step of a `parallel` group runs something"),
            ("      - name: Named\n        parallel:\n          - run: a\n", "line 5: a `parallel` group has only its steps"),
            ("      - id: g\n        parallel:\n          - run: a\n", "line 5: a `parallel` group has only its steps"),
            ("      - parallel: []\n", "line 5: a `parallel` group has steps"),
            ("      - id: a\n        run: a\n        background: true\n      - cancel: [a]\n", "line 8: `cancel` names the id of a step"),
            ("      - name: Nothing\n", "line 5: a step has `run`, `uses`, `parallel`, `wait`, `wait-all` or `cancel`"),
        ] {
            assert_eq!(read(steps).err().as_deref(), Some(said), "{steps}");
        }
        // Waiting for a step not run in the background, `background: false`, and an `id` on a wait (not kept).
        let ok = read("      - id: a\n        run: a\n        background: false\n      - wait: a\n        id: w\n      - wait-all:\n").unwrap();
        assert_eq!(ok["steps"][0].get("background"), None);
        assert_eq!((ok["steps"][1]["stepIds"].clone(), ok["steps"][1].get("contextName"), ok["steps"][2]["controlType"].as_str(), ok["steps"][2].get("stepIds")), (json!(["a"]), None, Some("wait-all"), None));
    }

    #[test]
    fn anchors_and_aliases_are_read_as_github_reads_them_and_merge_keys_are_not() {
        // A job's steps written once and named again, a value and a mapping too.
        let yaml = "jobs:\n  a:\n    runs-on: &machine superci/cpu=16\n    env: &env\n      NODE_ENV: test\n    steps: &steps\n      - run: npm ci\n      - run: npm test\n  b:\n    runs-on: *machine\n    env: *env\n    steps: *steps\n";
        let w = parse(yaml).unwrap();
        let (a, b) = (w.get("jobs").unwrap().get("a").unwrap(), w.get("jobs").unwrap().get("b").unwrap());
        assert_eq!(b.get("runs-on").unwrap().text(), Some("superci/cpu=16"));
        assert_eq!(b.get("env").unwrap().get("NODE_ENV").unwrap().text(), Some("test"));
        assert_eq!(b.get("steps").unwrap().items().len(), 2);
        assert_eq!(b.get("steps").unwrap().items()[1].get("run").unwrap().text(), a.get("steps").unwrap().items()[1].get("run").unwrap().text());
        // As the runner is handed them: the same steps in both jobs.
        let mut n = 0;
        let mut ids = || { n += 1; format!("id{n}") };
        let (pa, pb) = (job_parts(&w, ".github/workflows/ci.yml", "a", &mut ids).unwrap(), job_parts(&w, ".github/workflows/ci.yml", "b", &mut ids).unwrap());
        assert_eq!(pa["steps"].as_array().map(Vec::len), pb["steps"].as_array().map(Vec::len));
        // A merge key is YAML 1.1's: GitHub Actions does not read it, nor does this.
        let merged = parse("defaults: &d\n  shell: bash\njobs:\n  a:\n    <<: *d\n    runs-on: x\n");
        assert!(merged.as_ref().is_err_and(|e| e.contains("line 5: merge keys (<<) are not supported")), "{merged:?}");
        // An alias of an anchor not written before it is no workflow.
        assert!(parse("a: *nowhere\nb: &nowhere 1\n").is_err());
    }

    #[test]
    fn a_matrix_makes_its_combinations_as_github_makes_them() {
        let job = parse("strategy:\n  matrix:\n    node: [20, 22]\n    os: [linux, mac]\n    exclude:\n      - node: 20\n        os: mac\n    include:\n      - node: 22\n        extra: yes\n      - node: 24\n        os: linux\nneeds: [a, b]\n").unwrap();
        let m = matrix(&job).unwrap().unwrap();
        let shown: Vec<String> = m.iter().map(|c| serde_json::to_string(c).unwrap()).collect();
        assert_eq!(shown, [r#"{"node":20.0,"os":"linux"}"#, r#"{"extra":"yes","node":22.0,"os":"linux"}"#, r#"{"extra":"yes","node":22.0,"os":"mac"}"#, r#"{"node":24.0,"os":"linux"}"#]);
        assert_eq!(needs(&job), ["a", "b"]);
        assert!(matrix(&parse("runs-on: x").unwrap()).unwrap().is_none());
        let later = parse("strategy:\n  matrix: ${{ fromJSON(needs.a.outputs.m) }}").unwrap();
        assert!(matrix(&later).is_err() && matrix_later(&later));
        // Read once the jobs it needs have ended: what the expression gives, made into combinations as any matrix.
        let read = |_: &str| Ok(serde_json::json!({ "os": ["linux", "mac"], "include": [{ "os": "linux", "big": true }] }));
        let m = matrix_with(&later, Some(&read)).unwrap().unwrap();
        assert_eq!(serde_json::to_string(&m).unwrap(), r#"[{"big":true,"os":"linux"},{"os":"mac"}]"#);
    }

    #[test]
    fn a_step_runs_when_all_went_well_unless_it_asks_how_the_job_is_going() {
        let c = |s: &str| condition(&parse(&format!("if: {s}")).unwrap());
        assert_eq!(condition(&parse("run: x").unwrap()), "success()");
        assert_eq!((c("always()"), c("${{ failure() || true }}"), c("github.ref == 'refs/heads/main'")), ("always()".to_string(), "failure() || true".to_string(), "success() && (github.ref == 'refs/heads/main')".to_string()));
        // (A name that only ends like one of them is no such question.)
        assert_eq!(c("steps.x.outputs.success == 'yes'"), "success() && (steps.x.outputs.success == 'yes')");
    }

    #[test]
    fn a_manual_starts_inputs_are_its_defaults_and_what_is_given_typed_as_their_kinds() {
        let wf = parse("on:\n  workflow_dispatch:\n    inputs:\n      record:\n        type: boolean\n        default: false\n      shards:\n        type: number\n        default: 2\n      browser:\n        type: choice\n        options: [chromium, webkit]\n        default: chromium\n      note:\n        required: true\njobs:\n  t:\n    runs-on: x\n    steps: [{run: x}]\n").unwrap();
        let given = |p: &[(&str, &str)]| p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        let (typed, text) = dispatch_inputs(&wf, &given(&[("note", "hi"), ("record", "true")])).unwrap();
        assert_eq!(typed, serde_json::json!({ "record": true, "shards": 2.0, "browser": "chromium", "note": "hi" }));
        assert_eq!(text, serde_json::json!({ "record": "true", "shards": "2", "browser": "chromium", "note": "hi" }));
        assert!(dispatch_inputs(&wf, &[]).unwrap_err().contains("input note is required: --input=note=VALUE"));
        assert!(dispatch_inputs(&wf, &given(&[("note", "x"), ("record", "yes")])).unwrap_err().contains("true or false, not “yes”"));
        assert!(dispatch_inputs(&wf, &given(&[("note", "x"), ("shards", "many")])).unwrap_err().contains("a number, not “many”"));
        assert!(dispatch_inputs(&wf, &given(&[("note", "x"), ("browser", "edge")])).unwrap_err().contains("one of chromium, webkit, not “edge”"));
        assert!(dispatch_inputs(&wf, &given(&[("nope", "x")])).unwrap_err().contains("no input nope (its inputs: record, shards, browser, note)"));
        let push = parse("on: push\njobs:\n  t:\n    runs-on: x\n    steps: [{run: x}]\n").unwrap();
        assert!(dispatch_inputs(&push, &[]).unwrap_err().contains("no `on: workflow_dispatch`"));
        let bare = parse("on: [push, workflow_dispatch]\njobs:\n  t:\n    runs-on: x\n    steps: [{run: x}]\n").unwrap();
        assert_eq!(dispatch_inputs(&bare, &[]).unwrap(), (serde_json::json!({}), serde_json::json!({})));
    }
}
