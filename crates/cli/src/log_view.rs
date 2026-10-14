//! A job's log as the dashboard shows it: in steps, as GitHub shows a job's, each with how it went, how long it took
//! and its lines (their colours kept, a group's heading marked, errors and warnings told apart). From what the control
//! plane gives (`/job/log` with `steps`): GitHub's steps with their times and its log once the job has ended (each
//! line goes to the step it was written in, by its time); a run from a computer's steps, each with its log; GitLab's
//! log, whose sections are its steps.
use serde_json::{json, Value};

/// One line as the page draws it: its time (hh:mm:ss, when the log says), its kind (`g` a group's heading, `e` an
/// error, `w` a warning, `n` a notice, `c` a command, else nothing) and its text as HTML.
type Line = (String, &'static str, String);

struct Step { name: String, state: String, ms: Option<u64>, lines: Vec<Line> }

/// The page's view of a job's log: whether it is still going, its steps, and how many errors and warnings it has.
pub fn view(job: &Value, got: &Value, going: bool) -> Value {
    let log = got["log"].as_str().unwrap_or_default();
    let steps = match job["provider"].as_str().unwrap_or_default() {
        "local" => local(got),
        "gitlab" => gitlab(log, going, job["conclusion"].as_str().unwrap_or_default()),
        _ => github(got, log),
    };
    let count = |k: &str| steps.iter().map(|s| s.lines.iter().filter(|l| l.1 == k).count()).sum::<usize>();
    json!({ "going": going, "truncated": got["truncated"], "error": got["error"], "errors": count("e"), "warnings": count("w"),
        "steps": steps.iter().map(|s| json!({ "name": s.name, "state": s.state, "ms": s.ms, "lines": s.lines.iter().map(|(t, k, h)| json!([t, k, h])).collect::<Vec<_>>() })).collect::<Vec<_>>() })
}

/// A run from a computer: its steps as its runner said them, each with its log.
fn local(got: &Value) -> Vec<Step> {
    got["steps"].as_array().into_iter().flatten().map(|s| {
        let state = match (s["state"].as_str(), s["result"].as_str()) {
            (_, Some("succeeded")) => "success", (_, Some("failed")) => "failure", (_, Some("skipped")) => "skipped", (_, Some("cancelled" | "canceled")) => "cancelled",
            (Some("inProgress"), _) => "running", (Some("completed"), _) => "success", _ => "pending",
        };
        let ms = match (s["start"].as_str().and_then(seconds), s["finish"].as_str().and_then(seconds)) { (Some(a), Some(b)) => Some(b.saturating_sub(a) * 1000), _ => None };
        Step { name: s["name"].as_str().unwrap_or("Step").to_string(), state: state.into(), ms, lines: s["text"].as_str().unwrap_or_default().lines().map(line).collect() }
    }).collect()
}

/// A job of GitHub's: its steps as GitHub has them, and each line of its log (once it has ended) in the step that was
/// going when it was written. A step begins at its time: a line of that very second goes to it only from the step's
/// heading on (GitHub's times are whole seconds).
fn github(got: &Value, log: &str) -> Vec<Step> {
    let given = got["steps"].as_array().cloned().unwrap_or_default();
    let secs = |v: &Value| v.as_str().and_then(seconds);
    let mut steps: Vec<(Step, Option<u64>)> = given.iter().map(|s| {
        let state = match (s["status"].as_str(), s["conclusion"].as_str()) {
            (Some("completed"), Some(c)) => match c { "success" => "success", "skipped" => "skipped", "cancelled" => "cancelled", _ => "failure" },
            (Some("in_progress"), _) => "running", _ => "pending",
        };
        let ms = match (secs(&s["started_at"]), secs(&s["completed_at"])) { (Some(a), Some(b)) => Some(b.saturating_sub(a) * 1000), _ => None };
        (Step { name: s["name"].as_str().unwrap_or("Step").to_string(), state: state.into(), ms, lines: vec![] }, secs(&s["started_at"]))
    }).collect();
    if steps.is_empty() {
        return if log.trim().is_empty() { vec![] } else { vec![Step { name: "Log".into(), state: "success".into(), ms: None, lines: log.lines().map(line).collect() }] }
    }
    let mut at = 0;
    for raw in log.lines() {
        let when = raw.trim_start_matches('\u{feff}').get(..19).and_then(|t| seconds(&format!("{t}Z")));
        let l = line(raw);
        // On to a later step: its second has come and this is how a step begins (its heading, or the first line GitHub
        // writes for the steps of its own), or its second has passed.
        let begins = l.1 == "g" || ["Current runner version", "Post job cleanup.", "Cleaning up orphan processes"].iter().any(|b| l.2.starts_with(b));
        while let (Some(t), Some((_, Some(next)))) = (when, steps.get(at + 1)) {
            if t > *next || (t == *next && begins) { at += 1 } else { break }
        }
        steps[at].0.lines.push(l);
    }
    steps.into_iter().map(|(s, _)| s).collect()
}

/// A job of GitLab's: its sections are its steps (what comes before the first, and after the last, are steps of their
/// own); a section within one is a group in it.
fn gitlab(log: &str, going: bool, conclusion: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = vec![Step { name: "Started".into(), state: "success".into(), ms: None, lines: vec![] }];
    let (mut depth, mut began, mut untitled) = (0usize, 0u64, false);
    for raw in log.lines() {
        let text = gitlab_text(raw);
        // A section's heading on the line after its start: its name.
        if untitled && !text.contains("section_") {
            let heading = strip_controls(text);
            if !heading.trim().is_empty() { steps.last_mut().unwrap().name = heading.trim().to_string(); untitled = false; continue }
        }
        if let Some(i) = text.find("section_start:") {
            let rest = &text[i + "section_start:".len()..];
            let (ts, rest) = rest.split_once(':').unwrap_or(("0", rest));
            let header = rest.split_once('\r').map(|(_, h)| h).unwrap_or("");
            let header = strip_controls(header);
            if depth == 0 {
                began = ts.parse().unwrap_or(0);
                untitled = header.trim().is_empty();
                let name = if untitled { rest.split(['[', '\r']).next().unwrap_or("Section").replace('_', " ") } else { header.trim().to_string() };
                steps.push(Step { name, state: if going { "running".into() } else { "success".into() }, ms: None, lines: vec![] });
            } else {
                steps.last_mut().unwrap().lines.push((String::new(), "g", ansi_html(&header)));
            }
            depth += 1;
            continue;
        }
        if let Some(i) = text.find("section_end:") {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                let ended: u64 = text[i + "section_end:".len()..].split(':').next().and_then(|t| t.parse().ok()).unwrap_or(began);
                let s = steps.last_mut().unwrap();
                (s.ms, s.state) = (Some(ended.saturating_sub(began) * 1000), "success".into());
                steps.push(Step { name: "Ended".into(), state: "success".into(), ms: None, lines: vec![] });
            }
            continue;
        }
        steps.last_mut().unwrap().lines.push(line(raw));
    }
    // A step that never ended: still going, or where the job failed.
    let failed = !going && conclusion == "failure";
    if failed {
        if let Some(s) = steps.iter_mut().rev().find(|s| s.ms.is_some() || s.state == "running") { s.state = "failure".into() }
        if let Some(last) = steps.iter_mut().rev().find(|s| !s.lines.is_empty()) { last.state = "failure".into() }
    }
    if !going { for s in steps.iter_mut().filter(|s| s.state == "running") { s.state = if failed { "failure".into() } else { "success".into() } } }
    steps.retain(|s| !s.lines.is_empty() || s.ms.is_some());
    steps
}

