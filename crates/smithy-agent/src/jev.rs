//! Jev — TypeSafe's "System One" model — as the loop's reflexes.
//!
//! Jev returns decisions, not text: a state and typed questions in, a
//! probability out, in about half a second. Smithy asks it three things:
//!
//! - **Is this command worth a look?** When YOLO is about to run a command
//!   without asking, because the lexical checks found no path leaving the
//!   Project and no network, Jev is asked whether a careful developer would
//!   want to see it first. A yes turns the silent run into the approval prompt.
//! - **Is the agent going in circles?** After each step, from the fourth on.
//!   A yes gets a nudge; a second yes in the same turn ends it. See [`Supervisor`].
//! - **Is it actually done?** Before a turn ends with an answer. A no sends the
//!   model back to work, once.
//!
//! The thresholds are measured, not guessed: `cargo run -p smithy-agent
//! --example jev` scores known cases for all three and reports the misses.
//!
//! The lexical check answers *where* a command reaches, and nothing about what
//! it does there. `git reset --hard`, `git push --force`, `rm -rf src` and
//! `curl -d @.env https://…` all stay down in the Project as far as a path
//! scan can tell. That is the gap this fills.
//!
//! ## It can only add a prompt
//!
//! For the shell check: Jev never approves anything. The command it judges was written by a model
//! that reads the repository, and a comment inside that command is text Jev
//! reads too; an answer that could skip a prompt would make the prompt
//! negotiable. So the worst a wrong answer costs is one extra click, and an
//! absent Jev — no key, no network, a slow gateway — is exactly the behaviour
//! from before it existed.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::observe::{StepObserver, StepRecord, TurnView, Verdict};

/// Credential-store account for the Vercel AI Gateway key. `AI_GATEWAY_API_KEY`
/// in the environment is the fallback, as for every other key.
pub const AI_GATEWAY_KEY: &str = "ai-gateway-api-key";

const ENDPOINT: &str = "https://ai-gateway.vercel.sh/typesafe/v1/systemone";
const MODEL: &str = "typesafe-ai/jev";

/// Jev answers in tens of milliseconds. Past this the gateway is having a bad
/// day, and holding a turn for a check that can only add a prompt is not
/// worth it — the command falls back to what YOLO would have done anyway.
const TIMEOUT: Duration = Duration::from_secs(5);

/// At or above this, a command YOLO would have run silently is asked about.
pub const SHELL_RISK_THRESHOLD: f64 = 0.5;

/// The question, and the only place its wording lives. Concrete on both sides:
/// a bare "is this dangerous?" gets every `cargo build` a nervous 0.3.
const SHELL_QUESTION: &str = "Would a careful developer want to confirm this command before an AI \
agent runs it unattended? Yes if it deletes or overwrites files in bulk; discards uncommitted work \
or rewrites git history (reset --hard, clean -fd, checkout -- ., push --force, rebase, branch -D); \
sends files, environment variables or secrets over the network; installs or removes software \
outside the project; changes permissions, credentials or system settings; or kills processes. No \
for builds, tests, formatters, linters, searches, listings, reads, ordinary git status/diff/log/add/\
commit, and running the project's own scripts.";

#[cfg(windows)]
const SHELL: &str = "Git Bash (POSIX shell on Windows)";
#[cfg(not(windows))]
const SHELL: &str = "sh";

pub struct Jev {
    http: reqwest::Client,
    key: String,
}

impl Jev {
    /// `None` when no key is stored or set — the ordinary case, and not an error.
    pub fn from_store() -> Option<Jev> {
        let key = crate::config::api_key(AI_GATEWAY_KEY, "AI_GATEWAY_API_KEY")?;
        Jev::new(key).ok()
    }

