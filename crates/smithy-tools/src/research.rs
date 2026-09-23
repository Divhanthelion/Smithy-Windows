//! Research that can be checked after the fact.
//!
//! The research procedure says every finding needs a fetched source and a
//! quote. Until this module that rule was kept by the model's honesty alone:
//! pages were read and forgotten, so nobody could later confirm that a quote
//! was on the page it was attributed to. Three pieces close that:
//!
//! - **[`SourceStore`]** — `web_fetch` saves every page it renders, by the
//!   hash of its text, outside the Project (`~/.local/share/smithy/sources`),
//!   and tells the model the short id. Shared across Projects, so a re-check
//!   months later still has the page as it was read.
//! - **[`check_note`]** — reads a Note's findings and verifies each: its
//!   source id exists, the URL it names is the one that was fetched, and every
//!   quote appears in the saved text (whitespace, case, quote marks and
//!   markup normalised; `...` may elide). Repository citations
//!   (`{repo:path:line}`) are checked against the file. Findings marked
//!   `(key)` must rest on at least two domains between them.
//! - **[`find_notes`]** — Notes are found again by their question, so a later
//!   step reads what an earlier one learned instead of researching it again.
//!
//! A finding line looks like:
//!
//! ```text
//! - [spec] (key) Durations may carry a fraction only on the smallest unit — https://www.rfc-editor.org/rfc/rfc3339 {src:1a2b3c4d5e6f} "the smallest value may have a decimal fraction"
//! ```

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Where fetched pages are kept, and the log of every fetch.
#[derive(Debug, Clone)]
pub struct SourceStore {
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMeta {
    pub id: String,
    pub url: String,
    pub final_url: String,
    pub fetched_at: u64,
    pub status: u16,
    pub content_type: String,
    pub chars: usize,
}

/// Twelve hex characters: 48 bits, plenty for one machine's reading.
const ID_LEN: usize = 12;

impl SourceStore {
    pub fn new(dir: impl Into<PathBuf>) -> SourceStore {
        SourceStore { dir: dir.into() }
    }

    /// `~/.local/share/smithy/sources`, or `SMITHY_SOURCES_DIR`.
    pub fn default_location() -> Option<SourceStore> {
        if let Some(dir) = std::env::var_os("SMITHY_SOURCES_DIR") {
            return Some(SourceStore::new(dir));
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        Some(SourceStore::new(home.join(".local/share/smithy/sources")))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Save a rendered page. The same text fetched twice is one file and two
    /// log lines.
    pub fn save(
        &self,
        url: &str,
        final_url: &str,
        status: u16,
        content_type: &str,
        text: &str,
    ) -> Result<SourceMeta, String> {
        let id = source_id(text);
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("could not create {}: {e}", self.dir.display()))?;
        let file = self.dir.join(format!("{id}.txt"));
        if !file.exists() {
            std::fs::write(&file, text)
                .map_err(|e| format!("could not save {}: {e}", file.display()))?;
        }
        let meta = SourceMeta {
            id,
            url: url.to_string(),
            final_url: final_url.to_string(),
            fetched_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            status,
            content_type: content_type.to_string(),
            chars: text.chars().count(),
        };
        let line = serde_json::to_string(&meta).map_err(|e| e.to_string())?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("index.jsonl"))
            .and_then(|mut f| writeln!(f, "{line}"))
            .map_err(|e| format!("could not log the fetch: {e}"))?;
        Ok(meta)
    }

    pub fn text(&self, id: &str) -> Option<String> {
        if !is_id(id) {
            return None;
        }
        std::fs::read_to_string(self.dir.join(format!("{id}.txt"))).ok()
    }

