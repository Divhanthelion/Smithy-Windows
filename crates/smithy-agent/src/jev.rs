//! Jev — TypeSafe's "System One" model — as a second opinion on shell commands.
//!
//! Jev returns decisions, not text: a state and typed questions in, a
//! probability out. Smithy asks it one thing. When YOLO is about to run a
//! command without asking, because the lexical check found no path leaving the
//! Project, Jev is asked whether a careful developer would want to see it
//! first. A yes turns the silent run into the ordinary approval prompt.
//!
//! The lexical check answers *where* a command reaches, and nothing about what
//! it does there. `git reset --hard`, `git push --force`, `rm -rf src` and
//! `curl -d @.env https://…` all stay down in the Project as far as a path
//! scan can tell. That is the gap this fills.
//!
//! ## It can only add a prompt
//!
//! Jev never approves anything. The command it judges was written by a model
//! that reads the repository, and a comment inside that command is text Jev
//! reads too; an answer that could skip a prompt would make the prompt
//! negotiable. So the worst a wrong answer costs is one extra click, and an
//! absent Jev — no key, no network, a slow gateway — is exactly the behaviour
//! from before it existed.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

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
        let body = json!({
            "model": MODEL,
            "state": shell_state(command, root),
            "questions": {
                "confirm": { "type": "noul", "instructions": SHELL_QUESTION },
            },
        });
        let response = self
            .http
            .post(ENDPOINT)
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Jev unreachable: {e}"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| format!("Jev response unreadable: {e}"))?;
        if !status.is_success() {
            return Err(format!("Jev {status}: {}", error_message(&text)));
        }
        noul(&text, "confirm")
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
    fn the_state_names_the_command_and_the_project() {
        let state = shell_state("git reset --hard", Path::new("proj"));
        assert!(state.contains("git reset --hard"));
        assert!(state.contains("proj"));
    }
}