    pub fn new(key: String) -> Result<Jev, String> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| format!("could not build HTTP client: {e}"))?;
        Ok(Jev { http, key })
    }

    /// Probability, 0 to 1, that this command deserves a human look first.
    pub async fn shell_risk(&self, command: &str, root: &Path) -> Result<f64, String> {
        self.ask_noul(shell_state(command, root), SHELL_QUESTION).await
    }

    /// Probability that the agent is going in circles.
    pub async fn loop_risk(&self, request: &str, steps: &[StepRecord]) -> Result<f64, String> {
        self.ask_noul(loop_state(request, steps), LOOP_QUESTION).await
    }

    /// Probability that the turn has done what was asked.
    pub async fn completion(
        &self,
        request: &str,
        steps: &[StepRecord],
        answer: &str,
    ) -> Result<f64, String> {
        self.ask_noul(done_state(request, steps, answer), DONE_QUESTION).await
    }

    /// One `noul` question, retried once if the gateway had a bad moment.
    ///
    /// Once, not with backoff: every caller has somewhere to fall back to, and
    /// a check that holds a turn for a minute has stopped being cheap.
    async fn ask_noul(&self, state: String, instructions: &str) -> Result<f64, String> {
        let body = json!({
            "model": MODEL,
            "state": state,
            "questions": { "q": { "type": "noul", "instructions": instructions } },
        });
        match self.post(&body).await {
            Err(Failure::Transient(_)) => {
                tokio::time::sleep(Duration::from_millis(400)).await;
                self.post(&body).await
            }
            other => other,
        }
        .map_err(|(Failure::Transient(e) | Failure::Final(e))| e)
        .and_then(|text| noul(&text, "q"))
    }

    async fn post(&self, body: &Value) -> Result<String, Failure> {
        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.key)
            .json(body)
            .send()
            .await
            .map_err(|e| Failure::Transient(format!("Jev unreachable: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| Failure::Transient(format!("Jev response unreadable: {e}")))?;
        if status.is_success() {
            return Ok(text);
        }
        let message = format!("Jev {status}: {}", error_message(&text));
        Err(if status.is_server_error() || status.as_u16() == 429 {
            Failure::Transient(message)
        } else {
            Failure::Final(message)
        })
    }
}

enum Failure {
    Transient(String),
    Final(String),
}

// ---------------------------------------------------------------------------
// Supervision: loops and early stops
// ---------------------------------------------------------------------------

/// Before this many steps a loop is indistinguishable from getting oriented.
const LOOP_MIN_STEPS: usize = 4;
/// How far back the loop check looks. A loop is recent by definition.
const LOOP_WINDOW: usize = 8;
/// A nudge on a false alarm costs a paragraph of the model's attention; being
/// stopped costs the turn. High, because the step ceiling is still behind it.
/// Measured: loops 0.93–0.98, productive sequences 0.06–0.09.
const LOOP_THRESHOLD: f64 = 0.85;
/// Below this the answer is sent back once. Measured with the `jev` example:
/// finished turns scored 0.81–0.94 and turns that quit partway 0.04–0.28, so
/// the middle of that gap. 0.2 was the first guess, and let a half-done rename
/// (0.28) through.
const DONE_THRESHOLD: f64 = 0.5;

const LOOP_QUESTION: &str = "Is the agent stuck? Yes if it is repeating the same or nearly the \
same actions, re-reading or re-checking things it already has, or retrying a failing approach \
without changing it, instead of making progress on the request. No if each action gathers new \
information or changes something, even when the actions are similar in kind (reading several \
different files, fixing a series of different errors).";

const DONE_QUESTION: &str = "Has the agent finished what the user asked for? Yes if it did the \
work, answered the question, or explains a genuine blocker it cannot get past. No if it stopped \
partway, only described or planned what it would do, or asked for permission or confirmation it \
did not need.";

const LOOP_NUDGE: &str = "You appear to be repeating yourself without making progress. Stop and \
change approach: say what you have learned, what is blocking you, and try something different — \
or report the blocker if you cannot get past it.";

const DONE_NUDGE: &str = "The request does not look finished yet. Continue until it is done. If \
something genuinely blocks you, say exactly what, instead of stopping partway or asking for \
permission you do not need.";

/// Jev as a [`StepObserver`]: nudges a looping agent, stops one that keeps
/// looping, and sends a half-done answer back once.
///
/// Every failure is silent and means "carry on", for the same reason as the
/// shell check: the loop's own ceilings are still behind this, so a Jev that
/// is down only means the turn runs as it did before Jev existed.
pub struct Supervisor {
    jev: Arc<Jev>,
    /// Loop flags so far, keyed by the turn they belong to.
    flags: Mutex<(usize, usize)>,
}

impl Supervisor {
    pub fn new(jev: Arc<Jev>) -> Supervisor {
        Supervisor {
            jev,
            flags: Mutex::new((usize::MAX, 0)),
        }
    }

    /// A supervisor for a new Session, when a key is available.
    pub fn from_store() -> Option<Arc<dyn StepObserver>> {
        let jev = Arc::new(Jev::from_store()?);
        Some(Arc::new(Supervisor::new(jev)))
    }
}

#[async_trait]
impl StepObserver for Supervisor {
    fn name(&self) -> &'static str {
        "jev"
    }

    async fn after_step(&self, turn: &TurnView<'_>) -> Verdict {
        if turn.step < LOOP_MIN_STEPS {
            return Verdict::Continue;
        }
        let steps = turn.steps();
        let recent = &steps[steps.len().saturating_sub(LOOP_WINDOW)..];
        let risk = match self.jev.loop_risk(turn.request(), recent).await {
            Ok(risk) => risk,
            Err(e) => {
                jev_debug(&format!("no loop check at step {}: {e}", turn.step));
                return Verdict::Continue;
            }
        };
        jev_debug(&format!("loop {risk:.3} at step {}", turn.step));
        if risk < LOOP_THRESHOLD {
            return Verdict::Continue;
        }
        let flagged = {
            let mut flags = self.flags.lock().unwrap_or_else(|e| e.into_inner());
            if flags.0 != turn.turn_start {
                *flags = (turn.turn_start, 0);
            }
            flags.1 += 1;
            flags.1
        };
        if flagged == 1 {
            Verdict::Nudge(LOOP_NUDGE.to_string())
        } else {
            Verdict::Stop(format!(
                "still repeating itself after a nudge ({:.0}% sure it is stuck)",
                risk * 100.0
            ))
        }
    }

    async fn before_answer(&self, turn: &TurnView<'_>, answer: &str) -> Verdict {
        let done = match self.jev.completion(turn.request(), &turn.steps(), answer).await {
            Ok(done) => done,
            Err(e) => {
                jev_debug(&format!("no completion check: {e}"));
                return Verdict::Continue;
            }
        };
        jev_debug(&format!("done {done:.3}"));
        if done < DONE_THRESHOLD {
            Verdict::Nudge(DONE_NUDGE.to_string())
        } else {
            Verdict::Continue
        }
    }
}

