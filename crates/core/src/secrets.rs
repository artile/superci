//! Secrets a CI keeps for runs from a computer (`superci secrets create`): written from a computer into the control
//! plane's own cloud, each a setting of its own (`SECRET_<NAME>_<its scope's id>`), never read back out but into a
//! job. A run gets those its workflows name (`${{ secrets.NAME }}`), of the ones whose scope holds for it; one named
//! and not kept is empty, as on GitHub.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// The most a value may hold: what every control plane's cloud keeps in one setting (Cloudflare's 5 KB less what the
/// setting says beside it).
pub const MOST_BYTES: usize = 4096;

/// Where a secret may be used; each part set narrows it (none: every run of this CI).
#[derive(Serialize, Deserialize, Clone, Default, Debug, PartialEq)]
pub struct Scope {
    /// A repository, owner/name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Branches, as GitHub's branch filters say them (`main`, `release/*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Workflow files, by path or name (`deploy.yml`, `.github/workflows/e2e-*.yml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// Jobs with this `environment:`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
}

/// A secret as kept.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Stored {
    pub name: String,
    pub value: String,
    #[serde(flatten)]
    pub scope: Scope,
    #[serde(default)]
    pub updated_ms: u64,
}

impl Scope {
    /// The part of its setting's name that tells it from the same name's other scopes.
    pub fn id(&self) -> String {
        if *self == Scope::default() { return "ALL".into() }
        let parts = [self.repo.as_deref().map(str::to_ascii_lowercase), self.branch.clone(), self.workflow.clone(), self.environment.as_deref().map(str::to_ascii_lowercase)];
        let text: Vec<String> = parts.into_iter().map(|p| p.unwrap_or_default()).collect();
        let hash = Sha256::digest(text.join("\n").as_bytes());
        hash[..5].iter().map(|b| format!("{b:02X}")).collect()
    }

    /// Each part set, in words (for lists): `repo acme/web, branch main`.
    pub fn said(&self) -> String {
        let parts: Vec<String> = [("repo", &self.repo), ("branch", &self.branch), ("workflow", &self.workflow), ("environment", &self.environment)]
            .iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}"))).collect();
        if parts.is_empty() { "every run".into() } else { parts.join(", ") }
    }

    /// Whether it holds for a run of this repository, branch (none: not a branch, a tag) and workflow file. The
    /// environment is each job's (see `resolve`).
    pub fn holds(&self, repo: &str, branch: Option<&str>, path: &str) -> bool {
        self.repo.as_deref().is_none_or(|r| r.eq_ignore_ascii_case(repo))
            && self.branch.as_deref().is_none_or(|b| branch.is_some_and(|x| glob(b, x)))
            && self.workflow.as_deref().is_none_or(|w| glob(w, path) || glob(w, path.rsplit('/').next().unwrap_or(path)))
    }

    /// How narrow it is: of two kept under one name, the narrower is the one given (an environment's first, as on
    /// GitHub, then a repository's, a workflow's, a branch's).
    fn rank(&self) -> u8 {
        8 * self.environment.is_some() as u8 + 4 * self.repo.is_some() as u8 + 2 * self.workflow.is_some() as u8 + self.branch.is_some() as u8
    }
}

/// The setting a secret is kept as.
pub fn setting(name: &str, scope: &Scope) -> String { format!("SECRET_{name}_{}", scope.id()) }

/// A secret's name as GitHub allows it: letters, digits and `_`, not starting with a digit or `GITHUB_`; kept in
/// capitals (workflows name it in any case).
pub fn valid_name(name: &str) -> Result<String, String> {
    let upper = name.trim().to_ascii_uppercase();
    if upper.is_empty() { return Err("A secret needs a name.".into()) }
    if upper.len() > 100 { return Err("A secret's name is 100 characters at most.".into()) }
    if !upper.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { return Err(format!("{name}: a secret's name has only letters, digits and _.")) }
    if upper.starts_with(|c: char| c.is_ascii_digit()) { return Err(format!("{name}: a secret's name does not start with a digit.")) }
    if upper.starts_with("GITHUB_") { return Err(format!("{name}: names starting with GITHUB_ are GitHub's own.")) }
    Ok(upper)
}

