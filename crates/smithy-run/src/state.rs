//! Where a Run is: the one file a resume reads.
//!
//! `.smithy/runs/<id>/state.json`, committed with every checkpoint, so the
//! branch tip always says what the Run believed when it last made progress.
//! Between checkpoints it is also written to disk (not committed) as work
//! happens, so a crash loses at most the Attempt in flight.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::check::CheckOutcome;
use crate::toolchain::Toolchain;

/// The ceilings chosen in the interview. `run` flags may override them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ceilings {
    pub hours: u64,
    pub attempts_per_task: usize,
    /// Blocked Tasks in a row before the Run stops: past this, the Plan
    /// itself is the likelier problem.
    pub consecutive_blocked: usize,
    /// Check rounds inside one Attempt.
    pub rounds_per_attempt: usize,
    /// Minutes of research the whole Run may spend, however it was asked for.
    /// `None` is a third of `hours`: research scales with the Run, so a big
    /// project given more hours gets more of it.
    #[serde(default)]
    pub research_minutes: Option<u64>,
}

/// Ceilings given again with `--resume`: each one set replaces the Run's
/// own, so a Run can be given more time, attempts or research after it began.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CeilingChanges {
    pub hours: Option<u64>,
    pub attempts_per_task: Option<usize>,
    pub research_minutes: Option<u64>,
}

impl CeilingChanges {
    pub fn apply(&self, c: &mut Ceilings) {
        if let Some(h) = self.hours {
            c.hours = h;
        }
        if let Some(a) = self.attempts_per_task {
            c.attempts_per_task = a;
        }
        if let Some(m) = self.research_minutes {
            c.research_minutes = Some(m);
        }
    }
}

impl Ceilings {
    /// Seconds of research the Run may spend.
    pub fn research_budget(&self) -> u64 {
        self.research_minutes.unwrap_or(self.hours * 20) * 60
    }
}

