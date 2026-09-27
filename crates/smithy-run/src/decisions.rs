//! Every judgment a Run made, with what it was shown.
//!
//! `.smithy/runs/<id>/decisions.jsonl`, append-only. A line records the
//! question, the exact state it was asked about, the answer, the threshold
//! and what the runner did. When the Run later learns whether that was right
//! — a nudge that was followed by passing Checks, a "done" that was not — an
//! outcome line points back at it by sequence number. Nothing is rewritten.
//!
//! This is the calibration set that grows by itself: the `jev` example
//! replays a log, and a changed threshold can be tried against real runs
//! before it is shipped.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::unix_now;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Probability(f64),
    Choice {
        pick: String,
        confidence: f64,
        probabilities: BTreeMap<String, f64>,
    },
    /// No answer — no key, the gateway down — and the fallback that applied.
    Unavailable(String),
    /// Decided by a rule, before or instead of Jev.
    Rule(String),
}

impl Answer {
    pub fn short(&self) -> String {
        match self {
            Answer::Probability(p) => format!("{p:.2}"),
            Answer::Choice {
                pick, confidence, ..
            } => format!("{pick} ({confidence:.2})"),
            Answer::Unavailable(e) => format!("unavailable: {e}"),
            Answer::Rule(r) => format!("rule: {r}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub seq: usize,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<usize>,
    /// `guardrail`, `research`, `next`, `cheat`, `answered`, `loop`, `done`.
    pub kind: String,
    /// What Jev was shown, verbatim, so the case can be replayed.
    pub state: String,
    pub answer: Answer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    /// What the runner did about it.
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub outcome_for: usize,
    pub at: u64,
    /// `Some(true)` when the decision turned out right, `None` when the Run
    /// learned something but cannot say either way.
    pub right: Option<bool>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
enum Line {
    Decision(Decision),
    Outcome(Outcome),
}

pub struct DecisionLog {
    path: PathBuf,
    next: usize,
}

/// A decision about to be written; `seq` and `at` are filled in by the log.
pub struct Draft<'a> {
    pub task: Option<&'a str>,
    pub attempt: Option<usize>,
    pub kind: &'a str,
    pub state: &'a str,
    pub answer: Answer,
    pub threshold: Option<f64>,
    pub action: &'a str,
}

impl DecisionLog {
    /// Open for appending, continuing the sequence of whatever is there.
    pub fn open(path: &Path) -> DecisionLog {
        let next = read(path).0.iter().map(|d| d.seq + 1).max().unwrap_or(0);
        DecisionLog {
            path: path.to_path_buf(),
            next,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn record(&mut self, draft: Draft<'_>) -> usize {
        let seq = self.next;
        self.next += 1;
        self.append(&Line::Decision(Decision {
            seq,
            at: unix_now(),
            task: draft.task.map(str::to_string),
            attempt: draft.attempt,
            kind: draft.kind.to_string(),
            state: draft.state.to_string(),
            answer: draft.answer,
            threshold: draft.threshold,
            action: draft.action.to_string(),
        }));
        seq
    }

    pub fn outcome(&mut self, seq: usize, right: Option<bool>, note: &str) {
        self.append(&Line::Outcome(Outcome {
            outcome_for: seq,
            at: unix_now(),
            right,
            note: note.to_string(),
        }));
    }

    /// Best effort: a log line that cannot be written is not worth stopping a
    /// Run over, but it is worth saying so.
    fn append(&self, line: &Line) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            // A line torn by a crash has no newline; without this the next
            // line would be glued to it and lost as well.
            let torn = std::fs::read(&self.path)
                .map(|b| b.last().is_some_and(|&c| c != b'\n'))
                .unwrap_or(false);
            let json = serde_json::to_string(line).map_err(std::io::Error::other)?;
            let json = match crate::state::record_root(&self.path) {
                Some(root) => crate::state::scrub_paths(&json, &root),
                None => json,
            };
            if torn {
                writeln!(f)?;
            }
            writeln!(f, "{json}")
        })();
        if let Err(e) = result {
            eprintln!(
                "[run] could not log a decision to {}: {e}",
                self.path.display()
            );
        }
    }
}

/// Every decision in a log, and the outcomes learned for them by `seq`.
/// Unreadable lines are skipped: one torn write must not hide the rest.
pub fn read(path: &Path) -> (Vec<Decision>, BTreeMap<usize, Vec<Outcome>>) {
    let mut decisions = Vec::new();
    let mut outcomes: BTreeMap<usize, Vec<Outcome>> = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return (decisions, outcomes);
    };
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Line>(line) {
            Ok(Line::Decision(d)) => decisions.push(d),
            Ok(Line::Outcome(o)) => outcomes.entry(o.outcome_for).or_default().push(o),
            Err(_) => {}
        }
    }
    (decisions, outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft<'a>(kind: &'a str, answer: Answer) -> Draft<'a> {
        Draft {
            task: Some("T1"),
            attempt: Some(1),
            kind,
            state: "An AI coding agent …",
            answer,
            threshold: Some(0.5),
            action: "continue",
        }
    }

    #[test]
    fn decisions_and_their_outcomes_come_back_together() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("runs/r1/decisions.jsonl");
        let mut log = DecisionLog::open(&path);
        let a = log.record(draft("guardrail", Answer::Probability(0.02)));
        let b = log.record(draft(
            "next",
            Answer::Choice {
                pick: "handoff".into(),
                confidence: 0.4,
                probabilities: BTreeMap::from([("handoff".into(), 0.47), ("escalate".into(), 0.5)]),
            },
        ));
        log.outcome(b, Some(true), "next attempt passed its checks");

        let (decisions, outcomes) = read(&path);
        assert_eq!(
            decisions.iter().map(|d| d.seq).collect::<Vec<_>>(),
            vec![a, b]
        );
        assert_eq!(outcomes[&b][0].right, Some(true));
        assert!(!outcomes.contains_key(&a));
    }

    #[test]
    fn a_reopened_log_continues_the_sequence_and_skips_torn_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("decisions.jsonl");
        let mut log = DecisionLog::open(&path);
        log.record(draft("loop", Answer::Probability(0.1)));
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| write!(f, "{{\"line\":\"decis"))
            .unwrap();
        let mut again = DecisionLog::open(&path);
        assert_eq!(
            again.record(draft("done", Answer::Unavailable("429".into()))),
            1
        );
        assert_eq!(read(&path).0.len(), 2);
    }
}
