//! Shell execution.
//!
//! Ported from coda, which drains stdout and stderr on separate threads so a
//! full pipe cannot deadlock the parent, and kills the child on timeout. coda's
//! post-mortem flagged that this path "compiled and ran one quick command" and
//! was never verified to actually kill a runaway process or truncate correctly —
//! so those two behaviours have tests here.
//!
//! The guardrail in [`crate::sandbox::check_bash`] runs first, but the real
//! control for shell commands is an approval [`crate::ToolHook`]: unlike the
//! filesystem tools, a subprocess is not confined by the workspace capability.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{middle_truncate, Tool, ToolCtx};
use crate::sandbox::check_bash;
use crate::schema::{arg_i64, arg_str, ToolDefinition, ToolOutput, ToolParameter};

const DEFAULT_TIMEOUT_S: u64 = 30;
const MAX_TIMEOUT_S: u64 = 600;
const MAX_OUTPUT_CHARS: usize = 30_000;

pub struct Bash;

#[async_trait]
impl Tool for Bash {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "bash",
            format!(
                "Run a shell command from the workspace root and return combined stdout and \
                 stderr. Default timeout 30 seconds. Output is capped at 30000 characters, \
                 truncated in the middle so both the command's start and its error tail survive. \
                 Commands must be non-interactive.{SHELL_NOTE}"
            ),
            vec![
                ToolParameter::string("command", "The shell command to run.", true),
                ToolParameter::integer(
                    "timeout",
                    "Timeout in seconds (default 30, max 600).",
                    false,
                ),
            ],
        )
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutput {
        let command = match arg_str(args, "command") {
            Ok(c) => c.to_string(),
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = check_bash(&command) {
            return ToolOutput::err(e);
        }
        let timeout = Duration::from_secs(
            arg_i64(args, "timeout")
                .map(|t| t.clamp(1, MAX_TIMEOUT_S as i64) as u64)
                .unwrap_or(DEFAULT_TIMEOUT_S),
        );
        let cwd = ctx.workspace.root().to_path_buf();

        match tokio::task::spawn_blocking(move || run_with_timeout(&command, &cwd, timeout)).await {
            Ok(Ok(out)) => ToolOutput::ok(out),
            Ok(Err(e)) => ToolOutput::err(e),
            Err(e) => ToolOutput::err(format!("shell task failed: {e}")),
        }
    }
}

/// How long to wait for the output readers after the child is gone.
///
/// Belt and braces behind the process-group kill: a command that deliberately
/// escapes its group (`setsid`, a daemon that double-forks) can still hold the
/// pipe open, and a tool call that never returns is worse than one that returns
/// truncated output. The loop has no cancellation checkpoint inside it, so
/// "never returns" means the whole agent turn is stuck.
const DRAIN_GRACE: Duration = Duration::from_secs(3);

/// Kill a whole process group.
///
/// The child is its own group leader — see `process_group(0)` below — so its pid
/// doubles as the group id.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // SAFETY: `killpg` takes a group id and a signal number and touches no
    // memory we own. A group that has already exited yields `ESRCH`, which is
    // exactly the case we do not care about.
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

/// Told to the model on Windows, where it would otherwise reach for `dir` and
/// `C:\` paths that the shell below does not speak.
#[cfg(windows)]
const SHELL_NOTE: &str =
    " The shell is Git Bash: POSIX syntax and tools, with Windows drives at /c/, /d/, ...";
#[cfg(not(windows))]
const SHELL_NOTE: &str = "";

/// The POSIX shell commands run under.
#[cfg(not(windows))]
fn shell() -> Result<PathBuf, String> {
    Ok(PathBuf::from("sh"))
}

/// Windows has no `sh`, so this is Git for Windows' `bash`, found once.
///
/// Not `which("bash")`: on a machine with WSL that finds `System32\bash.exe`,
/// which runs the command inside a Linux VM with a different filesystem view —
/// the workspace root would not even be the directory it starts in.
#[cfg(windows)]
fn shell() -> Result<PathBuf, String> {
    static SHELL: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    SHELL.get_or_init(find_git_bash).clone().ok_or_else(|| {
        "no POSIX shell found: install Git for Windows (`winget install Git.Git`), or set \
         SMITHY_SHELL to a bash.exe"
            .to_string()
    })
}