impl Default for Ceilings {
    fn default() -> Self {
        Ceilings {
            hours: 8,
            attempts_per_task: 3,
            consecutive_blocked: 2,
            rounds_per_attempt: 6,
            research_minutes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Active,
    Done,
    Blocked,
    /// Stopped by the Guardrail. Cleared only by `--allow`.
    Flagged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    pub status: TaskStatus,
    pub attempts: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    /// The last round of Checks, passing or not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_checks: Vec<CheckOutcome>,
    /// What the last Attempt left for the next one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff: Option<String>,
    /// Notes this Task produced or reused, as `.smithy/research/…` paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    #[serde(default)]
    pub seconds: u64,
}

impl Default for TaskState {
    fn default() -> Self {
        TaskState {
            status: TaskStatus::Pending,
            attempts: 0,
            commit: None,
            blocker: None,
            last_checks: Vec::new(),
            handoff: None,
            notes: Vec::new(),
            seconds: 0,
        }
    }
}

/// Which tests passed when the Run began. May not shrink.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Baseline {
    pub passed: usize,
    pub failed: usize,
    pub passed_names: BTreeSet<String>,
    /// The suite did not build or run at all. Nothing to protect yet — the
    /// usual case for a new Project.
    #[serde(default)]
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", content = "why", rename_all = "snake_case")]
pub enum Verdict {
    Done,
    /// A Task could not be finished within its Attempts, twice in a row, or
    /// every remaining Task is blocked.
    Blocked(String),
    /// The clock ran out.
    Ceiling(String),
    /// The Guardrail stopped it.
    Flagged(String),
    /// Jev (or a rule) decided a human is needed.
    Escalated(String),
    /// It could not start or could not go on: a dirty tree, no model.
    Failed(String),
}

impl Verdict {
    pub fn headline(&self) -> String {
        match self {
            Verdict::Done => "done — every Task's Checks pass".into(),
            Verdict::Blocked(w) => format!("blocked — {w}"),
            Verdict::Ceiling(w) => format!("stopped at a ceiling — {w}"),
            Verdict::Flagged(w) => format!("stopped by the guardrail — {w}"),
            Verdict::Escalated(w) => format!("needs you — {w}"),
            Verdict::Failed(w) => format!("could not run — {w}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    pub at: u64,
    /// `intent`, or a Task id.
    pub subject: String,
    pub kind: String,
    pub probability: Option<f64>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denied {
    pub at: u64,
    pub task: Option<String>,
    pub command: String,
    pub why: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub requests: usize,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
}

impl UsageTotals {
    pub fn add(&mut self, u: &smithy_agent::Usage) {
        self.requests += u.requests;
        self.prompt_tokens += u.prompt_tokens;
        self.completion_tokens += u.completion_tokens;
        self.cached_tokens += u.cached_tokens;
    }
}

/// The Attempt in flight, written before it starts. Still set on resume
/// means the process died inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub task: String,
    pub attempt: usize,
    pub since: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub id: String,
    pub intent: String,
    pub branch: String,
    /// The commit the Run branched from.
    pub base: String,
    pub started: u64,
    pub updated: u64,
    /// Seconds spent across every process that worked on this Run, so a
    /// resume does not reset the clock.
    pub elapsed: u64,
    pub ceilings: Ceilings,
    pub toolchain: Toolchain,
    pub preexisting_untracked: BTreeSet<String>,
    #[serde(default)]
    pub baseline: Option<Baseline>,
    #[serde(default)]
    pub planned: bool,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskState>,
    #[serde(default)]
    pub consecutive_blocked: usize,
    #[serde(default)]
    pub in_flight: Option<InFlight>,
    #[serde(default)]
    pub verdict: Option<Verdict>,
    #[serde(default)]
    pub flags: Vec<Flag>,
    /// Tasks (or `intent`) the user cleared after a Guardrail flag.
    #[serde(default)]
    pub allowed: BTreeSet<String>,
    #[serde(default)]
    pub denied: Vec<Denied>,
    #[serde(default)]
    pub stashes: Vec<String>,
    #[serde(default)]
    pub usage: UsageTotals,
    #[serde(default)]
    pub notes: Vec<NoteRecord>,
    /// Seconds of research spent so far, against the Run's research budget.
    #[serde(default)]
    pub research_seconds: u64,
    /// Where this Run's events and transcripts are kept, if anywhere.
    #[serde(default)]
    pub log_dir: Option<String>,
}

/// A research Note this Run wrote or reused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteRecord {
    pub path: String,
    pub question: String,
    pub task: Option<String>,
    pub verified: usize,
    pub findings: usize,
    pub answered: Option<f64>,
    /// Jev's probability, before the research, that the question needed
    /// outside sources. Reported, not acted on.
    #[serde(default)]
    pub need: Option<f64>,
    /// Found on file instead of researched again.
    #[serde(default)]
    pub reused: bool,
}

impl RunState {
    pub fn new(id: &str, intent: &str, branch: &str, base: &str, toolchain: Toolchain) -> RunState {
        let now = unix_now();
        RunState {
            id: id.into(),
            intent: intent.into(),
            branch: branch.into(),
            base: base.into(),
            started: now,
            updated: now,
            elapsed: 0,
            ceilings: Ceilings::default(),
            toolchain,
            preexisting_untracked: BTreeSet::new(),
            baseline: None,
            planned: false,
            tasks: BTreeMap::new(),
            consecutive_blocked: 0,
            in_flight: None,
            verdict: None,
            flags: Vec::new(),
            allowed: BTreeSet::new(),
            denied: Vec::new(),
            stashes: Vec::new(),
            usage: UsageTotals::default(),
            notes: Vec::new(),
            research_seconds: 0,
            log_dir: None,
        }
    }

    pub fn task(&mut self, id: &str) -> &mut TaskState {
        self.tasks.entry(id.to_string()).or_default()
    }

    pub fn done(&self) -> BTreeSet<String> {
        self.tasks
            .iter()
            .filter(|(_, t)| t.status == TaskStatus::Done)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn load(path: &Path) -> Result<RunState, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read {}: {e}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a Run state: {e}", path.display()))
    }

    /// Written to a sibling and renamed, so a crash mid-write leaves the old
    /// state rather than half of a new one.
    pub fn save(&mut self, path: &Path) -> Result<(), String> {
        self.updated = unix_now();
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_atomic(path, &text)
    }
}

/// `.smithy/runs/<id>/` under `root`.
pub fn run_dir(root: &Path, id: &str) -> PathBuf {
    root.join(".smithy").join("runs").join(id)
}

pub fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let text = match record_root(path) {
        Some(root) => scrub_paths(text, &root),
        None => text.to_string(),
    };
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

/// The Project root of a file in `<root>/.smithy/runs/<id>/`, if it is one.
pub fn record_root(path: &Path) -> Option<PathBuf> {
    let mut up = path.ancestors();
    let (_, _, runs, smithy, root) = (up.next()?, up.next()?, up.next()?, up.next()?, up.next()?);
    (runs.file_name()? == "runs" && smithy.file_name()? == ".smithy").then(|| root.to_path_buf())
}

/// A Run's records are committed to its branch, which may be pushed, so the
/// paths in them name the Project as `.` and the home directory as `~`
/// rather than with a user name. The hebrew-calendar Run's records carried
/// both a Windows and a Linux user name in some 3,000 lines.
pub fn scrub_paths(text: &str, root: &Path) -> String {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    scrub_paths_with(text, root, home.as_deref())
}

fn scrub_paths_with(text: &str, root: &Path, home: Option<&Path>) -> String {
    let mut out = text.to_string();
    for (path, with) in [(Some(root), "."), (home, "~")] {
        let Some(path) = path else { continue };
        for form in path_spellings(path) {
            // Never a bare drive or `/`: that would rewrite every path.
            if form.trim_matches(['/', '\\', ':']).len() > 2 {
                out = out.replace(&form, with);
            }
        }
    }
    out
}

/// The ways a path is written in logs and commands: as the OS writes it,
/// with forward slashes, as Git Bash writes a drive (`/c/Users/…`), with
/// either case of drive letter, and with backslashes escaped for JSON.
/// Longest first, so the JSON form is replaced before its unescaped part.
fn path_spellings(path: &Path) -> Vec<String> {
    let native = path.to_string_lossy();
    let native = native.strip_prefix(r"\\?\").unwrap_or(&native);
    let slashed = native.trim_end_matches(['/', '\\']).replace('\\', "/");
    let mut forward = vec![slashed.clone()];
    if let Some((drive, rest)) = slashed.split_once(":/") {
        if drive.len() == 1 {
            forward.push(format!("{}:/{rest}", drive.to_ascii_lowercase()));
            forward.push(format!("{}:/{rest}", drive.to_ascii_uppercase()));
            forward.push(format!("/{}/{rest}", drive.to_ascii_lowercase()));
        }
    }
    let mut forms = Vec::new();
    for f in forward {
        let backslashed = if f.starts_with('/') {
            f.clone()
        } else {
            f.replace('/', "\\")
        };
        forms.push(backslashed.replace('\\', "\\\\"));
        forms.push(backslashed);
        forms.push(f);
    }
    forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
    forms.dedup();
    forms
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `2026-09-23 04:38 UTC`.
pub fn utc_minute(unix: u64) -> String {
    let (y, m, d) = civil_from_days((unix / 86_400) as i64);
    let secs = unix % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// `2026-09-23`.
pub fn utc_date(unix: u64) -> String {
    let (y, m, d) = civil_from_days((unix / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-civil, for the proleptic Gregorian calendar.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `3h 04m`, `12m`, `40s`.
pub fn human_duration(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(utc_date(0), "1970-01-01");
        // 2026-09-23 00:00:00 UTC
        assert_eq!(
            utc_minute(1_790_121_600 + 4 * 3600 + 38 * 60),
            "2026-09-23 04:38 UTC"
        );
        assert_eq!(utc_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn state_round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = run_dir(tmp.path(), "r1").join("state.json");
        let mut s = RunState::new("r1", "build it", "smithy/run-r1", "abc", Toolchain::rust());
        s.task("T1").status = TaskStatus::Done;
        s.task("T2").attempts = 2;
        s.verdict = Some(Verdict::Blocked("T2 would not build".into()));
        s.save(&path).unwrap();
        let back = RunState::load(&path).unwrap();
        assert_eq!(back.tasks["T2"].attempts, 2);
        assert_eq!(back.done(), BTreeSet::from(["T1".to_string()]));
        assert_eq!(back.verdict, s.verdict);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn durations_read_like_a_person_wrote_them() {
        assert_eq!(human_duration(40), "40s");
        assert_eq!(human_duration(720), "12m");
        assert_eq!(human_duration(3 * 3600 + 4 * 60), "3h 04m");
    }

    #[test]
    fn records_name_the_project_and_home_without_a_user_name() {
        let root = Path::new(r"C:\Users\someone\code\calendar");
        let home = Path::new(r"C:\Users\someone");
        let text = concat!(
            r#"{"cmd": "cd /c/Users/someone/code/calendar && cat > /c/Users/someone/AppData/Local/Temp/x.py", "#,
            r#""log_dir": "C:\\Users\\someone\\.local\\share", "#,
            r#""note": "wrote c:/Users/someone/code/calendar/src/lib.rs and C:\Users\someone\n.txt"}"#
        );
        let out = scrub_paths_with(text, root, Some(home));
        assert!(!out.contains("someone"), "{out}");
        assert!(
            out.contains("cd . && cat > ~/AppData/Local/Temp/x.py"),
            "{out}"
        );
        assert!(out.contains(r#""log_dir": "~\\.local\\share""#), "{out}");
        assert!(out.contains(r"wrote ./src/lib.rs and ~\n.txt"), "{out}");
    }

    #[test]
    fn linux_paths_and_the_record_root() {
        let out = scrub_paths_with(
            "Compiling core (/home/someone/code/cal/core); logs in /home/someone/.local/share",
            Path::new("/home/someone/code/cal"),
            Some(Path::new("/home/someone")),
        );
        assert_eq!(out, "Compiling core (./core); logs in ~/.local/share");
        assert_eq!(
            record_root(Path::new("/p/.smithy/runs/r1/REPORT.md")),
            Some(PathBuf::from("/p"))
        );
        assert_eq!(record_root(Path::new("/p/notes/r1/REPORT.md")), None);
    }

    #[test]
    fn a_short_home_is_left_alone() {
        let out = scrub_paths_with("C:/ and /", Path::new("/x/y/z"), Some(Path::new("C:/")));
        assert_eq!(out, "C:/ and /");
    }
}
