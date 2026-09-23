//! Running a Check, and deciding what it said.
//!
//! The model's "the tests pass" is a claim. This is the measurement: the
//! runner runs the command itself, through the same shell the model's `bash`
//! uses, and reads the exit status and the test count out of the full output.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use smithy_tools::tools::bash::{run_captured, Captured};

use crate::toolchain::{TestRun, Toolchain};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckKind {
    Build,
    Test,
    Lint,
    Command,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    pub kind: CheckKind,
    pub run: String,
    /// For `test`: fewer tests than this ran is a failure, whatever the exit
    /// status. A filter that matches nothing exits 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tests: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub spec: CheckSpec,
    pub passed: bool,
    /// One line: why it passed or failed.
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestRun>,
    /// The part of the output worth reading, for the model and the Report.
    pub excerpt: String,
    pub seconds: u64,
}

impl CheckOutcome {
    /// Stable enough to tell "the same failure again" from a new one: the
    /// command and the first error line, without numbers that drift.
    pub fn signature(&self) -> String {
        let first = self
            .excerpt
            .lines()
            .find(|l| is_error_line(l))
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .collect::<String>();
        format!("{}|{}", self.spec.run, first.trim())
    }
}

/// Long enough for a cold `cargo build` of a small crate on this machine.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(1200);

/// How much output survives into history and the Report.
const EXCERPT_CHARS: usize = 6000;

pub fn run_check(
    spec: &CheckSpec,
    toolchain: &Toolchain,
    root: &Path,
    timeout: Duration,
) -> CheckOutcome {
    let started = Instant::now();
    let captured = run_captured(&spec.run, root, timeout);
    let seconds = started.elapsed().as_secs();
    match captured {
        Ok(captured) => judge(spec, toolchain, &captured, seconds),
        Err(e) => CheckOutcome {
            spec: spec.clone(),
            passed: false,
            verdict: format!("could not run: {e}"),
            tests: None,
            excerpt: String::new(),
            seconds,
        },
    }
}

fn judge(
    spec: &CheckSpec,
    toolchain: &Toolchain,
    captured: &Captured,
    seconds: u64,
) -> CheckOutcome {
    let output = captured.combined();
    let tests = toolchain.parse_tests(&output);
    let excerpt = excerpt(&output, EXCERPT_CHARS);
    let (passed, verdict) = if captured.timed_out {
        (false, format!("killed after {seconds}s"))
    } else if !captured.success() {
        let code = captured
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into());
        let counts = tests
            .as_ref()
            .map(|t| format!(" ({} passed, {} failed)", t.passed, t.failed))
            .unwrap_or_default();
        (false, format!("exit {code}{counts}"))
    } else if spec.kind == CheckKind::Test {
        let min = spec.min_tests.unwrap_or(1);
        match &tests {
            None => (
                false,
                "exit 0 but no test summary in the output — cannot tell whether any test ran"
                    .into(),
            ),
            Some(t) if t.failed > 0 => (false, format!("{} failed despite exit 0", t.failed)),
            Some(t) if t.passed < min => (
                false,
                format!(
                    "only {} test(s) ran and passed; this check needs at least {min}",
                    t.passed
                ),
            ),
            Some(t) => (true, format!("{} passed", t.passed)),
        }
    } else {
        (true, "exit 0".into())
    };
    CheckOutcome {
        spec: spec.clone(),
        passed,
        verdict,
        tests,
        excerpt,
        seconds,
    }
}

fn is_error_line(line: &str) -> bool {
    let l = line.trim_start();
    l.starts_with("error")
        || l.starts_with("FAILED")
        || l.starts_with("E ")
        || l.contains("panicked at")
        || l.starts_with("Traceback")
        || l.contains("***Failed")
        || l.contains(": error:")
        || l.starts_with("---- ")
}