/// A scope as given: each part trimmed, empty ones left out; a repository must be owner/name.
pub fn valid_scope(repo: Option<&str>, branch: Option<&str>, workflow: Option<&str>, environment: Option<&str>) -> Result<Scope, String> {
    let part = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    let scope = Scope { repo: part(repo), branch: part(branch), workflow: part(workflow), environment: part(environment) };
    if let Some(r) = &scope.repo {
        let ok = r.split('/').count() == 2 && r.split('/').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)));
        if !ok { return Err(format!("{r}: a repository is owner/name.")) }
    }
    Ok(scope)
}

/// A value as it may be kept.
pub fn valid_value(value: &str) -> Result<(), String> {
    if value.is_empty() { return Err("The value is empty.".into()) }
    if value.len() > MOST_BYTES { return Err(format!("The value is {} bytes; a secret holds 4 KB at most. Keep a larger file encrypted in the repository and its key here.", value.len())) }
    Ok(())
}

/// GitHub's filter patterns: `*` any characters but `/`, `**` any characters, `?` one, `[...]` one of them; the rest
/// as it is.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') if p.get(1) == Some(&'*') => (0..=t.len()).any(|i| go(&p[2..], &t[i..])),
            Some('*') => (0..=t.len()).take_while(|i| *i == 0 || t[i - 1] != '/').any(|i| go(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some('[') => match p.iter().position(|c| *c == ']') {
                Some(end) if end > 1 => !t.is_empty() && class(&p[1..end], t[0]) && go(&p[end + 1..], &t[1..]),
                _ => t.first() == Some(&'[') && go(&p[1..], &t[1..]),
            },
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    // A class's ranges (`a-z`) and characters, one after another.
    fn class(set: &[char], c: char) -> bool {
        match set {
            [from, '-', to, rest @ ..] => (*from <= c && c <= *to) || class(rest, c),
            [one, rest @ ..] => *one == c || class(rest, c),
            [] => false,
        }
    }
    go(&p, &t)
}

/// The secrets workflow files name in their expressions, in capitals; None when one reads them all (`toJSON(secrets)`,
/// `secrets[matrix.name]`).
pub fn named(texts: &[&str]) -> Option<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for text in texts {
        let mut rest = *text;
        while let Some(at) = rest.find("${{") {
            let inner = &rest[at + 3..];
            let end = inner.find("}}").unwrap_or(inner.len());
            let expr = &inner[..end];
            rest = &inner[end..];
            let lower = expr.to_ascii_lowercase();
            let mut from = 0;
            while let Some(i) = lower[from..].find("secrets").map(|i| i + from) {
                from = i + 7;
                // (A word of its own: not `github.secrets_x`.)
                let before = expr[..i].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
                let after = expr[from..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || "_-".contains(c));
                if before || after { continue }
                let after = expr[from..].trim_start();
                let key = if let Some(r) = after.strip_prefix('.') {
                    let r = r.trim_start();
                    Some(r.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect::<String>())
                } else if let Some(r) = after.strip_prefix('[') {
                    let r = r.trim_start();
                    let q = r.chars().next().filter(|q| *q == '\'' || *q == '"');
                    q.and_then(|q| { let s = &r[1..]; let e = s.find(q)?; s[e + 1..].trim_start().starts_with(']').then(|| s[..e].to_string()) })
                } else { None };
                match key.filter(|k| !k.is_empty()) { Some(k) => { names.insert(k.to_ascii_uppercase()); } None => return None }
            }
        }
    }
    Some(names)
}

/// What a run may be given of what is kept: each secret whose scope holds for its repository, branch and workflow
/// file, and that its workflows name (`named`; None: all).
pub fn for_run(kept: &[Stored], repo: &str, branch: Option<&str>, path: &str, named: Option<&BTreeSet<String>>) -> Vec<Stored> {
    kept.iter().filter(|s| s.scope.holds(repo, branch, path) && named.is_none_or(|n| n.contains(&s.name))).cloned().collect()
}