/// A GitLab log line's text: without the time and stream GitLab's runner puts before it ("2026-…Z 00O+ ").
fn gitlab_text(raw: &str) -> &str {
    let (_, rest) = split_time(raw);
    let b = rest.as_bytes();
    if b.len() >= 4 && b[..2].iter().all(u8::is_ascii_digit) && (b[2] == b'O' || b[2] == b'E') {
        let skip = if b.get(3) == Some(&b'+') { 4 } else { 3 };
        return rest[skip..].strip_prefix(' ').unwrap_or(&rest[skip..])
    }
    rest
}

/// A line's time (when it begins with one, as GitHub's and GitLab's runners write) and the rest.
fn split_time(raw: &str) -> (Option<&str>, &str) {
    let r = raw.trim_start_matches('\u{feff}');
    let b = r.as_bytes();
    if b.len() > 20 && b[4] == b'-' && b[10] == b'T' && b[..4].iter().all(u8::is_ascii_digit) {
        if let Some(at) = r.find("Z ").filter(|at| *at < 40) { return (r.get(11..19), &r[at + 2..]) }
    }
    (None, r)
}

/// One line for the page.
fn line(raw: &str) -> Line {
    let (time, rest) = split_time(raw);
    let rest = gitlab_text(rest);
    let time = time.unwrap_or_default().to_string();
    for (mark, kind) in [("##[group]", "g"), ("##[error]", "e"), ("##[warning]", "w"), ("##[notice]", "n"), ("[command]", "c")] {
        if let Some(text) = rest.strip_prefix(mark) { return (time, kind, ansi_html(text)) }
    }
    if rest.starts_with("##[endgroup]") { return (time, "x", String::new()) }
    let kind = if rest.starts_with("ERROR: ") { "e" } else if rest.starts_with("WARNING: ") { "w" } else { "" };
    (time, kind, ansi_html(rest))
}

