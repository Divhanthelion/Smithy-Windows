//! What a Run did, kept for reviewing the process afterwards.
//!
//! The first real Run kept its decisions and its report, and nothing else:
//! the model's conversations ended with their Sessions, so the post-mortem
//! was reconstructed from the Supervisor's clipped step records. This keeps
//! the rest, at a level you choose:
//!
//! - `off` — nothing beyond the state, decisions and report on the branch.
//! - `events` — `events.jsonl`: one line per model request (how long, how
//!   many tokens, how many cached), per tool call (name, arguments, size of
//!   the result, how long), per Check, per Session, and every progress line.
//! - `full` — events, and every Session's whole conversation with its
//!   reasoning in `sessions/`, in the format the REPL saves (so
//!   `cargo run -p smithy-agent --example transcript -- show FILE` reads it).
//!
//! Outside the repository by default (`~/.local/share/smithy/runs/<project>/
//! <run>/`): transcripts are large, and they are for review, not history.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use smithy_agent::{Session, TurnEvent};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Events,
    #[default]
    Full,
}

impl LogLevel {
    pub fn parse(s: &str) -> Result<LogLevel, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" => Ok(LogLevel::Off),
            "events" => Ok(LogLevel::Events),
            "full" => Ok(LogLevel::Full),
            other => Err(format!("log level `{other}`: expected off, events or full")),
        }
    }
}

pub struct RunLog {
    level: LogLevel,
    dir: PathBuf,
    events: Mutex<()>,
    sessions: Mutex<usize>,
}

/// One Session as the log names it: `03-build-T2-a1`.
#[derive(Debug, Clone)]
pub struct SessionLabel(pub String);

impl RunLog {
    pub fn new(level: LogLevel, dir: impl Into<PathBuf>) -> RunLog {
        RunLog {
            level,
            dir: dir.into(),
            events: Mutex::new(()),
            sessions: Mutex::new(0),
        }
    }

    pub fn off() -> RunLog {
        RunLog::new(LogLevel::Off, PathBuf::new())
    }

