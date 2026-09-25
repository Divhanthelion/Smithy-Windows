//! Watching a Session for the failures that cost a Run the most time.
//!
//! [`WriteTheNote`]: the second real Run's research turn read eleven primary
//! sources for half an hour — ISO's own sample PDFs, BIPM, ITU-R — and hit
//! its time limit without writing a line of the note. Everything it learned
//! was in a history that ended with the turn. The prompt now says to draft
//! early; this makes sure: past a number of tool calls with no note on disk,
//! the model is told once to write what it has.
//!
//! [`StopWhenGreen`]: the third real Run's T1 turn lasted 57 minutes before
//! the runner checked anything. The model wrote the whole grammar — later
//! Tasks' work — then polished it. The Task's checks are what done means, so
//! they are run as the model goes, and the turn ends the first time they
//! pass.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use smithy_agent::observe::{StepObserver, TurnView, Verdict};

use crate::check::{run_check, CheckSpec, CHECK_TIMEOUT};
use crate::runlog::RunLog;
use crate::toolchain::Toolchain;

/// Tool calls a research Session may make before its note must exist.
pub const DRAFT_BY_STEP: usize = 12;

pub struct WriteTheNote {
    pub note: PathBuf,
    pub by_step: usize,
    nudged: AtomicBool,
}

