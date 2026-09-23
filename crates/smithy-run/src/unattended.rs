//! Hooks for a Session nobody is watching.
//!
//! In the editor and the terminal a hook that is unsure asks. During a Run
//! there is nobody to ask, so every question becomes a refusal the model
//! hears and the Report lists under "would have asked". What still runs
//! without a prompt is exactly what YOLO runs without one, minus git, which
//! belongs to the runner.

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use smithy_agent::jev::{flags_shell, Jev};
use smithy_tools::{yolo_skips_bash, HookDecision, ToolCall, ToolCtx, ToolHook};

use crate::git::model_may_run;
use crate::state::{unix_now, Denied};

/// What was refused, for the Report. Shared with the runner, which drains it
/// into the Run's state after every turn.
pub type DeniedLog = Arc<Mutex<Vec<Denied>>>;

/// The shell policy during a Run. Named `shell-approval` because that is what
/// it is — approval, given by rule — and the registry only runs `bash` under
/// such a hook.
pub struct UnattendedShell {
    pub jev: Option<Arc<Jev>>,
    pub denied: DeniedLog,
    pub task: Option<String>,
}

#[async_trait]
impl ToolHook for UnattendedShell {
    fn name(&self) -> &'static str {
        "shell-approval"
    }

    async fn before(&self, call: &ToolCall, args: &Value, ctx: &ToolCtx) -> HookDecision {
        if call.name != "bash" {
            return HookDecision::Allow;
        }
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match shell_verdict(&command, ctx.workspace.root(), self.jev.as_deref()).await {
            None => HookDecision::Allow,
            Some(why) => {
                if let Ok(mut d) = self.denied.lock() {
                    d.push(Denied {
                        at: unix_now(),
                        task: self.task.clone(),
                        command: command.clone(),
                        why: why.clone(),
                    });
                }
                HookDecision::Deny(format!(
                    "Not run: {why}. This is an unattended Run and nobody can approve it. Find \
                     another way that stays inside the Project, or leave it out and say so in \
                     your answer."
                ))
            }
        }
    }
}

/// `None` to run it; otherwise why not.
pub async fn shell_verdict(command: &str, root: &Path, jev: Option<&Jev>) -> Option<String> {
    if let Err(why) = model_may_run(command) {
        return Some(why);
    }
    if !yolo_skips_bash(command, root) {
        return Some("it reaches outside the Project or onto the network".into());
    }
    flags_shell(jev, command, root).await
}

/// Where the model may write during a Run.
pub struct UnattendedWrites {
    /// When set, writes are confined under this Project-relative prefix
    /// (a research Session writes its Note and nothing else).
    pub only_under: Option<String>,
    pub denied: DeniedLog,
    pub task: Option<String>,
}

/// The Run's own records. Only the runner writes them.
const RUN_RECORDS: &str = ".smithy/runs/";

#[async_trait]
impl ToolHook for UnattendedWrites {
    fn name(&self) -> &'static str {
        "unattended-writes"
    }

    async fn before(&self, call: &ToolCall, args: &Value, ctx: &ToolCtx) -> HookDecision {
        if !matches!(call.name.as_str(), "write" | "edit") {
            return HookDecision::Allow;
        }
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return HookDecision::Allow;
        };
        let rel = match ctx.workspace.relative(path) {
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            // Scratch and outside paths are the sandbox's business.
            Err(_) => return HookDecision::Allow,
        };
        let refusal = if rel.starts_with(RUN_RECORDS) {
            Some(format!(
                "`{rel}` is the Run's own record; only the runner writes it"
            ))
        } else {
            match &self.only_under {
                Some(prefix) if !rel.starts_with(prefix.as_str()) => Some(format!(
                    "this Session may only write under `{prefix}`, not `{rel}`"
                )),
                _ => None,
            }
        };
        match refusal {
            None => HookDecision::Allow,
            Some(why) => {
                if let Ok(mut d) = self.denied.lock() {
                    d.push(Denied {
                        at: unix_now(),
                        task: self.task.clone(),
                        command: format!("{} {rel}", call.name),
                        why: why.clone(),
                    });
                }
                HookDecision::Deny(format!("Not written: {why}."))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithy_tools::{Registry, Workspace};

    fn setup() -> (tempfile::TempDir, Arc<ToolCtx>, DeniedLog) {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = Arc::new(ToolCtx::new(Workspace::open(tmp.path()).unwrap()));
        (tmp, ctx, Arc::new(Mutex::new(Vec::new())))
    }

    #[tokio::test]
    async fn the_shell_runs_what_yolo_runs_and_refuses_git_and_escapes() {
        let (tmp, ctx, denied) = setup();
        let registry = Registry::core().with_hook(UnattendedShell {
            jev: None,
            denied: denied.clone(),
            task: Some("T1".into()),
        });
        let ok = registry
            .execute(
                &ToolCall::new("1", "bash", r#"{"command":"echo hi"}"#),
                &ctx,
            )
            .await;
        assert!(!ok.is_error, "{}", ok.content);
        assert!(ok.content.contains("hi"));

        for cmd in [
            "git commit -am wip",
            "cat ../../secrets.txt",
            "curl https://example.com",
        ] {
            let args = serde_json::json!({ "command": cmd }).to_string();
            let r = registry
                .execute(&ToolCall::new("2", "bash", &args), &ctx)
                .await;
            assert!(
                r.is_error && r.content.contains("unattended Run"),
                "{cmd}: {}",
                r.content
            );
        }
        let d = denied.lock().unwrap();
        assert_eq!(d.len(), 3);
        assert_eq!(d[0].task.as_deref(), Some("T1"));
        drop(tmp);
    }

    #[tokio::test]
    async fn writes_to_the_runs_records_or_outside_a_confinement_are_refused() {
        let (_tmp, ctx, denied) = setup();
        let research = Registry::core().with_hook(UnattendedWrites {
            only_under: Some(".smithy/research/".into()),
            denied: denied.clone(),
            task: None,
        });
        let note = research
            .execute(
                &ToolCall::new(
                    "1",
                    "write",
                    r##"{"path":".smithy/research/n.md","content":"# n"}"##,
                ),
                &ctx,
            )
            .await;
        assert!(!note.is_error, "{}", note.content);
        let code = research
            .execute(
                &ToolCall::new("2", "write", r#"{"path":"src/lib.rs","content":"x"}"#),
                &ctx,
            )
            .await;
        assert!(
            code.is_error && code.content.contains("only write under"),
            "{}",
            code.content
        );

        let build = Registry::core().with_hook(UnattendedWrites {
            only_under: None,
            denied: denied.clone(),
            task: None,
        });
        let plan = build
            .execute(
                &ToolCall::new(
                    "3",
                    "write",
                    r#"{"path":".smithy/runs/r1/plan.toml","content":"x"}"#,
                ),
                &ctx,
            )
            .await;
        assert!(
            plan.is_error && plan.content.contains("Run's own record"),
            "{}",
            plan.content
        );
        assert_eq!(denied.lock().unwrap().len(), 2);
    }
}