    /// Every fetch that produced `id`.
    pub fn fetches(&self, id: &str) -> Vec<SourceMeta> {
        let Ok(text) = std::fs::read_to_string(self.dir.join("index.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str::<SourceMeta>(l).ok())
            .filter(|m| m.id == id)
            .collect()
    }
}

pub fn source_id(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()[..ID_LEN]
        .to_string()
}

fn is_id(s: &str) -> bool {
    s.len() >= 8 && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Citation {
    Source(String),
    Repo {
        path: String,
        start: usize,
        end: usize,
    },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// 1-based line in the Note.
    pub line: usize,
    pub text: String,
    pub kind: String,
    pub key: bool,
    pub url: Option<String>,
    pub citation: Citation,
    pub quotes: Vec<String>,
}

/// The findings of a Note: `- [kind] …` bullets under `## Findings`, or
/// anywhere if the Note has no such heading.
pub fn parse_findings(note: &str) -> Vec<Finding> {
    let bullet = Regex::new(r"^\s*[-*]\s+\[(owner|spec|empirical|opinion)\]").unwrap();
    let has_heading = note.lines().any(is_findings_heading);
    let mut in_findings = !has_heading;
    let mut out = Vec::new();
    for (i, line) in note.lines().enumerate() {
        if line.starts_with("## ") {
            in_findings = !has_heading || is_findings_heading(line);
            continue;
        }
        if !in_findings {
            continue;
        }
        let Some(c) = bullet.captures(line) else {
            continue;
        };
        out.push(parse_finding(i + 1, line, &c[1]));
    }
    out
}

fn is_findings_heading(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("## findings")
}

fn parse_finding(line_no: usize, line: &str, kind: &str) -> Finding {
    let src = Regex::new(r"\{src:([0-9a-fA-F]{8,64})\}").unwrap();
    let repo = Regex::new(r"\{repo:([^}:]+):(\d+)(?:-(\d+))?\}").unwrap();
    let url = Regex::new(r#"https?://[^\s)>\]}"'”]+"#).unwrap();
    let quote = Regex::new(r#""([^"]+)"|“([^”]+)”"#).unwrap();
    let citation = if let Some(c) = src.captures(line) {
        Citation::Source(c[1].to_lowercase())
    } else if let Some(c) = repo.captures(line) {
        let start = c[2].parse().unwrap_or(1);
        let end = c
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(start);
        Citation::Repo {
            path: c[1].trim().to_string(),
            start,
            end: end.max(start),
        }
    } else {
        Citation::None
    };
    Finding {
        line: line_no,
        text: line.to_string(),
        kind: kind.to_string(),
        key: line.contains("(key)"),
        url: url
            .find(line)
            .map(|m| m.as_str().trim_end_matches(['.', ',', ';']).to_string()),
        citation,
        quotes: quote
            .captures_iter(line)
            .filter_map(|c| {
                c.get(1)
                    .or_else(|| c.get(2))
                    .map(|m| m.as_str().to_string())
            })
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Verified,
    NoCitation,
    NoQuote,
    QuoteTooShort(String),
    UnknownSource(String),
    UrlMismatch { cited: String, fetched: String },
    QuoteNotFound(String),
    RepoFileMissing(String),
}

impl Status {
    pub fn is_verified(&self) -> bool {
        matches!(self, Status::Verified)
    }

    pub fn reason(&self) -> String {
        match self {
            Status::Verified => "verified".into(),
            Status::NoCitation => "no {src:…} or {repo:path:line} citation".into(),
            Status::NoQuote => "no quote to check".into(),
            Status::QuoteTooShort(q) => format!("quote too short to identify anything: \"{q}\""),
            Status::UnknownSource(id) => {
                format!("source {id} was never fetched (no saved page by that id)")
            }
            Status::UrlMismatch { cited, fetched } => {
                format!("cites {cited}, but source was fetched from {fetched}")
            }
            Status::QuoteNotFound(q) => format!("quote not found in the source: \"{q}\""),
            Status::RepoFileMissing(p) => format!("{p} does not exist in the Project"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub finding: Finding,
    pub status: Status,
    /// Host (without `www.`) for a web source, `repo` for the Project.
    pub domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCheck {
    pub findings: Vec<Checked>,
    /// Problems with the Note as a whole.
    pub problems: Vec<String>,
}

impl NoteCheck {
    pub fn verified(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.status.is_verified())
            .count()
    }

    pub fn passes(&self) -> bool {
        self.problems.is_empty()
    }

    /// For the model and the user: one line per finding, then the verdict.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for c in &self.findings {
            let mark = if c.status.is_verified() { "✓" } else { "✗" };
            out.push_str(&format!(
                "{mark} line {} [{}]{} {}\n",
                c.finding.line,
                c.finding.kind,
                if c.finding.key { " (key)" } else { "" },
                c.status.reason()
            ));
        }
        out.push_str(&format!(
            "\n{} of {} findings verified.\n",
            self.verified(),
            self.findings.len()
        ));
        if self.passes() {
            out.push_str("PASS\n");
        } else {
            out.push_str("FAIL\n");
            for p in &self.problems {
                out.push_str(&format!("- {p}\n"));
            }
        }
        out
    }
}

/// Words a quote needs before it identifies anything.
const MIN_QUOTE_WORDS: usize = 4;

/// Check every finding in `note` against the saved sources and the Project.
pub fn check_note(note: &str, store: &SourceStore, root: &Path) -> NoteCheck {
    let findings: Vec<Checked> = parse_findings(note)
        .into_iter()
        .map(|f| {
            let (status, domain) = check_finding(&f, store, root);
            Checked {
                finding: f,
                status,
                domain,
            }
        })
        .collect();

    let mut problems = Vec::new();
    if findings.is_empty() {
        problems.push(
            "no findings: expected `- [owner|spec|empirical|opinion] claim — url {src:id} \
             \"quote\"` bullets under ## Findings"
                .into(),
        );
    } else if !findings.iter().any(|c| c.status.is_verified()) {
        problems.push("no finding could be verified".into());
    }
    let unverified = findings.iter().filter(|c| !c.status.is_verified()).count();
    if unverified > 0 && !findings.is_empty() {
        problems.push(format!(
            "{unverified} finding(s) failed verification: fix the quote or citation, or drop the \
             claim"
        ));
    }
    let key: Vec<&Checked> = findings.iter().filter(|c| c.finding.key).collect();
    if !findings.is_empty() && key.is_empty() {
        problems.push("no finding is marked (key): mark the load-bearing ones".into());
    }
    if !key.is_empty() {
        let domains: BTreeSet<&str> = key
            .iter()
            .filter(|c| c.status.is_verified())
            .filter_map(|c| c.domain.as_deref())
            .collect();
        if domains.len() < 2 {
            problems.push(format!(
                "the (key) findings rest on {} verified domain(s); load-bearing claims need two \
                 independent sources",
                domains.len()
            ));
        }
    }
    NoteCheck { findings, problems }
}

fn check_finding(f: &Finding, store: &SourceStore, root: &Path) -> (Status, Option<String>) {
    match &f.citation {
        Citation::None => (Status::NoCitation, None),
        Citation::Source(id) => {
            let fetches = store.fetches(id);
            let Some(text) = store.text(id) else {
                return (Status::UnknownSource(id.clone()), None);
            };
            let fetched = fetches
                .last()
                .map(|m| m.final_url.clone())
                .unwrap_or_default();
            let domain = domain_of(f.url.as_deref().unwrap_or(&fetched));
            if let Some(cited) = &f.url {
                let matches = fetches
                    .iter()
                    .any(|m| same_url(cited, &m.url) || same_url(cited, &m.final_url));
                if !matches && !fetches.is_empty() {
                    return (
                        Status::UrlMismatch {
                            cited: cited.clone(),
                            fetched,
                        },
                        domain,
                    );
                }
            }
            (check_quotes(&f.quotes, &text), domain)
        }
        Citation::Repo { path, start, end } => {
            let Ok(text) = std::fs::read_to_string(root.join(path)) else {
                return (Status::RepoFileMissing(path.clone()), None);
            };
            // A few lines of slack: citations drift by a line or two as a
            // file is edited, and the quote is what identifies the claim.
            let lines: Vec<&str> = text.lines().collect();
            let from = start.saturating_sub(4);
            let to = (*end + 3).min(lines.len());
            let window = lines
                .get(from..to)
                .map(|l| l.join("\n"))
                .unwrap_or_default();
            (check_quotes(&f.quotes, &window), Some("repo".into()))
        }
    }
}

fn check_quotes(quotes: &[String], text: &str) -> Status {
    if quotes.is_empty() {
        return Status::NoQuote;
    }
    let haystack = normalize(text);
    for q in quotes {
        let words = normalize(q)
            .split_whitespace()
            .filter(|w| *w != "...")
            .count();
        if words < MIN_QUOTE_WORDS {
            return Status::QuoteTooShort(q.clone());
        }
        if !contains_in_order(&haystack, q) {
            return Status::QuoteNotFound(q.clone());
        }
    }
    Status::Verified
}

/// Every `...`-separated part of the quote, in order.
fn contains_in_order(haystack: &str, quote: &str) -> bool {
    let mut from = 0;
    for part in normalize(quote)
        .split("...")
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        match haystack[from..].find(part) {
            Some(i) => from += i + part.len(),
            None => return false,
        }
    }
    true
}

/// Case, whitespace, typographic quotes and dashes, markdown emphasis, and
/// the `[1]` link markers the HTML renderer adds.
pub fn normalize(s: &str) -> String {
    let refs = Regex::new(r"\[\d+\]").unwrap();
    let s = refs.replace_all(s, "");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '‘' | '’' | '′' => out.push('\''),
            '“' | '”' | '″' => out.push('"'),
            '–' | '—' | '−' => out.push('-'),
            '…' => out.push_str("..."),
            '*' | '_' | '`' | '[' | ']' => {}
            '\u{a0}' => out.push(' '),
            c => out.extend(c.to_lowercase()),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn same_url(a: &str, b: &str) -> bool {
    let clean = |u: &str| {
        let u = u.split('#').next().unwrap_or(u);
        let u = u
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        u.trim_end_matches('/').to_lowercase()
    };
    clean(a) == clean(b)
}

pub fn domain_of(url: &str) -> Option<String> {
    let host = url::Url::parse(url).ok()?.host_str()?.to_lowercase();
    Some(host.trim_start_matches("www.").to_string())
}

/// Mark the findings that failed, in place, so a Note that is kept anyway
/// says which of its claims are not backed. Idempotent.
pub fn annotate(note: &str, check: &NoteCheck) -> String {
    let failed: std::collections::BTreeMap<usize, String> = check
        .findings
        .iter()
        .filter(|c| !c.status.is_verified())
        .map(|c| (c.finding.line, c.status.reason()))
        .collect();
    note.lines()
        .enumerate()
        .map(|(i, l)| match failed.get(&(i + 1)) {
            Some(why) if !l.contains("⚠ unverified") => format!("{l} — ⚠ unverified: {why}"),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
        + if note.ends_with('\n') { "\n" } else { "" }
}

// ---------------------------------------------------------------------------
// Finding Notes again
// ---------------------------------------------------------------------------

/// Where Notes live in a Project.
pub const NOTES_DIR: &str = ".smithy/research";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteMeta {
    /// Relative to the Project root, `/`-separated.
    pub path: String,
    pub question: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

pub fn note_meta(path: &str, text: &str) -> NoteMeta {
    let field = |name: &str| {
        text.lines().find_map(|l| {
            l.trim()
                .strip_prefix(&format!("**{name}:**"))
                .map(|v| v.trim().to_string())
        })
    };
    let title = text
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()));
    NoteMeta {
        path: path.to_string(),
        question: field("Pinned question")
            .or_else(|| field("Pinned decision"))
            .or(title)
            .unwrap_or_default(),
        status: field("Status").unwrap_or_default(),
        task: field("Task"),
    }
}

/// Every Note in the Project, newest name first.
pub fn list_notes(root: &Path) -> Vec<NoteMeta> {
    let dir = root.join(NOTES_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut notes: Vec<NoteMeta> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            Some(note_meta(&format!("{NOTES_DIR}/{name}"), &text))
        })
        .collect();
    notes.sort_by(|a, b| b.path.cmp(&a.path));
    notes
}

/// Notes whose question shares words with `query`, best first. Plain word
/// overlap: the candidates are few, and the judgment of whether one actually
/// answers the question belongs to whoever reads it.
pub fn find_notes(root: &Path, query: &str, limit: usize) -> Vec<(f64, NoteMeta)> {
    let q = words(query);
    if q.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(f64, NoteMeta)> = list_notes(root)
        .into_iter()
        .filter_map(|n| {
            let w = words(&n.question);
            let shared = q.intersection(&w).count();
            (shared > 0).then(|| (shared as f64 / q.len().max(1) as f64, n))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.truncate(limit);
    scored
}

fn words(s: &str) -> BTreeSet<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "are", "what", "which", "how", "does", "with", "that", "this", "from",
        "into", "may", "can", "its", "use", "when", "should", "their", "there", "not", "any",
        "all",
    ];
    s.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.len() >= 3 && !STOP.contains(&w.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "Appendix A. ISO 8601 Collected ABNF\n\n   Durations:\n\n   dur-second = 1*DIGIT \"S\"\n   The smallest value used may also have a decimal fraction, as in \u{201c}P0.5Y\u{201d}.\n";

    fn store_with_page() -> (tempfile::TempDir, SourceStore, String) {
        let tmp = tempfile::tempdir().unwrap();
        let store = SourceStore::new(tmp.path().join("sources"));
        let meta = store
            .save(
                "https://www.rfc-editor.org/rfc/rfc3339",
                "https://www.rfc-editor.org/rfc/rfc3339.html",
                200,
                "text/html",
                PAGE,
            )
            .unwrap();
        (tmp, store, meta.id)
    }

    #[test]
    fn saving_is_content_addressed_and_logged() {
        let (_t, store, id) = store_with_page();
        assert_eq!(id.len(), ID_LEN);
        assert_eq!(store.text(&id).as_deref(), Some(PAGE));
        store
            .save(
                "https://mirror.example/rfc3339",
                "https://mirror.example/rfc3339",
                200,
                "text/html",
                PAGE,
            )
            .unwrap();
        assert_eq!(store.fetches(&id).len(), 2, "one file, two fetches");
        assert_eq!(store.text("../../etc/passwd"), None);
    }

    fn note(lines: &[String]) -> String {
        format!("# Q\n\n**Pinned question:** Which duration fractions are allowed?\n\n## Findings\n{}\n\n## Implication\nx\n", lines.join("\n"))
    }

    #[test]
    fn a_true_quote_verifies_and_an_invented_one_does_not() {
        let (tmp, store, id) = store_with_page();
        let n = note(&[
            format!("- [spec] (key) Only the smallest unit may be fractional — https://www.rfc-editor.org/rfc/rfc3339 {{src:{id}}} \"The smallest value used may also have a decimal fraction\""),
            format!("- [spec] Weeks mix freely with days — https://www.rfc-editor.org/rfc/rfc3339 {{src:{id}}} \"weeks may be combined with days in any duration\""),
        ]);
        let check = check_note(&n, &store, tmp.path());
        assert!(
            check.findings[0].status.is_verified(),
            "{:?}",
            check.findings[0].status
        );
        assert!(matches!(check.findings[1].status, Status::QuoteNotFound(_)));
        assert!(!check.passes());
        let annotated = annotate(&n, &check);
        assert!(annotated.contains("any duration\" — ⚠ unverified: quote not found"));
        assert_eq!(annotate(&annotated, &check), annotated, "idempotent");
    }

    #[test]
    fn quotes_survive_typography_markup_and_elision() {
        let (tmp, store, id) = store_with_page();
        let n = note(&[format!(
            "- [spec] x — {{src:{id}}} “the SMALLEST value used ... a decimal fraction, as in \"P0.5Y\"”"
        )]);
        let check = check_note(&n, &store, tmp.path());
        assert!(
            check.findings[0].status.is_verified(),
            "{:?}",
            check.findings[0].status
        );
    }

    #[test]
    fn citations_must_exist_match_and_say_something() {
        let (tmp, store, id) = store_with_page();
        let n = note(&[
            "- [spec] x — https://a.example/ {src:deadbeefcafe} \"some words that are long\"".into(),
            format!("- [spec] x — https://evil.example/page {{src:{id}}} \"The smallest value used may\""),
            format!("- [spec] x — {{src:{id}}} \"decimal fraction\""),
            "- [opinion] x with no citation \"some words that are long\"".into(),
            format!("- [spec] x {{src:{id}}}"),
        ]);
        let s: Vec<Status> = check_note(&n, &store, tmp.path())
            .findings
            .into_iter()
            .map(|c| c.status)
            .collect();
        assert!(matches!(s[0], Status::UnknownSource(_)), "{:?}", s[0]);
        assert!(matches!(s[1], Status::UrlMismatch { .. }), "{:?}", s[1]);
        assert!(matches!(s[2], Status::QuoteTooShort(_)), "{:?}", s[2]);
        assert_eq!(s[3], Status::NoCitation);
        assert_eq!(s[4], Status::NoQuote);
    }

    #[test]
    fn key_findings_need_two_domains() {
        let (tmp, store, id) = store_with_page();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/lib.rs"),
            "a\nb\n// fractions only on the smallest unit\nc\n",
        )
        .unwrap();
        let web = format!("- [spec] (key) x — https://www.rfc-editor.org/rfc/rfc3339 {{src:{id}}} \"The smallest value used may also have\"");
        let one = check_note(&note(std::slice::from_ref(&web)), &store, tmp.path());
        assert!(
            one.problems.iter().any(|p| p.contains("1 verified domain")),
            "{:?}",
            one.problems
        );

        let repo =
            "- [owner] (key) x — {repo:src/lib.rs:3} \"fractions only on the smallest unit\""
                .to_string();
        let two = check_note(&note(&[web, repo]), &store, tmp.path());
        assert!(two.passes(), "{}", two.render());
        assert_eq!(two.verified(), 2);
    }

    #[test]
    fn only_the_findings_section_counts() {
        let n = "# Q\n\n## Seeds\n- [owner] a seed, not a finding\n\n## Findings\n- [spec] real \"one two three four\"\n\n## Dropped\n- [opinion] dropped\n";
        let f = parse_findings(n);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, 7);
    }

    #[test]
    fn notes_are_found_by_their_question() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(NOTES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("2026-09-23-durations.md"), "# x\n\n**Status:** verified\n**Pinned question:** Which ISO 8601 duration designators allow fractions?\n**Task:** T1\n").unwrap();
        std::fs::write(
            dir.join("2026-09-22-tls.md"),
            "# How does rustls pick a cipher suite?\n",
        )
        .unwrap();
        let hits = find_notes(
            tmp.path(),
            "may ISO 8601 durations carry fractional seconds",
            5,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.path, ".smithy/research/2026-09-23-durations.md");
        assert_eq!(hits[0].1.task.as_deref(), Some("T1"));
        assert_eq!(list_notes(tmp.path()).len(), 2);
    }
}