impl WriteTheNote {
    pub fn new(note: PathBuf, by_step: usize) -> WriteTheNote {
        WriteTheNote {
            note,
            by_step,
            nudged: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl StepObserver for WriteTheNote {
    fn name(&self) -> &'static str {
        "draft"
    }

    async fn after_step(&self, turn: &TurnView<'_>) -> Verdict {
        if turn.step < self.by_step
            || self.note.is_file()
            || self.nudged.swap(true, Ordering::SeqCst)
        {
            return Verdict::Continue;
        }
        Verdict::Nudge(format!(
            "You have made {} tool calls and the note does not exist yet. Write it now at `{}` \
             with what you have — findings so far, the frozen hypotheses, and what is still open \
             under Unknowns — then keep researching and update it. A turn that ends with no note \
             loses everything it read.",
            turn.step,
            self.note.display()
        ))
    }
}

/// Ends a research turn once its Note is done by the skill's own definition.
///
/// Every research session in the third real Run ran to its time limit — 8, 8
/// and 20 minutes — whether or not the Note was already finished. Done is
/// what the research skills already say it is, checked mechanically, with no
/// judgment in it: **Status** reads `verified`, `cite_check` passes, and
/// there is an Unknowns section and an Implication with something in it. Jev
/// still reports whether the Note answers the question afterwards, as before.
pub struct NoteIsDone {
    pub note: PathBuf,
    pub root: PathBuf,
    /// Where fetched pages are, to check quotes. Without it the Status and
    /// the sections are what is checked.
    pub sources: Option<smithy_tools::research::SourceStore>,
    /// The Note's text when it was last judged not done.
    last: std::sync::Mutex<String>,
}

impl NoteIsDone {
    pub fn new(
        note: PathBuf,
        root: PathBuf,
        sources: Option<smithy_tools::research::SourceStore>,
    ) -> NoteIsDone {
        NoteIsDone {
            note,
            root,
            sources,
            last: std::sync::Mutex::new(String::new()),
        }
    }

    /// Whether `text` is a finished Note.
    pub fn done(&self, text: &str) -> bool {
        let verified = text.lines().any(|l| {
            l.trim()
                .strip_prefix("**Status:**")
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("verified"))
        });
        let section = |name: &str| -> Option<String> {
            let mut lines = text
                .lines()
                .skip_while(|l| l.trim() != format!("## {name}"));
            lines.next()?;
            Some(
                lines
                    .take_while(|l| !l.starts_with("## "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        };
        let implication = section("Implication").is_some_and(|s| !s.trim().is_empty());
        if !verified || !implication || section("Unknowns").is_none() {
            return false;
        }
        match &self.sources {
            Some(store) => {
                let check = smithy_tools::research::check_note(text, store, &self.root);
                check.passes() && check.verified() > 0
            }
            None => true,
        }
    }
}

#[async_trait]
impl StepObserver for NoteIsDone {
    fn name(&self) -> &'static str {
        "note-done"
    }

    async fn after_step(&self, _turn: &TurnView<'_>) -> Verdict {
        let Ok(text) = std::fs::read_to_string(&self.note) else {
            return Verdict::Continue;
        };
        {
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if *last == text {
                return Verdict::Continue;
            }
            *last = text.clone();
        }
        if self.done(&text) {
            Verdict::Stop(
                "the Note is done: verified, cite_check passes, Implication written".into(),
            )
        } else {
            Verdict::Continue
        }
    }
}

/// Tool calls between the Task's checks, once the model has changed a file.
pub const CHECK_EVERY: usize = 6;

/// Tools that change files. `bash` can too, but a model that runs a command
/// is usually running the checks itself, which the next write will follow.
const WRITES: [&str; 2] = ["write", "edit"];

/// Ends a build turn the first time the Task's own checks pass.
///
/// Only the Task's checks, and silently: a failure is the model's work in
/// progress, not something to tell it. The runner still runs everything
/// after the turn — the full suite against the Baseline, the cheat rules —
/// so stopping early lands nothing the checks at the end would not.
pub struct StopWhenGreen {
    pub checks: Vec<CheckSpec>,
    pub toolchain: Toolchain,
    pub root: PathBuf,
    pub every: usize,
    pub log: Option<(Arc<RunLog>, String)>,
    /// The step at which the checks last ran (0 before they have).
    checked_at: AtomicUsize,
}

impl StopWhenGreen {
    pub fn new(
        checks: Vec<CheckSpec>,
        toolchain: Toolchain,
        root: PathBuf,
        every: usize,
    ) -> StopWhenGreen {
        StopWhenGreen {
            checks,
            toolchain,
            root,
            every,
            log: None,
            checked_at: AtomicUsize::new(0),
        }
    }

    /// Log each early check as a `check` event for this Task.
    pub fn with_log(mut self, log: Arc<RunLog>, task: &str) -> StopWhenGreen {
        self.log = Some((log, task.to_string()));
        self
    }

    /// Whether a file changed after the step the checks last ran at.
    fn wrote_since(&self, turn: &TurnView<'_>, step: usize) -> bool {
        turn.steps()
            .iter()
            .skip(step)
            .any(|s| WRITES.contains(&s.name.as_str()))
    }
}

#[async_trait]
impl StepObserver for StopWhenGreen {
    fn name(&self) -> &'static str {
        "green"
    }

    async fn after_step(&self, turn: &TurnView<'_>) -> Verdict {
        let last = self.checked_at.load(Ordering::SeqCst);
        if self.checks.is_empty() || turn.step < last + self.every || !self.wrote_since(turn, last)
        {
            return Verdict::Continue;
        }
        self.checked_at.store(turn.step, Ordering::SeqCst);
        let checks = self.checks.clone();
        let tc = self.toolchain.clone();
        let root = self.root.clone();
        let outcomes = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            for spec in &checks {
                let o = run_check(spec, &tc, &root, CHECK_TIMEOUT);
                let passed = o.passed;
                out.push(o);
                if !passed {
                    break;
                }
            }
            out
        })
        .await
        .unwrap_or_default();
        if let Some((log, task)) = &self.log {
            for o in &outcomes {
                log.event(
                    "check",
                    serde_json::json!({
                        "task": task, "run": o.spec.run, "passed": o.passed,
                        "verdict": o.verdict, "seconds": o.seconds, "early": turn.step,
                    }),
                );
            }
        }
        if outcomes.len() == self.checks.len() && outcomes.iter().all(|o| o.passed) {
            Verdict::Stop(format!(
                "the Task's checks pass (after {} tool calls)",
                turn.step
            ))
        } else {
            Verdict::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::CheckKind;
    use smithy_agent::{History, Message};
    use smithy_tools::{ToolCall, ToolResult};

    fn check(run: &str) -> CheckSpec {
        CheckSpec {
            kind: CheckKind::Build,
            run: run.into(),
            min_tests: None,
        }
    }

    /// A history whose turn made `calls`, each a tool by name.
    fn history_with(calls: &[&str]) -> History {
        let mut history = History::new("sys");
        history.push(Message::user("build it"));
        for (i, name) in calls.iter().enumerate() {
            let call = ToolCall {
                id: format!("c{i}"),
                name: name.to_string(),
                arguments: "{}".into(),
            };
            history.push(Message::assistant_with_calls("", vec![call.clone()]));
            history.push(Message::tool_result(&ToolResult::ok(&call, "ok")));
        }
        history
    }

    fn green(dir: &std::path::Path, run: &str) -> StopWhenGreen {
        StopWhenGreen::new(vec![check(run)], Toolchain::rust(), dir.to_path_buf(), 3)
    }

    #[tokio::test]
    async fn stops_once_the_checks_pass_after_a_write() {
        let tmp = tempfile::tempdir().unwrap();
        let g = green(tmp.path(), "true");
        let h = history_with(&["read", "write", "read"]);
        assert_eq!(
            g.after_step(&view(&h, 2)).await,
            Verdict::Continue,
            "too soon"
        );
        assert!(matches!(g.after_step(&view(&h, 3)).await, Verdict::Stop(r) if r.contains("pass")));
    }

    #[tokio::test]
    async fn no_write_no_check() {
        let tmp = tempfile::tempdir().unwrap();
        let g = green(tmp.path(), "true");
        let h = history_with(&["read", "grep", "bash", "read"]);
        assert_eq!(g.after_step(&view(&h, 4)).await, Verdict::Continue);
    }

    #[tokio::test]
    async fn failing_checks_say_nothing_and_wait_for_the_next_write() {
        let tmp = tempfile::tempdir().unwrap();
        let g = green(tmp.path(), "false");
        let h = history_with(&["edit", "read", "read"]);
        assert_eq!(g.after_step(&view(&h, 3)).await, Verdict::Continue);
        assert_eq!(g.checked_at.load(Ordering::SeqCst), 3);
        // Three more calls, none of them writes: not worth another run.
        let h = history_with(&["edit", "read", "read", "read", "bash", "read"]);
        assert_eq!(g.after_step(&view(&h, 6)).await, Verdict::Continue);
        assert_eq!(g.checked_at.load(Ordering::SeqCst), 3, "not rerun");
    }

    fn view(history: &History, step: usize) -> TurnView<'_> {
        TurnView {
            history,
            turn_start: 1,
            step,
        }
    }
}

#[cfg(test)]
mod note_tests {
    use super::*;
    use smithy_agent::{History, Message};