/// Seconds since the epoch of a time GitHub writes ("2026-10-14T21:09:53Z", fractions allowed).
fn seconds(t: &str) -> Option<u64> {
    let b = t.as_bytes();
    if b.len() < 19 { return None }
    let n = |r: std::ops::Range<usize>| t.get(r).and_then(|s| s.parse::<i64>().ok());
    let (y, mo, d, h, mi, s) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y2, m2) = if mo <= 2 { (y - 1, mo + 9) } else { (y, mo - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

/// Control characters a line's text can carry that are not colours (GitLab's "erase line", carriage returns).
fn strip_controls(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') { chars.next(); for n in chars.by_ref() { if n.is_ascii_alphabetic() { break } } continue }
        if c != '\r' { out.push(c) }
    }
    out
}

/// A line's text as HTML: escaped, its colours (SGR: bold, the 16 colours) as spans, every other control left out.
pub fn ansi_html(s: &str) -> String {
    let (mut out, mut open) = (String::new(), false);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            let mut end = ' ';
            for n in chars.by_ref() { if n.is_ascii_alphabetic() { end = n; break } code.push(n) }
            if end != 'm' { continue }
            if open { out.push_str("</span>"); open = false }
            let classes: Vec<String> = code.split(';').filter_map(|p| match p.parse::<u32>().ok()? { 1 => Some("ab".to_string()), n @ (30..=37 | 90..=97) => Some(format!("a{n}")), _ => None }).collect();
            if !classes.is_empty() { out.push_str(&format!(r#"<span class="{}">"#, classes.join(" "))); open = true }
            continue;
        }
        match c { '&' => out.push_str("&amp;"), '<' => out.push_str("&lt;"), '>' => out.push_str("&gt;"), '"' => out.push_str("&quot;"), '\r' => {} c if c.is_control() && c != '\t' => {} c => out.push(c) }
    }
    if open { out.push_str("</span>") }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn githubs_lines_go_to_the_step_they_were_written_in() {
        let got = json!({ "steps": [
            { "name": "Set up job", "status": "completed", "conclusion": "success", "started_at": "2026-10-14T21:09:50Z", "completed_at": "2026-10-14T21:09:52Z" },
            { "name": "Run tests", "status": "completed", "conclusion": "failure", "started_at": "2026-10-14T21:09:52Z", "completed_at": "2026-10-14T21:10:30Z" },
            { "name": "Complete job", "status": "completed", "conclusion": "success", "started_at": "2026-10-14T21:10:30Z", "completed_at": "2026-10-14T21:10:31Z" }],
            "log": "\u{feff}2026-10-14T21:09:50.1000000Z Current runner version: '2.337.0'\n2026-10-14T21:09:52.0100000Z Prepare all required actions\n2026-10-14T21:09:52.2000000Z ##[group]Run npm test\n2026-10-14T21:09:59.0000000Z \u{1b}[31mFAIL\u{1b}[0m a <b>.test.js\n2026-10-14T21:10:29.0000000Z ##[error]Process completed with exit code 1.\n2026-10-14T21:10:30.5000000Z Cleaning up orphan processes" });
        let v = view(&json!({ "provider": "" }), &got, false);
        let steps = v["steps"].as_array().unwrap();
        let lines = |i: usize| steps[i]["lines"].as_array().unwrap().iter().map(|l| l[2].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(lines(0), ["Current runner version: '2.337.0'", "Prepare all required actions"], "a line of the next step's second before its heading stays");
        assert_eq!(lines(1), ["Run npm test", r#"<span class="a31">FAIL</span> a &lt;b&gt;.test.js"#, "Process completed with exit code 1."]);
        assert_eq!((steps[1]["state"].as_str(), steps[1]["ms"].as_u64(), steps[1]["lines"][0][1].as_str(), steps[1]["lines"][0][0].as_str()), (Some("failure"), Some(38_000), Some("g"), Some("21:09:52")));
        assert_eq!(lines(2), ["Cleaning up orphan processes"]);
        assert_eq!((v["errors"].as_u64(), v["warnings"].as_u64()), (Some(1), Some(0)));
    }

    #[test]
    fn gitlabs_sections_are_its_steps() {
        let log = "2026-10-14T15:22:39.414647Z 00O Running with gitlab-runner 19.4.1\n\
                   2026-10-14T15:22:40.000000Z 00O \u{1b}[0Ksection_start:1760455360:get_sources\r\u{1b}[0K\u{1b}[36;1mGetting source from Git repository\u{1b}[0;m\n\
                   2026-10-14T15:22:41.000000Z 00O Fetching changes...\n\
                   2026-10-14T15:22:42.000000Z 00O \u{1b}[0Ksection_end:1760455363:get_sources\r\u{1b}[0K\n\
                   2026-10-14T15:22:43.000000Z 00O \u{1b}[0Ksection_start:1760455363:step_script\r\u{1b}[0K\n\
                   2026-10-14T15:22:43.100000Z 00O \u{1b}[0K\u{1b}[36;1mExecuting \"step_script\" stage\u{1b}[0;m\n\
                   2026-10-14T15:22:44.000000Z 01E ERROR: something broke\n\
                   2026-10-14T15:22:45.000000Z 00O \u{1b}[0Ksection_end:1760455370:step_script\r\u{1b}[0K\n\
                   2026-10-14T15:22:46.000000Z 00O ERROR: Job failed: exit code 1";
        let v = view(&json!({ "provider": "gitlab", "conclusion": "failure" }), &json!({ "log": log }), false);
        let names: Vec<&str> = v["steps"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Started", "Getting source from Git repository", "Executing \"step_script\" stage", "Ended"]);
        assert_eq!((v["steps"][1]["ms"].as_u64(), v["steps"][2]["ms"].as_u64(), v["steps"][1]["lines"][0][2].as_str()), (Some(3000), Some(7000), Some("Fetching changes...")));
        assert_eq!((v["steps"][2]["state"].as_str(), v["steps"][3]["state"].as_str(), v["steps"][1]["state"].as_str()), (Some("failure"), Some("failure"), Some("success")), "where it failed, and its end");
        assert_eq!(v["errors"].as_u64(), Some(2));
    }

    #[test]
    fn a_run_from_a_computers_steps_each_with_its_log() {
        let got = json!({ "steps": [{ "name": "Run make", "state": "completed", "result": "succeeded", "text": "2026-10-14T21:09:53.1Z ok" }, { "name": "Run deploy", "state": "inProgress", "result": null, "text": "going" }] });
        let v = view(&json!({ "provider": "local" }), &got, true);
        assert_eq!((v["steps"][0]["state"].as_str(), v["steps"][0]["lines"][0][2].as_str(), v["steps"][1]["state"].as_str()), (Some("success"), Some("ok"), Some("running")));
    }
}
