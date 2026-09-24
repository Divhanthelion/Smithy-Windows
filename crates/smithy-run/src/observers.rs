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
