//! Did the model make the tests pass, or make them stop testing?
//!
//! Checked when a Task's Checks pass, against the tests that existed when the
//! Run began. Rules first, because they are cheap and cannot be talked round:
//!
//! - a test file from before the Run was deleted;
//! - a skip marker was added (`#[ignore]`, `@pytest.mark.skip`, `DISABLED_`…);
//! - a file lost assertions on balance.
//!
//! Any of those fails the Attempt outright. What passes the rules but still
//! edits or removes an assertion line goes to Jev with the diff: renaming a
//! function across the tests is fine, changing an expected value to match
//! the bug is not, and only reading the change tells them apart.
//!
//! Test counts are the other half, checked by the runner: no test that passed
//! at the Baseline may fail or vanish.

use std::path::Path;

use similar::{ChangeTag, TextDiff};

use crate::git::{ChangeKind, Git};
use crate::toolchain::Toolchain;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheatReport {
    /// Rules broken. Any one fails the Attempt.
    pub violations: Vec<String>,
    /// Test files whose assertion lines changed within the rules.
    pub suspicious: Vec<String>,
    /// The diff of the suspicious files, for Jev.
    pub diff: String,
}

impl CheatReport {
    pub fn clean(&self) -> bool {
        self.violations.is_empty() && self.suspicious.is_empty()
    }
}

/// Compare the working tree to `since` (the Task's starting commit), for test
/// files that already existed at `run_base`.
pub fn inspect(
    git: &Git,
    toolchain: &Toolchain,
    run_base: &str,
    since: &str,
) -> Result<CheatReport, String> {
    let mut report = CheatReport::default();
    for change in git.changed_since(since)? {
        let path = change.path.replace('\\', "/");
        if !toolchain.is_test_path(&path) || git.show(run_base, &path).is_none() {
            continue;
        }
        let Some(before) = git.show(since, &path) else {
            continue;
        };
        if change.kind == ChangeKind::Deleted {
            report
                .violations
                .push(format!("deleted `{path}`, a test file from before the Run"));
            continue;
        }
        let after = std::fs::read_to_string(git.root().join(Path::new(&path))).unwrap_or_default();
        let (skips_before, skips_after) = (
            toolchain.count_skips(&before),
            toolchain.count_skips(&after),
        );
        if skips_after > skips_before {
            report.violations.push(format!(
                "`{path}`: {} test(s) switched off (skip markers {skips_before} → {skips_after})",
                skips_after - skips_before
            ));
        }
        let (asserts_before, asserts_after) = (
            toolchain.count_asserts(&before),
            toolchain.count_asserts(&after),
        );
        if asserts_after < asserts_before {
            report.violations.push(format!(
                "`{path}`: assertions went from {asserts_before} to {asserts_after}"
            ));
        }
        if touched_assertions(toolchain, &before, &after) {
            report.suspicious.push(path.clone());
            report.diff.push_str(&unified(&path, &before, &after));
        }
    }
    Ok(report)
}

/// Whether any removed line was an assertion. Added assertions alone are
/// never suspicious.
fn touched_assertions(toolchain: &Toolchain, before: &str, after: &str) -> bool {
    TextDiff::from_lines(before, after)
        .iter_all_changes()
        .filter(|c| c.tag() == ChangeTag::Delete)
        .any(|c| toolchain.count_asserts(c.value()) > 0)
}

fn unified(path: &str, before: &str, after: &str) -> String {
    format!(
        "{}",
        TextDiff::from_lines(before, after)
            .unified_diff()
            .context_radius(3)
            .header(&format!("a/{path}"), &format!("b/{path}"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::process::Command;

    const TESTS: &str = "#[test]\nfn one_day() {\n    assert_eq!(parse(\"P1D\"), Ok(86_400));\n}\n\n#[test]\nfn one_week() {\n    assert_eq!(parse(\"P1W\"), Ok(604_800));\n}\n";

    fn repo() -> (tempfile::TempDir, Git, String) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| {
            assert!(Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success());
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/parse.rs"), TESTS).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        let g = Git::open(root).unwrap();
        let base = g.head().unwrap();
        (tmp, g, base)
    }

    fn edit(tmp: &tempfile::TempDir, from: &str, to: &str) {
        let p = tmp.path().join("tests/parse.rs");
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains(from), "fixture lacks {from}");
        std::fs::write(p, text.replace(from, to)).unwrap();
    }

    #[test]
    fn untouched_tests_are_clean() {
        let (tmp, g, base) = repo();
        std::fs::write(tmp.path().join("src.rs"), "fn parse() {}").unwrap();
        assert!(inspect(&g, &Toolchain::rust(), &base, &base)
            .unwrap()
            .clean());
    }

    #[test]
    fn deleting_a_test_file_is_a_violation() {
        let (tmp, g, base) = repo();
        std::fs::remove_file(tmp.path().join("tests/parse.rs")).unwrap();
        let r = inspect(&g, &Toolchain::rust(), &base, &base).unwrap();
        assert!(r.violations[0].contains("deleted"), "{r:?}");
    }

    #[test]
    fn a_new_ignore_is_a_violation() {
        let (tmp, g, base) = repo();
        edit(
            &tmp,
            "#[test]\nfn one_week",
            "#[test]\n#[ignore]\nfn one_week",
        );
        let r = inspect(&g, &Toolchain::rust(), &base, &base).unwrap();
        assert!(
            r.violations.iter().any(|v| v.contains("switched off")),
            "{r:?}"
        );
    }

    #[test]
    fn losing_an_assertion_is_a_violation() {
        let (tmp, g, base) = repo();
        edit(&tmp, "    assert_eq!(parse(\"P1W\"), Ok(604_800));\n", "");
        let r = inspect(&g, &Toolchain::rust(), &base, &base).unwrap();
        assert!(
            r.violations
                .iter()
                .any(|v| v.contains("assertions went from 2 to 1")),
            "{r:?}"
        );
    }

    /// Within the rules, but only reading it tells a fix from a cheat: to Jev.
    #[test]
    fn a_changed_expected_value_is_suspicious_not_a_violation() {
        let (tmp, g, base) = repo();
        edit(&tmp, "Ok(604_800)", "Ok(518_400)");
        let r = inspect(&g, &Toolchain::rust(), &base, &base).unwrap();
        assert!(r.violations.is_empty(), "{r:?}");
        assert_eq!(r.suspicious, vec!["tests/parse.rs".to_string()]);
        assert!(
            r.diff
                .contains("-    assert_eq!(parse(\"P1W\"), Ok(604_800));"),
            "{}",
            r.diff
        );
    }

    #[test]
    fn adding_assertions_is_fine() {
        let (tmp, g, base) = repo();
        edit(
            &tmp,
            "Ok(604_800));\n",
            "Ok(604_800));\n    assert!(parse(\"P1X\").is_err());\n",
        );
        assert!(inspect(&g, &Toolchain::rust(), &base, &base)
            .unwrap()
            .clean());
    }

    /// A test file the Run itself created is the Run's to change.
    #[test]
    fn tests_written_during_the_run_are_not_protected() {
        let (tmp, g, base) = repo();
        std::fs::write(tmp.path().join("tests/new.rs"), "fn a() { assert!(x); }").unwrap();
        let t1 = g.checkpoint("T1", &BTreeSet::new()).unwrap();
        std::fs::write(tmp.path().join("tests/new.rs"), "fn a() {}").unwrap();
        assert!(inspect(&g, &Toolchain::rust(), &base, &t1).unwrap().clean());
    }
}
