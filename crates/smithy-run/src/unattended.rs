//! Hooks for a Session nobody is watching.
//!
//! In the editor and the terminal a hook that is unsure asks. During a Run
//! there is nobody to ask, so every question becomes a refusal the model
//! hears and the Report lists under "would have asked". What still runs
//! without a prompt is exactly what YOLO runs without one, minus git, which
//! belongs to the runner.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
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

/// Project-relative paths of files this Session created with `write`. Shared
/// by a Session's two hooks: [`UnattendedWrites`] records, and
/// [`UnattendedShell`] lets the model delete what it made. A file that existed
/// before the Session is never in it.
pub type Written = Arc<Mutex<std::collections::BTreeSet<String>>>;

/// The shell policy during a Run. Named `shell-approval` because that is what
/// it is — approval, given by rule — and the registry only runs `bash` under
/// such a hook.
pub struct UnattendedShell {
    pub jev: Option<Arc<Jev>>,
    pub denied: DeniedLog,
    pub task: Option<String>,
    pub written: Written,
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
        let written = self.written.lock().map(|w| w.clone()).unwrap_or_default();
        match shell_verdict_with(
            &command,
            ctx.workspace.root(),
            self.jev.as_deref(),
            &written,
        )
        .await
        {
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
    shell_verdict_with(command, root, jev, &Default::default()).await
}

/// [`shell_verdict`], knowing which files this Session created.
pub async fn shell_verdict_with(
    command: &str,
    root: &Path,
    jev: Option<&Jev>,
    written: &BTreeSet<String>,
) -> Option<String> {
    if let Err(why) = model_may_run(command) {
        return Some(why);
    }
    if !yolo_skips_bash(command, root) {
        return Some("it reaches outside the Project or onto the network".into());
    }
    // Deleting its own scratch is housekeeping, not a risk to ask about. In
    // the hebrew-calendar Run, Jev rated `rm` of a probe file the model had
    // just written at 68-80%, so it was refused four times, and the model was
    // stopped for repeating itself.
    if only_removes_own_files(command, root, written) {
        return None;
    }
    flags_shell(jev, command, root).await
}

/// Characters that make a command less plain than [`only_removes_own_files`]
/// will vouch for: pipes, redirection, substitution, globs, quoting.
const NOT_PLAIN: &[char] = &['|', '>', '<', '`', '$', '*', '?', '"', '\'', '\\'];

/// Whether `command` is nothing but moving about the Project (`cd`, `ls`,
/// `pwd`) and removing files this Session created or its scratch directory
/// holds. No recursion, no globs, no pipes or redirection, no quoting: anything
/// less plain goes to Jev. Checked after the lexical checks, never instead.
pub fn only_removes_own_files(command: &str, root: &Path, written: &BTreeSet<String>) -> bool {
    if command.contains(NOT_PLAIN) {
        return false;
    }
    let scratch = smithy_tools::scratch_dir_for(root);
    let mut cwd = root.to_path_buf();
    let mut removes = false;
    for segment in command.split([';', '\n']).flat_map(|s| s.split("&&")) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        let Some((&program, args)) = words.split_first() else {
            continue;
        };
        match program {
            "ls" | "pwd" => {}
            "cd" => match args {
                [dir] => match inside(&cwd.join(dir), root) {
                    Some(to) => cwd = to,
                    None => return false,
                },
                _ => return false,
            },
            "rm" => {
                let mut targets = 0;
                for arg in args {
                    if let Some(flags) = arg.strip_prefix('-') {
                        if flags.is_empty() || !flags.chars().all(|c| c == 'f' || c == 'v') {
                            return false;
                        }
                        continue;
                    }
                    let path = normalize(&cwd.join(arg));
                    let own = path.starts_with(&scratch)
                        || path
                            .strip_prefix(root)
                            .ok()
                            .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                            .is_some_and(|rel| written.contains(&rel));
                    if !own {
                        return false;
                    }
                    targets += 1;
                }
                if targets == 0 {
                    return false;
                }
                removes = true;
            }
            _ => return false,
        }
    }
    removes
}