/// From a little before the first error, or the tail when nothing looks like
/// one. Compiler output puts the cause first and the summary last; the model
/// needs the cause.
pub fn excerpt(output: &str, max: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines
        .iter()
        .position(|l| is_error_line(l))
        .map(|i| i.saturating_sub(3));
    let text = match start {
        Some(i) => lines[i..].join("\n"),
        None => output.to_string(),
    };
    let count = text.chars().count();
    if count <= max {
        return text;
    }
    if start.is_some() {
        let head: String = text.chars().take(max).collect();
        format!("{head}\n[… {} more characters]", count - max)
    } else {
        let tail: String = text.chars().skip(count - max).collect();
        format!("[… {} earlier characters]\n{tail}", count - max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(code: i32, stdout: &str) -> Captured {
        Captured {
            code: Some(code),
            timed_out: false,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    fn test_spec(min: Option<usize>) -> CheckSpec {
        CheckSpec {
            kind: CheckKind::Test,
            run: "cargo test dur".into(),
            min_tests: min,
        }
    }

    const PASSED_2: &str = "test a ... ok\ntest b ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";

    #[test]
    fn a_test_check_passes_on_exit_zero_with_enough_tests() {
        let o = judge(
            &test_spec(Some(2)),
            &Toolchain::rust(),
            &captured(0, PASSED_2),
            1,
        );
        assert!(o.passed, "{}", o.verdict);
    }

    #[test]
    fn too_few_tests_fails_even_on_exit_zero() {
        let o = judge(
            &test_spec(Some(3)),
            &Toolchain::rust(),
            &captured(0, PASSED_2),
            1,
        );
        assert!(!o.passed);
        assert!(o.verdict.contains("at least 3"), "{}", o.verdict);
    }

    #[test]
    fn a_filter_that_matched_nothing_fails() {
        let out = "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out\n";
        let o = judge(&test_spec(None), &Toolchain::rust(), &captured(0, out), 1);
        assert!(!o.passed, "{}", o.verdict);
    }

    #[test]
    fn a_test_check_without_a_summary_fails() {
        let o = judge(
            &test_spec(None),
            &Toolchain::rust(),
            &captured(0, "all good!\n"),
            1,
        );
        assert!(!o.passed);
    }

    #[test]
    fn a_build_check_is_exit_status_alone() {
        let spec = CheckSpec {
            kind: CheckKind::Build,
            run: "cargo build".into(),
            min_tests: None,
        };
        assert!(judge(&spec, &Toolchain::rust(), &captured(0, ""), 1).passed);
        let failed = judge(
            &spec,
            &Toolchain::rust(),
            &captured(101, "error[E0425]: nope\n"),
            1,
        );
        assert!(!failed.passed);
        assert_eq!(failed.verdict, "exit 101");
    }

    #[test]
    fn the_excerpt_starts_near_the_first_error() {
        let mut out = String::new();
        for i in 0..500 {
            out.push_str(&format!("   Compiling crate{i} v1.0.0\n"));
        }
        out.push_str("error[E0308]: mismatched types\n --> src/lib.rs:3:5\n");
        let e = excerpt(&out, 400);
        assert!(e.contains("error[E0308]"), "{e}");
        assert!(!e.contains("crate10 "), "{e}");
    }

    #[test]
    fn signatures_ignore_line_numbers() {
        let mk = |ex: &str| CheckOutcome {
            spec: test_spec(None),
            passed: false,
            verdict: String::new(),
            tests: None,
            excerpt: ex.into(),
            seconds: 0,
        };
        assert_eq!(
            mk("error[E0308]: at src/lib.rs:10").signature(),
            mk("error[E0308]: at src/lib.rs:12").signature()
        );
        assert_ne!(
            mk("error[E0308]: x").signature(),
            mk("error[E0425]: y").signature()
        );
    }

    /// Through the real shell, so the Windows path (Git Bash) is exercised.
    #[test]
    fn a_real_command_runs_in_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
        let spec = CheckSpec {
            kind: CheckKind::Command,
            run: "test -f marker.txt".into(),
            min_tests: None,
        };
        let o = run_check(
            &spec,
            &Toolchain::rust(),
            tmp.path(),
            Duration::from_secs(30),
        );
        assert!(o.passed, "{o:?}");
        let spec = CheckSpec {
            run: "test -f absent.txt".into(),
            ..spec
        };
        assert!(
            !run_check(
                &spec,
                &Toolchain::rust(),
                tmp.path(),
                Duration::from_secs(30)
            )
            .passed
        );
    }
}