/// A job's secrets from what its run was given: under each name the narrowest that holds for the job's environment
/// (a later one of two as narrow).
pub fn resolve(given: &[Stored], environment: Option<&str>) -> Vec<(String, String)> {
    let mut best: std::collections::BTreeMap<&str, &Stored> = Default::default();
    for s in given.iter().filter(|s| s.scope.environment.as_deref().is_none_or(|e| environment.is_some_and(|x| x.eq_ignore_ascii_case(e)))) {
        let better = best.get(s.name.as_str()).is_none_or(|b| (s.scope.rank(), s.updated_ms) > (b.scope.rank(), b.updated_ms));
        if better { best.insert(&s.name, s); }
    }
    best.into_iter().map(|(n, s)| (n.to_string(), s.value.clone())).collect()
}

/// The branch a ref names (`refs/heads/x`, or a bare name); None for a tag or a pull request's ref.
pub fn branch_of(reference: &str) -> Option<&str> {
    if let Some(b) = reference.strip_prefix("refs/heads/") { return Some(b) }
    (!reference.is_empty() && !reference.starts_with("refs/")).then_some(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(name: &str, value: &str, scope: Scope, at: u64) -> Stored { Stored { name: name.into(), value: value.into(), scope, updated_ms: at } }

    #[test]
    fn names_are_githubs() {
        assert_eq!(valid_name(" npm_token ").unwrap(), "NPM_TOKEN");
        assert!(valid_name("GITHUB_TOKEN").is_err());
        assert!(valid_name("1PASSWORD").is_err());
        assert!(valid_name("NPM-TOKEN").is_err());
        assert!(valid_name("").is_err());
        assert!(valid_name(&"A".repeat(101)).is_err());
        assert!(valid_value("").is_err());
        assert!(valid_value(&"x".repeat(MOST_BYTES + 1)).is_err());
        assert!(valid_value("x").is_ok());
        assert!(valid_scope(Some("acme"), None, None, None).is_err());
        assert!(valid_scope(Some("acme/web/x"), None, None, None).is_err());
        assert_eq!(valid_scope(Some(" acme/web "), Some(""), None, Some("prod")).unwrap(), Scope { repo: Some("acme/web".into()), environment: Some("prod".into()), ..Default::default() });
    }

    #[test]
    fn each_scope_is_a_setting_of_its_own() {
        assert_eq!(setting("NPM_TOKEN", &Scope::default()), "SECRET_NPM_TOKEN_ALL");
        let a = Scope { repo: Some("Acme/Web".into()), ..Default::default() };
        let b = Scope { repo: Some("acme/web".into()), ..Default::default() };
        let c = Scope { branch: Some("acme/web".into()), ..Default::default() };
        assert_eq!(a.id(), b.id());
        assert_ne!(a.id(), c.id());
        assert_eq!(a.id().len(), 10);
        assert_eq!(Scope::default().said(), "every run");
        assert_eq!(Scope { repo: Some("acme/web".into()), branch: Some("main".into()), ..Default::default() }.said(), "repo acme/web, branch main");
    }

    #[test]
    fn patterns_are_githubs_filters() {
        assert!(glob("main", "main"));
        assert!(!glob("main", "main2"));
        assert!(glob("release/*", "release/1.2"));
        assert!(!glob("release/*", "release/1/2"));
        assert!(glob("release/**", "release/1/2"));
        assert!(glob("v?", "v1"));
        assert!(glob("v[0-9]", "v7"));
        assert!(!glob("v[0-9]", "va"));
        assert!(glob("v[ab]", "vb"));
        // Ranges and characters together; below a range, above it, and a dash at the end taken as itself.
        assert!(glob("v[a-cx]", "vx") && glob("v[a-cx]", "vb") && !glob("v[b-cx]", "va") && !glob("v[a-cx]", "vd") && glob("v[a-]", "v-"));
        assert!(glob("[", "["));
        assert!(glob("*.yml", "deploy.yml"));
    }

    #[test]
    fn a_scope_holds_for_its_repository_branch_and_workflow() {
        let s = Scope { repo: Some("acme/web".into()), branch: Some("release/*".into()), workflow: Some("deploy.yml".into()), environment: None };
        assert!(s.holds("ACME/web", Some("release/1"), ".github/workflows/deploy.yml"));
        assert!(!s.holds("acme/api", Some("release/1"), ".github/workflows/deploy.yml"));
        assert!(!s.holds("acme/web", Some("main"), ".github/workflows/deploy.yml"));
        assert!(!s.holds("acme/web", None, ".github/workflows/deploy.yml"));
        assert!(!s.holds("acme/web", Some("release/1"), ".github/workflows/ci.yml"));
        assert!(Scope { workflow: Some(".github/workflows/e2e-*".into()), ..Default::default() }.holds("a/b", None, ".github/workflows/e2e-web.yml"));
        assert_eq!(branch_of("refs/heads/feature/x"), Some("feature/x"));
        assert_eq!(branch_of("refs/tags/v1"), None);
        assert_eq!(branch_of("main"), Some("main"));
        assert_eq!(branch_of(""), None);
    }

    #[test]
    fn the_secrets_a_workflow_names_are_read_from_its_expressions() {
        let wf = "jobs:\n  a:\n    secrets: inherit\n    steps:\n      - run: echo ${{ secrets.npm_token }} ${{ secrets['Deploy_Key'] }}\n        env:\n          X: ${{ format('{0}', secrets . OTHER) }}\n          Y: ${{ github.secrets_like }}\n# secrets are fine here\n";
        assert_eq!(named(&[wf]).unwrap().into_iter().collect::<Vec<_>>(), ["DEPLOY_KEY", "NPM_TOKEN", "OTHER"]);
        assert_eq!(named(&["run: ${{ toJSON(secrets) }}"]), None);
        assert_eq!(named(&["run: ${{ secrets[matrix.name] }}"]), None);
        assert_eq!(named(&["run: ${{ secrets.A }}", "run: ${{ secrets.B }}"]).unwrap().len(), 2);
        assert!(named(&["run: echo hi"]).unwrap().is_empty());
        assert!(named(&[]).unwrap().is_empty(), "no workflow files: none named");
        assert_eq!(named(&["run: ${{ secrets['A }}"]), None, "a name whose quote never ends reads no one name");
    }

    #[test]
    fn a_job_gets_the_narrowest_under_each_name() {
        let all = vec![
            kept("TOKEN", "everywhere", Scope::default(), 1),
            kept("TOKEN", "web", Scope { repo: Some("acme/web".into()), ..Default::default() }, 1),
            kept("TOKEN", "prod", Scope { environment: Some("production".into()), ..Default::default() }, 1),
            kept("TOKEN", "api", Scope { repo: Some("acme/api".into()), ..Default::default() }, 1),
            kept("KEY", "old", Scope::default(), 1),
            kept("KEY", "new", Scope { branch: Some("main".into()), ..Default::default() }, 2),
            kept("UNUSED", "x", Scope::default(), 1),
        ];
        let names: BTreeSet<String> = ["TOKEN", "KEY"].iter().map(|s| s.to_string()).collect();
        let given = for_run(&all, "acme/web", Some("main"), ".github/workflows/ci.yml", Some(&names));
        assert_eq!(given.len(), 5);
        assert_eq!(resolve(&given, None), [("KEY".to_string(), "new".to_string()), ("TOKEN".into(), "web".into())]);
        assert_eq!(resolve(&given, Some("Production")), [("KEY".to_string(), "new".to_string()), ("TOKEN".into(), "prod".into())]);
        assert_eq!(for_run(&all, "acme/web", Some("dev"), "x.yml", None).len(), 5);
    }
}