    fn view(history: &History, step: usize) -> TurnView<'_> {
        TurnView {
            history,
            turn_start: 1,
            step,
        }
    }

    #[tokio::test]
    async fn nudges_once_past_the_step_while_the_note_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let note = tmp.path().join("note.md");
        let mut history = History::new("sys");
        history.push(Message::user("research"));
        let w = WriteTheNote::new(note.clone(), 20);

        assert_eq!(w.after_step(&view(&history, 19)).await, Verdict::Continue);
        assert!(
            matches!(w.after_step(&view(&history, 20)).await, Verdict::Nudge(n) if n.contains("Write it now"))
        );
        assert_eq!(
            w.after_step(&view(&history, 30)).await,
            Verdict::Continue,
            "once"
        );
    }

    #[tokio::test]
    async fn a_note_on_disk_needs_no_nudge() {
        let tmp = tempfile::tempdir().unwrap();
        let note = tmp.path().join("note.md");
        std::fs::write(&note, "# draft").unwrap();
        let mut history = History::new("sys");
        history.push(Message::user("research"));
        let w = WriteTheNote::new(note, 20);
        assert_eq!(w.after_step(&view(&history, 25)).await, Verdict::Continue);
    }
}

#[cfg(test)]
mod done_tests {
    use super::*;
    use smithy_agent::{History, Message};
    use smithy_tools::research::SourceStore;

    const RFC: &str = "https://www.rfc-editor.org/rfc/rfc3339";
    const W3C: &str = "https://www.w3.org/TR/xmlschema-2/";
    const RFC_QUOTE: &str = "The smallest value used may also have a decimal fraction";
    const W3C_QUOTE: &str = "the seconds component may have a decimal fraction";

    /// A note whose first load-bearing finding quotes `quote` from the RFC; the
    /// second, from another domain, always holds. The skills' cite_check PASS
    /// needs load-bearing findings on two domains.
    fn note(status: &str, ids: &(String, String), quote: &str, implication: &str) -> String {
        format!(
            "# Which components may be fractional?\n\n**Status:** {status}\n\n## Findings\n\
             - [spec] (key) Only the smallest unit may be fractional — {RFC} {{src:{}}} \"{quote}\"\n\
             - [spec] (key) The same rule, restated — {W3C} {{src:{}}} \"{W3C_QUOTE}\"\n\n\
             ## Unknowns\nNothing searched beyond these two.\n\n## Implication\n{implication}\n",
            ids.0, ids.1
        )
    }

    async fn verdict(done: &NoteIsDone) -> Verdict {
        let mut history = History::new("sys");
        history.push(Message::user("research"));
        done.after_step(&TurnView {
            history: &history,
            turn_start: 1,
            step: 3,
        })
        .await
    }

    #[tokio::test]
    async fn a_verified_note_whose_quotes_check_ends_the_turn() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SourceStore::new(tmp.path().join("sources"));
        let page = |url: &str, text: &str| store.save(url, url, 200, "text/html", text).unwrap().id;
        let ids = (
            page(
                RFC,
                &format!("Durations: P[n]Y[n]M[n]DT[n]H[n]M[n]S. {RFC_QUOTE}."),
            ),
            page(W3C, &format!("In a duration, {W3C_QUOTE}, and no other.")),
        );
        let path = tmp.path().join("note.md");
        let done = NoteIsDone::new(path.clone(), tmp.path().to_path_buf(), Some(store.clone()));
        let write = |text: String| std::fs::write(&path, text).unwrap();
        let answer = "Fractions only on the last component.";

        assert_eq!(verdict(&done).await, Verdict::Continue, "no note yet");
        write(note("draft", &ids, RFC_QUOTE, answer));
        assert_eq!(verdict(&done).await, Verdict::Continue, "still a draft");
        write(note(
            "verified",
            &ids,
            "any component may be fractional",
            answer,
        ));
        assert_eq!(
            verdict(&done).await,
            Verdict::Continue,
            "a quote not on the page"
        );
        write(note("verified", &ids, RFC_QUOTE, ""));
        assert_eq!(verdict(&done).await, Verdict::Continue, "no implication");
        write(note("verified", &ids, RFC_QUOTE, answer));
        assert!(matches!(verdict(&done).await, Verdict::Stop(r) if r.contains("done")));
    }
}