fn loop_state(request: &str, steps: &[StepRecord]) -> String {
    let mut state = format!(
        "An AI coding agent is working on a request.\n\nRequest:\n{}\n\nIts most recent actions, \
         oldest first:\n",
        clip(request, 1500)
    );
    for (i, step) in steps.iter().enumerate() {
        state.push_str(&format!(
            "{}. {} {} → {}\n",
            i + 1,
            step.name,
            clip(&step.arguments, 200),
            clip(&step.result, 240)
        ));
    }
    state
}

fn done_state(request: &str, steps: &[StepRecord], answer: &str) -> String {
    let mut state = format!(
        "An AI coding agent was given a request and has now stopped with a final message.\n\n\
         Request:\n{}\n\nActions it took ({} in all):\n",
        clip(request, 1500),
        steps.len()
    );
    if steps.is_empty() {
        state.push_str("(none)\n");
    }
    for step in &steps[steps.len().saturating_sub(30)..] {
        state.push_str(&format!("- {} {}\n", step.name, clip(&step.arguments, 120)));
    }
    state.push_str(&format!("\nFinal message:\n{}", clip(answer, 2000)));
    state
}

/// At most `max` characters, on one line, marked when cut.
fn clip(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Whether YOLO should ask after all, and the line that says why.
///
/// `None` means run it: Jev is absent, unreachable, or unconcerned. Failures
/// are silent by design — see the module note — except under
/// `SMITHY_JEV_DEBUG=1`, because a revoked key otherwise looks exactly like a
/// Jev that approves of everything.
pub async fn flags_shell(jev: Option<&Jev>, command: &str, root: &Path) -> Option<String> {
    let risk = match jev?.shell_risk(command, root).await {
        Ok(risk) => risk,
        Err(e) => {
            jev_debug(&format!("no second opinion for `{command}`: {e}"));
            return None;
        }
    };
    jev_debug(&format!("{risk:.3} for `{command}`"));
    (risk >= SHELL_RISK_THRESHOLD)
        .then(|| format!("Jev: {:.0}% likely worth a look before it runs", risk * 100.0))
}

fn jev_debug(message: &str) {
    if std::env::var("SMITHY_JEV_DEBUG").is_ok_and(|v| v != "0") {
        eprintln!("[jev] {message}");
    }
}

fn shell_state(command: &str, root: &Path) -> String {
    format!(
        "An AI coding agent wants to run a shell command, unattended, inside a software \
         project.\nProject root: {}\nShell: {SHELL}\nCommand:\n{command}",
        root.display()
    )
}

/// The probability for one `noul` question, from a `systemone` response.
fn noul(body: &str, question: &str) -> Result<f64, String> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|e| format!("Jev sent something unreadable: {e}"))?;
    value["answers"][question]["noul"]
        .as_f64()
        .filter(|p| (0.0..=1.0).contains(p))
        .ok_or_else(|| format!("Jev's answer has no probability for `{question}`"))
}