#[cfg(windows)]
fn find_git_bash() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("SMITHY_SHELL") {
        return Some(PathBuf::from(explicit));
    }
    // `git.exe` is on PATH as `<root>\cmd\git.exe`; the shell is `<root>\bin\bash.exe`,
    // the launcher that puts the MSYS tools on the child's PATH.
    let beside_git = which::which("git")
        .ok()
        .and_then(|git| Some(git.parent()?.parent()?.join("bin").join("bash.exe")));
    let default_install = std::env::var_os("ProgramFiles")
        .map(|pf| PathBuf::from(pf).join("Git").join("bin").join("bash.exe"));
    // A per-user install (the installer's "only for me", and winget's default
    // scope on a machine without admin rights) lands here and is not always on
    // the PATH of an app launched from the Start menu.
    let user_install = std::env::var_os("LOCALAPPDATA").map(|local| {
        PathBuf::from(local)
            .join("Programs")
            .join("Git")
            .join("bin")
            .join("bash.exe")
    });
    beside_git
        .into_iter()
        .chain(default_install)
        .chain(user_install)
        .find(|candidate| candidate.is_file())
}

/// Whether the agent can run shell commands here, and if not, why.
///
/// For a notice on connect: without a shell the agent can still read, search
/// and edit, but every build and test it tries fails, and only the model sees
/// why unless the person is told.
pub fn shell_available() -> Result<PathBuf, String> {
    shell()
}