/// `path` with `.` and `..` resolved textually.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path`, normalized, if it is inside `root`.
fn inside(path: &Path, root: &Path) -> Option<PathBuf> {
    let p = normalize(path);
    p.starts_with(root).then_some(p)
}

/// Where the model may write during a Run.
pub struct UnattendedWrites {
    /// When set, writes are confined under this Project-relative prefix
    /// (a research Session writes its Note and nothing else).
    pub only_under: Option<String>,
    pub denied: DeniedLog,
    pub task: Option<String>,
    /// Where files this Session creates are recorded (see [`Written`]).
    pub written: Written,
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
            None => {
                if call.name == "write" && !ctx.workspace.root().join(&rel).exists() {
                    if let Ok(mut w) = self.written.lock() {
                        w.insert(rel);
                    }
                }
                HookDecision::Allow
            }
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
            written: Written::default(),
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
            written: Written::default(),
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
            written: Written::default(),
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

    fn own(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    /// The four commands the hebrew-calendar Run was refused, deleting a
    /// probe it had written itself — now housekeeping, not a question.
    #[test]
    fn deleting_what_the_session_wrote_needs_no_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let r = root.display().to_string();
        let written = own(&["hebrew_core/src/calendar_tmp_probe.rs"]);
        for cmd in [
            format!("cd {r} && rm -f hebrew_core/src/calendar_tmp_probe.rs && ls hebrew_core"),
            format!("cd {r} && rm hebrew_core/src/calendar_tmp_probe.rs && ls hebrew_core/src"),
            format!("cd {r} && ls examples && rm ./hebrew_core/src/calendar_tmp_probe.rs; ls hebrew_core/src"),
            "cd hebrew_core && rm src/calendar_tmp_probe.rs".to_string(),
        ] {
            if cfg!(windows) && cmd.contains(":\\") {
                continue; // a Windows root in a POSIX command line is quoting, below
            }
            assert!(only_removes_own_files(&cmd, root, &written), "{cmd}");
        }
        let scratch = smithy_tools::scratch_dir_for(root).display().to_string();
        if !scratch.contains('\\') {
            assert!(only_removes_own_files(
                &format!("rm {scratch}/probe"),
                root,
                &own(&[])
            ));
        }
    }

    #[test]
    fn anything_else_still_goes_to_jev() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let written = own(&["probe.rs"]);
        for cmd in [
            "rm src/lib.rs",              // not written by this Session
            "rm -r probe.rs",             // recursion
            "rm -rf .",                   // recursion, everything
            "rm *.rs",                    // a glob
            "rm probe.rs && cargo build", // something else too
            "rm probe.rs | tee x",        // a pipe
            "rm \"probe.rs\"",            // quoting
            "cd .. && rm probe.rs",       // outside the Project
            "rm",                         // nothing named
            "ls",                         // nothing removed: not this rule's business
        ] {
            assert!(!only_removes_own_files(cmd, root, &written), "{cmd}");
        }
    }

    /// Only a file the Session *created* is recorded; writing over one that
    /// was already there does not make it the Session's to delete.
    #[tokio::test]
    async fn only_created_files_are_recorded() {
        let (tmp, ctx, denied) = setup();
        std::fs::write(tmp.path().join("existing.rs"), "old").unwrap();
        let written = Written::default();
        let build = Registry::core().with_hook(UnattendedWrites {
            only_under: None,
            denied,
            task: None,
            written: written.clone(),
        });
        for (id, path) in [("1", "existing.rs"), ("2", "probe.rs")] {
            let args = serde_json::json!({ "path": path, "content": "x" }).to_string();
            let r = build
                .execute(&ToolCall::new(id, "write", &args), &ctx)
                .await;
            assert!(!r.is_error, "{}", r.content);
        }
        assert_eq!(*written.lock().unwrap(), own(&["probe.rs"]));
    }
}