    /// `~/.local/share/smithy/runs/<project dir name>/<run id>`.
    pub fn default_dir(project: &Path, run: &str) -> Option<PathBuf> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        let name = project
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        Some(home.join(".local/share/smithy/runs").join(name).join(run))
    }

    pub fn level(&self) -> LogLevel {
        self.level
    }

    pub fn dir(&self) -> Option<&Path> {
        (self.level != LogLevel::Off).then_some(self.dir.as_path())
    }

    /// One line of `events.jsonl`: `{"t": <unix ms>, "kind": …, …fields}`.
    pub fn event(&self, kind: &str, fields: Value) {
        if self.level == LogLevel::Off {
            return;
        }
        let mut line = json!({ "t": unix_ms(), "kind": kind });
        if let (Some(obj), Value::Object(extra)) = (line.as_object_mut(), fields) {
            obj.extend(extra);
        }
        let _guard = self.events.lock();
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&self.dir)?;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join("events.jsonl"))?;
            writeln!(f, "{line}")
        })();
        if let Err(e) = result {
            eprintln!(
                "[run] could not write the run log in {}: {e}",
                self.dir.display()
            );
        }
    }

    /// Name the next Session, and note that it started.
    pub fn session_started(&self, purpose: &str) -> SessionLabel {
        let n = {
            let mut n = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            *n += 1;
            *n
        };
        let label = SessionLabel(format!("{n:02}-{purpose}"));
        self.event("session", json!({ "session": label.0 }));
        label
    }

    /// A sink for one turn's events, turning them into log lines. Content and
    /// reasoning deltas are left out here: `full` keeps them in the
    /// transcript, whole, rather than in fragments.
    pub fn sink(self: &Arc<Self>, label: &SessionLabel) -> impl Fn(TurnEvent) + Send + Sync {
        let log = Arc::clone(self);
        let session = label.0.clone();
        let started: Mutex<HashMap<String, Instant>> = Mutex::new(HashMap::new());
        move |event: TurnEvent| match event {
            TurnEvent::Completed {
                step,
                millis,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                reasoning_tokens,
                finish_reason,
                tool_calls,
            } => log.event(
                "request",
                json!({
                    "session": session, "step": step, "ms": millis,
                    "prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens,
                    "cached_tokens": cached_tokens, "reasoning_tokens": reasoning_tokens,
                    "finish": finish_reason, "tool_calls": tool_calls,
                }),
            ),
            TurnEvent::ToolStarted {
                id,
                step,
                name,
                arguments,
            } => {
                if let Ok(mut s) = started.lock() {
                    s.insert(id.clone(), Instant::now());
                }
                log.event(
                    "tool",
                    json!({ "session": session, "step": step, "id": id, "name": name,
                            "args": clip(&arguments, 600) }),
                );
            }
            TurnEvent::ToolFinished {
                id,
                step,
                name,
                content,
                is_error,
            } => {
                let ms = started
                    .lock()
                    .ok()
                    .and_then(|mut s| s.remove(&id))
                    .map(|t| t.elapsed().as_millis() as u64);
                log.event(
                    "tool_done",
                    json!({ "session": session, "step": step, "id": id, "name": name,
                            "ms": ms, "error": is_error, "chars": content.chars().count(),
                            "head": clip(content.lines().next().unwrap_or(""), 200) }),
                );
            }
            TurnEvent::Warning(w) => log.event("warning", json!({ "session": session, "text": w })),
            TurnEvent::Content(_) | TurnEvent::Reasoning(_) => {}
        }
    }

    /// At `full`, the Session's whole conversation so far, replacing the
    /// last save of it: a crash leaves the latest complete turn.
    pub fn save_session(&self, label: &SessionLabel, session: &Session, root: &Path) {
        if self.level != LogLevel::Full {
            return;
        }
        let stored = smithy_agent::persist::StoredSession::from_history_with_reasoning(
            label.0.clone(),
            root,
            "run",
            session.history(),
            session.sampling(),
            session.limits(),
            session.reasoning().to_vec(),
            session.skill().map(str::to_string),
        )
        .with_tools(session.tools_schema().clone());
        let dir = self.dir.join("sessions");
        let result = std::fs::create_dir_all(&dir).and_then(|_| {
            let text = serde_json::to_string_pretty(&stored).map_err(std::io::Error::other)?;
            std::fs::write(dir.join(format!("{}.json", label.0)), text)
        });
        if let Err(e) = result {
            eprintln!("[run] could not save transcript {}: {e}", label.0);
        }
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(dir: &Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn levels_parse() {
        assert_eq!(LogLevel::parse("Full"), Ok(LogLevel::Full));
        assert_eq!(LogLevel::parse("events"), Ok(LogLevel::Events));
        assert_eq!(LogLevel::parse("off"), Ok(LogLevel::Off));
        assert!(LogLevel::parse("loud").is_err());
    }

    #[test]
    fn off_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let log = RunLog::new(LogLevel::Off, tmp.path().join("log"));
        log.event("progress", json!({"text": "x"}));
        assert!(!tmp.path().join("log").exists());
        assert!(log.dir().is_none());
    }

    #[test]
    fn a_turns_events_become_timed_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let log = Arc::new(RunLog::new(LogLevel::Events, tmp.path()));
        let label = log.session_started("build-T1-a1");
        assert_eq!(label.0, "01-build-T1-a1");
        let sink = log.sink(&label);
        sink(TurnEvent::Completed {
            step: 0,
            millis: 1234,
            prompt_tokens: 52_000,
            completion_tokens: 900,
            cached_tokens: 50_000,
            reasoning_tokens: 300,
            finish_reason: "tool_calls".into(),
            tool_calls: 1,
        });
        sink(TurnEvent::ToolStarted {
            id: "c1".into(),
            step: 1,
            name: "bash".into(),
            arguments: "{\"command\":\"cargo test\"}".into(),
        });
        sink(TurnEvent::ToolFinished {
            id: "c1".into(),
            step: 1,
            name: "bash".into(),
            content: "test result: ok. 12 passed\nmore".into(),
            is_error: false,
        });
        sink(TurnEvent::Reasoning("thinking".into()));

        let l = lines(tmp.path());
        let kinds: Vec<&str> = l.iter().map(|v| v["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["session", "request", "tool", "tool_done"]);
        assert_eq!(l[1]["cached_tokens"], 50_000);
        assert_eq!(l[1]["ms"], 1234);
        assert_eq!(l[3]["head"], "test result: ok. 12 passed");
        assert!(l[3]["ms"].is_u64(), "tool duration measured");
        assert!(l.iter().all(|v| v["t"].is_u64()));
    }

    #[test]
    fn the_default_dir_is_outside_the_project() {
        let dir =
            RunLog::default_dir(Path::new("C:/code/smithy-trial"), "20260924-1512-abcd").unwrap();
        let s = dir.to_string_lossy().replace('\\', "/");
        assert!(
            s.ends_with(".local/share/smithy/runs/smithy-trial/20260924-1512-abcd"),
            "{s}"
        );
    }
}