/// A Job Object holding the shell, so the timeout kills its whole tree.
///
/// The Windows counterpart of the process group: children inherit the job, and
/// `TerminateJobObject` ends every member at once. Git Bash's `bin\bash.exe`
/// is itself a launcher for `usr\bin\bash.exe`, so without this even a plain
/// `sleep` would outlive a kill aimed at the process we spawned.
///
/// The job is assigned just after spawn rather than with the child suspended,
/// which `std::process` cannot do. The window is the launcher's own startup,
/// before it has created anything to leak.
#[cfg(windows)]
struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    fn holding(child: &std::process::Child) -> Option<Job> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
        // SAFETY: an anonymous job with default security; the handle is owned by
        // the returned `Job` and closed exactly once, in `Drop`. The process
        // handle is borrowed from `child`, which outlives this call.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            if AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Job(job))
        }
    }

    fn terminate(&self) {
        // SAFETY: a live job handle we own. Terminating an already-empty job
        // succeeds and does nothing.
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for Job {
    /// Closing the handle does not kill anything: without
    /// `KILL_ON_JOB_CLOSE`, a command that finished normally and left a
    /// background process behind leaves it running, as it would on Unix.
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateJobObjectW` and is closed only here.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// What a command did, in full. The tool formats this for the model; a
/// caller that decides on the result (the runner's checks) reads it directly,
/// because the model-facing form is middle-truncated and a test summary in the
/// middle of a long run is exactly what truncation would drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// `None` when it was killed at the timeout, or ended by a signal.
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }

    /// Stdout, then stderr, the way a terminal would interleave them roughly.
    pub fn combined(&self) -> String {
        let mut combined = self.stdout.clone();
        if !self.stderr.trim().is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&self.stderr);
        }
        combined
    }
}

/// Spawn `sh -c command`, capture combined output, kill it if it overruns.
pub fn run_with_timeout(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
) -> Result<String, String> {
    let captured = run_captured(command, cwd, timeout)?;
    let mut result = middle_truncate(captured.combined().trim_end(), MAX_OUTPUT_CHARS);

    if captured.timed_out {
        result = format!(
            "[command killed after {}s timeout]\n{result}",
            timeout.as_secs()
        );
    } else if captured.code != Some(0) {
        let code = captured
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into());
        result = format!("[exit {code}]\n{result}");
    }
    if result.trim().is_empty() {
        result = "[no output]".to_string();
    }
    Ok(result)
}

/// Spawn `sh -c command` with the secret-scrubbed environment, and return
/// everything it wrote. Killed, with its whole process tree, at `timeout`.
pub fn run_captured(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
) -> Result<Captured, String> {
    let mut builder = Command::new(shell()?);
    builder
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_secret_env(&mut builder);

    // Put the shell in a process group of its own, so the timeout can kill
    // everything it started rather than just the shell.
    //
    // Without this, `Child::kill` reaps `sh` and leaves any backgrounded
    // grandchild running — still holding the write end of the stdout pipe it
    // inherited. The reader thread below then blocks in `read_to_end` until
    // *that* process exits, which for the runaway command this timeout exists
    // to stop is never. The observable failure is not a slow tool call; it is a
    // turn that never ends and a leaked blocking thread.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        builder.process_group(0);
    }

    let mut child = builder
        .spawn()
        .map_err(|e| format!("failed to spawn shell: {e}"))?;
    #[cfg(unix)]
    let pid = child.id();
    #[cfg(windows)]
    let job = Job::holding(&child);

    // Drain both pipes on their own threads: a child that fills the stdout pipe
    // blocks forever if the parent is not reading while it waits.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let (tx_o, rx_o) = mpsc::channel();
    let (tx_e, rx_e) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx_o.send(buf);
    });
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = tx_e.send(buf);
    });

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    // The group first: `kill` alone leaves the grandchildren.
                    #[cfg(unix)]
                    kill_process_group(pid);
                    #[cfg(windows)]
                    if let Some(job) = &job {
                        job.terminate();
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("error waiting on command: {e}")),
        }
    };

    // The reader threads finish once the pipes close, on exit or on kill —
    // bounded, because a process that escaped the group would otherwise hold
    // this open forever. See `DRAIN_GRACE`.
    let out = rx_o.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    let err = rx_e.recv_timeout(DRAIN_GRACE).unwrap_or_default();

    Ok(Captured {
        code: status.and_then(|s| s.code()),
        timed_out,
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
    })
}

/// Hygiene, not a boundary: the child inherits the process environment, which
/// can include `OPENROUTER_API_KEY` when the env fallback is in use. Approval
/// is the boundary for shell; this only stops the obvious leak of named
/// secrets. `cd ..` out of the project remains possible.
fn scrub_secret_env(cmd: &mut Command) {
    let keys: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| {
            let key = k.to_str()?;
            if is_secret_env(key) {
                Some(key.to_string())
            } else {
                None
            }
        })
        .collect();
    for key in keys {
        cmd.env_remove(key);
    }
}

fn is_secret_env(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.ends_with("_API_KEY") || upper.ends_with("_TOKEN") || upper.ends_with("_SECRET")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Workspace;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path()).unwrap();
        (tmp, ToolCtx::new(ws))
    }

    #[tokio::test]
    async fn runs_a_command_and_returns_stdout() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "echo hello"}), &ctx)
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "hello");
    }

    #[tokio::test]
    async fn runs_in_the_workspace_root() {
        let (_t, ctx) = ctx();
        ctx.workspace.write("marker.txt", "").unwrap();
        let out = Bash.run(&serde_json::json!({"command": "ls"}), &ctx).await;
        assert!(out.content.contains("marker.txt"));
    }

    #[tokio::test]
    async fn reports_a_nonzero_exit_code() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "exit 3"}), &ctx)
            .await;
        assert!(out.content.starts_with("[exit 3]"));
    }

    #[tokio::test]
    async fn captures_stderr_too() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "echo oops 1>&2"}), &ctx)
            .await;
        assert!(out.content.contains("oops"));
    }

    /// coda shipped the kill path untested. This verifies a runaway command is
    /// actually terminated at the deadline rather than hanging the agent.
    #[tokio::test]
    async fn kills_a_runaway_command_at_the_timeout() {
        let (_t, ctx) = ctx();
        let started = Instant::now();
        let out = Bash
            .run(
                &serde_json::json!({"command": "sleep 30", "timeout": 1}),
                &ctx,
            )
            .await;
        let elapsed = started.elapsed();
        assert!(
            out.content.contains("killed after 1s timeout"),
            "got: {}",
            out.content
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "took {elapsed:?}, should have been ~1s"
        );
    }

    /// **A command that backgrounds something must still be killed whole.**
    ///
    /// `Child::kill` signals only the shell. A grandchild it started inherits
    /// the stdout pipe and keeps the write end open, so the reader thread stays
    /// blocked in `read_to_end` until *that* process exits — thirty seconds
    /// here, and never for the runaway this timeout exists to stop. The tool
    /// call does not return late, it does not return, and the turn it belongs to
    /// has no cancellation checkpoint inside a tool.
    ///
    /// The bound is deliberately tighter than `DRAIN_GRACE`. Falling back to the
    /// reader timeout would also return, eventually, and a looser assertion
    /// would pass with the process group left un-killed — which is the thing
    /// under test.
    #[tokio::test]
    async fn a_backgrounded_grandchild_is_killed_with_the_shell_that_started_it() {
        let (_t, ctx) = ctx();
        let started = Instant::now();
        let out = Bash
            .run(
                &serde_json::json!({"command": "sleep 30 & wait", "timeout": 1}),
                &ctx,
            )
            .await;
        let elapsed = started.elapsed();

        assert!(
            out.content.contains("killed after 1s timeout"),
            "got: {}",
            out.content
        );
        assert!(
            elapsed < Duration::from_millis(2_500),
            "took {elapsed:?}: the shell was killed but its child kept the pipe open, so this \
             returned via the reader grace period rather than because the group died"
        );
    }

    /// Also untested in coda: that large output truncates instead of blowing the
    /// context budget, and that the tail survives.
    #[tokio::test]
    async fn truncates_large_output_keeping_both_ends() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(
                &serde_json::json!({
                    "command": "for i in $(seq 1 20000); do echo LINE$i; done",
                    "timeout": 60
                }),
                &ctx,
            )
            .await;
        assert!(
            out.content.contains("truncated"),
            "output should be truncated"
        );
        assert!(out.content.contains("LINE1\n"), "head should survive");
        assert!(out.content.contains("LINE20000"), "tail should survive");
        assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 200);
    }

    #[tokio::test]
    async fn a_command_producing_nothing_says_so() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "true"}), &ctx)
            .await;
        assert_eq!(out.content, "[no output]");
    }

    #[tokio::test]
    async fn the_guardrail_blocks_before_spawning() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "sudo rm -rf /"}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("guardrail"));
    }

    #[tokio::test]
    async fn stdin_is_closed_so_interactive_commands_cannot_hang() {
        let (_t, ctx) = ctx();
        let out = Bash
            .run(&serde_json::json!({"command": "cat", "timeout": 5}), &ctx)
            .await;
        // With stdin at /dev/null, `cat` reads EOF immediately instead of hanging.
        assert!(
            !out.content.contains("killed after"),
            "got: {}",
            out.content
        );
    }

    /// Hygiene, not a sandbox: approval is the boundary for shell. This only
    /// asserts a planted `*_API_KEY` does not appear in the child. `cd ..`
    /// out of the project remains possible.
    #[tokio::test]
    async fn a_child_process_does_not_see_a_planted_api_key() {
        let (_t, ctx) = ctx();
        let planted = "smithy-test-planted-key-9f3a";
        std::env::set_var("OPENROUTER_API_KEY", planted);
        std::env::set_var("SMITHY_TEST_API_KEY", planted);
        let out = Bash
            .run(
                &serde_json::json!({
                    "command": "printf 'OPEN=%s PLANTED=%s' \"$OPENROUTER_API_KEY\" \"$SMITHY_TEST_API_KEY\""
                }),
                &ctx,
            )
            .await;
        std::env::remove_var("OPENROUTER_API_KEY");
        std::env::remove_var("SMITHY_TEST_API_KEY");
        assert!(
            !out.content.contains(planted),
            "child saw a secret: {}",
            out.content
        );
    }
}