/// TypeSafe errors carry a `message`; anything else is shown as sent, clipped.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response shape from the gateway's documentation, verbatim but for
    /// the question name.
    #[test]
    fn a_probability_is_read_from_its_question() {
        let body = r#"{"model":"typesafe-ai/jev","answers":{"confirm":{"type":"noul","noul":0.98}},"usage":{"input_tokens":275,"output_tokens":20}}"#;
        assert_eq!(noul(body, "confirm"), Ok(0.98));
    }

    /// A missing or out-of-range answer is an error, not a zero — a zero would
    /// read as "run it", which is the one direction this must not fail in by
    /// accident rather than by design.
    #[test]
    fn a_malformed_answer_is_an_error_not_a_verdict() {
        assert!(noul(r#"{"answers":{}}"#, "confirm").is_err());
        assert!(noul(r#"{"answers":{"confirm":{"noul":1.7}}}"#, "confirm").is_err());
        assert!(noul("not json", "confirm").is_err());
    }

    #[test]
    fn a_typesafe_error_shows_its_message() {
        let body = r#"{"message":"questions.confirm.type: expected one of 'noul', 'choice', 'score'","error_type":"invalid_request"}"#;
        assert!(error_message(body).starts_with("questions.confirm.type"));
    }

    #[tokio::test]
    async fn without_a_key_nothing_is_flagged() {
        assert_eq!(flags_shell(None, "git push --force", Path::new(".")).await, None);
    }

    #[test]
    fn a_loop_state_numbers_recent_steps_on_one_line_each() {
        let steps = vec![StepRecord {
            name: "grep".into(),
            arguments: "{\"pattern\":\"fn\nmain\"}".into(),
            result: "a.rs:1\nb.rs:2".into(),
        }];
        let state = loop_state("find main", &steps);
        assert!(state.contains("1. grep {\"pattern\":\"fn main\"} → a.rs:1 b.rs:2"), "{state}");
    }

    #[test]
    fn clipping_is_marked_and_char_safe() {
        assert_eq!(clip("日本語テキスト", 3), "日本語…");
        assert_eq!(clip("short", 10), "short");
    }

    #[test]
    fn the_state_names_the_command_and_the_project() {
        let state = shell_state("git reset --hard", Path::new("proj"));
        assert!(state.contains("git reset --hard"));
        assert!(state.contains("proj"));
    }
}
