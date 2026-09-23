//! Watching a turn from outside the loop.
//!
//! [`smithy_tools::ToolHook`] sees one tool call before it runs. That is the
//! right seam for a gate and the wrong one for a supervisor: whether the agent
//! is going in circles, or has stopped before the work is done, is a property of
//! the *turn*, visible only after steps have happened. A [`StepObserver`] is
//! shown the turn so far at the two points where the loop can act on an answer
//! without breaking anything:
//!
//! - **after a step**, once every tool result is in history — the same point
//!   the step-limit warning is appended, where a new message cannot come
//!   between an assistant's `tool_calls` and their results;
//! - **before the turn ends** with an answer.
//!
//! What an observer can do is deliberately small. It can let the loop go on, add
//! a note the model will read, or end the turn. It cannot edit history, run a
//! tool, or approve anything a hook refused.

use async_trait::async_trait;

use crate::message::{History, Role};

/// What an observer wants the loop to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Continue,
    /// Append this for the model to read, then carry on.
    Nudge(String),
    /// End the turn, with this as the reason shown.
    Stop(String),
}

/// Nudges are machinery, not something the user said. The transcript hides
/// them by this prefix, as it does the loop's own retry notes.
pub const SUPERVISOR_PREFIX: &str = "[supervisor] ";

#[async_trait]
pub trait StepObserver: Send + Sync {
    fn name(&self) -> &'static str;

    /// After a step's tool results are all in history.
    async fn after_step(&self, _turn: &TurnView<'_>) -> Verdict {
        Verdict::Continue
    }

    /// When the model has answered and the turn is about to end. `Stop` is
    /// read as `Continue` here: the turn is ending anyway.
    async fn before_answer(&self, _turn: &TurnView<'_>, _answer: &str) -> Verdict {
        Verdict::Continue
    }
}

/// One tool call and what came back, reassembled from history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    pub name: String,
    pub arguments: String,
    pub result: String,
}

/// The current turn, as an observer is shown it.
pub struct TurnView<'a> {
    pub history: &'a History,
    /// Index of this turn's user message in `history`.
    pub turn_start: usize,
    /// Tool calls made so far this turn.
    pub step: usize,
}

impl TurnView<'_> {
    /// What the user asked for this turn.
    pub fn request(&self) -> &str {
        self.history
            .messages()
            .get(self.turn_start)
            .map(|m| m.content.as_str())
            .unwrap_or_default()
    }

    /// Every tool call this turn with its result, oldest first.
    pub fn steps(&self) -> Vec<StepRecord> {
        let mut out: Vec<(String, StepRecord)> = Vec::new();
        for message in &self.history.messages()[self.turn_start.min(self.history.len())..] {
            match message.role {
                Role::Assistant => {
                    for call in &message.tool_calls {
                        out.push((
                            call.id.clone(),
                            StepRecord {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                                result: String::new(),
                            },
                        ));
                    }
                }
                Role::Tool => {
                    let id = message.tool_call_id.as_deref().unwrap_or_default();
                    if let Some((_, step)) = out.iter_mut().find(|(sid, _)| sid == id) {
                        step.result = message.content.clone();
                    }
                }
                _ => {}
            }
        }
        out.into_iter().map(|(_, step)| step).collect()
    }
}
