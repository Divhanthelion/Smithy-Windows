//! Watching a research Session for the one failure that loses its work.
//!
//! The second real Run's research turn read eleven primary sources for half
//! an hour — ISO's own sample PDFs, BIPM, ITU-R — and hit its time limit
//! without writing a line of the note. Everything it learned was in a
//! history that ended with the turn. The prompt now says to draft early;
//! this makes sure: past a number of tool calls with no note on disk, the
//! model is told once to write what it has.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use smithy_agent::observe::{StepObserver, TurnView, Verdict};

/// Tool calls a research Session may make before its note must exist.
pub const DRAFT_BY_STEP: usize = 20;

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

#[cfg(test)]
mod tests {
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
