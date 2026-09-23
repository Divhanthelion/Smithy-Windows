//! What "build" and "test" mean in a Project, whatever its language.
//!
//! This is the whole of the language seam for a Run. The runner never says
//! `cargo`: it asks the [`Toolchain`] for commands, for which paths are tests,
//! for the markers that skip a test, and for the test counts in a command's
//! output. Rust, C (CMake + ctest) and Python (pytest) are detected from their
//! manifests; `.smithy/checks.toml` overrides any command, or declares a
//! toolchain for a Project nothing here recognises.
//!
//! The Map, the symbol index and the call graph stay Rust-only. They are not
//! part of this seam, because a Run's ground truth is the compiler and the
//! tests, and those every toolchain has.

use std::collections::BTreeSet;
use std::path::Path;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// How to read test results out of a command's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Counter {
    Cargo,
    Pytest,
    Ctest,
    /// Exit status only. A `test` check on a toolchain that cannot count is
    /// refused at plan validation rather than passing on a silent zero.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Toolchain {
    pub name: String,
    /// Compile everything, tests included, without running them.
    pub build: Option<String>,
    /// The full suite. The Baseline is taken with this.
    pub test: String,
    pub lint: Option<String>,
    pub counter: Counter,
    /// Globs, relative to the Project root, for files that are tests.
    pub test_paths: Vec<String>,
    /// Regexes for a test being switched off rather than fixed.
    pub skip_markers: Vec<String>,
    /// Regexes counted per line as an assertion, for the weakening check.
    pub assert_markers: Vec<String>,
}

impl Toolchain {
    pub fn rust() -> Toolchain {
        Toolchain {
            name: "rust".into(),
            build: Some("cargo build --all-targets".into()),
            test: "cargo test --no-fail-fast".into(),
            lint: Some("cargo clippy --all-targets -- -D warnings".into()),
            counter: Counter::Cargo,
            // Unit tests live beside the code in Rust, so every source file is
            // a potential test file; the weakening check looks inside
            // `#[cfg(test)]` modules rather than trusting the path.
            test_paths: vec![
                "tests/**/*.rs".into(),
                "src/**/*.rs".into(),
                "crates/**/*.rs".into(),
            ],
            skip_markers: vec![r"#\[ignore".into(), r"#\[cfg\(any\(\)\)\]".into()],
            assert_markers: vec![
                r"\bassert(_eq|_ne|_matches)?!".into(),
                r"\bdebug_assert(_eq|_ne)?!".into(),
                r"\.(unwrap_err|expect_err)\(".into(),
                r"#\[should_panic".into(),
            ],
        }
    }

    pub fn cmake() -> Toolchain {
        Toolchain {
            name: "cmake".into(),
            build: Some("cmake -S . -B build && cmake --build build".into()),
            test: "cmake -S . -B build && cmake --build build && ctest --test-dir build --output-on-failure"
                .into(),
            lint: None,
            counter: Counter::Ctest,
            test_paths: vec!["test/**".into(), "tests/**".into(), "**/test_*.c".into(), "**/*_test.c".into()],
            skip_markers: vec![
                r"DISABLED_".into(),
                r"\bGTEST_SKIP\b".into(),
                r"\bTEST_IGNORE".into(),
                r"DISABLED\s+(TRUE|ON)".into(),
                r"WILL_FAIL\s+(TRUE|ON)".into(),
            ],
            assert_markers: vec![
                r"\bassert\s*\(".into(),
                r"\b(ASSERT|EXPECT|TEST_ASSERT|CU_ASSERT|ck_assert)\w*\s*\(".into(),
            ],
        }
    }

    pub fn python() -> Toolchain {
        Toolchain {
            name: "python".into(),
            build: Some("python -m compileall -q .".into()),
            // `-rA` puts every outcome in the short summary, which is where
            // the per-test names for the Baseline come from.
            test: "python -m pytest -rA".into(),
            lint: None,
            counter: Counter::Pytest,
            test_paths: vec![
                "tests/**/*.py".into(),
                "test/**/*.py".into(),
                "**/test_*.py".into(),
                "**/*_test.py".into(),
            ],
            skip_markers: vec![
                r"@pytest\.mark\.(skip|xfail)".into(),
                r"\bpytest\.skip\(".into(),
                r"@unittest\.skip".into(),
                r"\bself\.skipTest\(".into(),
            ],
            assert_markers: vec![
                r"^\s*assert\b".into(),
                r"\bself\.assert\w+\(".into(),
                r"\bpytest\.raises\(".into(),
            ],
        }
    }

    /// The toolchain for `root`: detected from a manifest, then overridden by
    /// `.smithy/checks.toml`. `Err` when neither says anything.
    pub fn for_project(root: &Path) -> Result<Toolchain, String> {
        let detected = detect(root);
        let overrides = match std::fs::read_to_string(root.join(".smithy").join("checks.toml")) {
            Ok(text) => Some(
                toml::from_str::<Overrides>(&text)
                    .map_err(|e| format!(".smithy/checks.toml: {e}"))?,
            ),
            Err(_) => None,
        };
        match (detected, overrides) {
            (Some(tc), None) => Ok(tc),
            (Some(tc), Some(o)) => Ok(o.apply(tc)),
            (None, Some(o)) => o.standalone(),
            (None, None) => Err(
                "no toolchain: no Cargo.toml, CMakeLists.txt or pyproject.toml at the Project \
                 root, and no .smithy/checks.toml declaring one"
                    .into(),
            ),
        }
    }

    /// Whether `path` (relative, `/`-separated) is a test file here.
    pub fn is_test_path(&self, path: &str) -> bool {
        self.test_paths.iter().any(|g| glob_match(g, path))
    }

    /// Lines that switch a test off.
    pub fn count_skips(&self, text: &str) -> usize {
        count_matching_lines(&self.skip_markers, text)
    }

    pub fn count_asserts(&self, text: &str) -> usize {
        count_matching_lines(&self.assert_markers, text)
    }

    /// The test results in one command's output.
    pub fn parse_tests(&self, output: &str) -> Option<TestRun> {
        match self.counter {
            Counter::Cargo => parse_cargo(output),
            Counter::Pytest => parse_pytest(output),
            Counter::Ctest => parse_ctest(output),
            Counter::None => None,
        }
    }
}

/// `root`'s toolchain from its manifest alone. Rust wins a tie: a Cargo
/// workspace with a `pyproject.toml` for its bindings is a Rust Project.
pub fn detect(root: &Path) -> Option<Toolchain> {
    if root.join("Cargo.toml").is_file() {
        Some(Toolchain::rust())
    } else if root.join("CMakeLists.txt").is_file() {
        Some(Toolchain::cmake())
    } else if ["pyproject.toml", "setup.py", "setup.cfg", "pytest.ini"]
        .iter()
        .any(|m| root.join(m).is_file())
    {
        Some(Toolchain::python())
    } else {
        None
    }
}

/// `.smithy/checks.toml`. Every field is optional; what is present wins.
///
/// ```toml
/// toolchain = "python"          # base adapter when detection finds none
/// build = "make"
/// test = "make check"
/// lint = "ruff check ."
/// counter = "none"
/// test_paths = ["t/**"]
/// ```
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    toolchain: Option<String>,
    build: Option<String>,
    test: Option<String>,
    lint: Option<String>,
    counter: Option<Counter>,
    test_paths: Option<Vec<String>>,
    skip_markers: Option<Vec<String>>,
    assert_markers: Option<Vec<String>>,
}

impl Overrides {
    fn apply(self, mut tc: Toolchain) -> Toolchain {
        if let Some(b) = self.build {
            tc.build = Some(b).filter(|b| !b.trim().is_empty());
        }
        if let Some(t) = self.test {
            tc.test = t;
        }
        if let Some(l) = self.lint {
            tc.lint = Some(l).filter(|l| !l.trim().is_empty());
        }
        if let Some(c) = self.counter {
            tc.counter = c;
        }
        if let Some(p) = self.test_paths {
            tc.test_paths = p;
        }
        if let Some(s) = self.skip_markers {
            tc.skip_markers = s;
        }
        if let Some(a) = self.assert_markers {
            tc.assert_markers = a;
        }
        tc
    }

    fn standalone(self) -> Result<Toolchain, String> {
        let base = match self.toolchain.as_deref() {
            Some("rust") => Toolchain::rust(),
            Some("cmake") | Some("c") => Toolchain::cmake(),
            Some("python") => Toolchain::python(),
            name if self.test.is_some() => Toolchain {
                name: name.unwrap_or("custom").to_string(),
                build: None,
                test: String::new(),
                lint: None,
                counter: Counter::None,
                test_paths: Vec::new(),
                skip_markers: Vec::new(),
                assert_markers: Vec::new(),
            },
            _ => {
                return Err(
                    ".smithy/checks.toml must name a toolchain or give a test command".into(),
                )
            }
        };
        Ok(self.apply(base))
    }
}

/// What one test command reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestRun {
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    /// Per-test names, when the output carries them. Qualified by the test
    /// binary for cargo, so `lib` and `bin` tests with one path stay apart.
    pub passed_names: BTreeSet<String>,
    pub failed_names: BTreeSet<String>,
}

impl TestRun {
    pub fn ran(&self) -> usize {
        self.passed + self.failed
    }
}

fn parse_cargo(output: &str) -> Option<TestRun> {
    let summary =
        Regex::new(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored").ok()?;
    let line = Regex::new(r"^test (.+?) \.\.\. (ok|FAILED|ignored)").ok()?;
    let running = Regex::new(r"^\s*(?:Running (?:unittests )?(\S+)|Doc-tests (\S+))").ok()?;
    let mut run = TestRun::default();
    let mut seen_summary = false;
    let mut binary = String::new();
    for raw in output.lines() {
        let l = raw.trim_end();
        if let Some(c) = running.captures(l) {
            binary = c
                .get(1)
                .or_else(|| c.get(2))
                .map(|m| m.as_str().replace('\\', "/"))
                .unwrap_or_default();
            continue;
        }
        if let Some(c) = summary.captures(l) {
            seen_summary = true;
            run.passed += c[1].parse::<usize>().unwrap_or(0);
            run.failed += c[2].parse::<usize>().unwrap_or(0);
            run.ignored += c[3].parse::<usize>().unwrap_or(0);
            continue;
        }
        if let Some(c) = line.captures(l) {
            let name = format!("{binary}::{}", &c[1]);
            match &c[2] {
                "ok" => {
                    run.passed_names.insert(name);
                }
                "FAILED" => {
                    run.failed_names.insert(name);
                }
                _ => {}
            }
        }
    }
    seen_summary.then_some(run)
}

fn parse_pytest(output: &str) -> Option<TestRun> {
    // The final line: `==== 3 failed, 10 passed, 1 skipped in 0.12s ====`,
    // or `no tests ran`. Counts come in any order.
    let tail = Regex::new(r"^=+ (.*) in [\d.]+s(?: \([^)]*\))? =+$").ok()?;
    let count =
        Regex::new(r"(\d+) (passed|failed|error|errors|skipped|xfailed|xpassed|deselected)")
            .ok()?;
    let short = Regex::new(r"^(PASSED|FAILED|ERROR) (\S+)").ok()?;
    let mut run = TestRun::default();
    let mut seen = false;
    for l in output.lines().map(str::trim_end) {
        if let Some(c) = short.captures(l) {
            let name = c[2].to_string();
            if &c[1] == "PASSED" {
                run.passed_names.insert(name);
            } else {
                run.failed_names.insert(name);
            }
            continue;
        }
        if let Some(c) = tail.captures(l) {
            seen = true;
            run.passed = 0;
            run.failed = 0;
            run.ignored = 0;
            for k in count.captures_iter(&c[1]) {
                let n = k[1].parse::<usize>().unwrap_or(0);
                match &k[2] {
                    "passed" | "xpassed" => run.passed += n,
                    "failed" | "error" | "errors" => run.failed += n,
                    _ => run.ignored += n,
                }
            }
        }
    }
    seen.then_some(run)
}

fn parse_ctest(output: &str) -> Option<TestRun> {
    let tail = Regex::new(r"(\d+)% tests passed, (\d+) tests? failed out of (\d+)").ok()?;
    let line =
        Regex::new(r"Test\s+#\d+: (\S+) \.+\s*(\*\*\*)?(Passed|Failed|Not Run|Exception|Timeout)")
            .ok()?;
    let mut run = TestRun::default();
    let mut seen = false;
    for l in output.lines() {
        if let Some(c) = line.captures(l) {
            let name = c[1].to_string();
            if &c[3] == "Passed" {
                run.passed_names.insert(name);
            } else {
                run.failed_names.insert(name);
            }
        }
        if let Some(c) = tail.captures(l) {
            seen = true;
            let failed = c[2].parse::<usize>().unwrap_or(0);
            let total = c[3].parse::<usize>().unwrap_or(0);
            run.failed = failed;
            run.passed = total.saturating_sub(failed);
        }
    }
    if !seen && output.contains("No tests were found") {
        return Some(TestRun::default());
    }
    seen.then_some(run)
}

fn count_matching_lines(patterns: &[String], text: &str) -> usize {
    let compiled: Vec<Regex> = patterns.iter().filter_map(|p| Regex::new(p).ok()).collect();
    text.lines()
        .filter(|l| compiled.iter().any(|r| r.is_match(l)))
        .count()
}

/// `**` is any number of directories, `*` anything within one segment.
pub fn glob_match(glob: &str, path: &str) -> bool {
    let mut re = String::from("^");
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).is_ok_and(|r| r.is_match(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARGO: &str = "\
   Compiling dur v0.1.0
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.2s
     Running unittests src/lib.rs (target/debug/deps/dur-1a2b)

running 3 tests
test parse::tests::days ... ok
test parse::tests::hours ... FAILED
test parse::tests::slow ... ignored

failures:

---- parse::tests::hours stdout ----
thread 'parse::tests::hours' panicked at src/parse.rs:40:9:

test result: FAILED. 1 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/cli.rs (target/debug/deps/cli-9f8e)

running 2 tests
test days ... ok
test weeks ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

   Doc-tests dur

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
";

    #[test]
    fn cargo_counts_sum_across_binaries_and_names_carry_the_binary() {
        let run = Toolchain::rust().parse_tests(CARGO).unwrap();
        assert_eq!((run.passed, run.failed, run.ignored), (3, 1, 1));
        assert!(run.passed_names.contains("src/lib.rs::parse::tests::days"));
        assert!(run.passed_names.contains("tests/cli.rs::days"));
        assert!(run.failed_names.contains("src/lib.rs::parse::tests::hours"));
    }

    /// The case that makes counting worth doing: a filter that matched
    /// nothing exits 0.
    #[test]
    fn a_filter_that_matched_nothing_reads_as_zero_ran() {
        let out = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.00s\n";
        assert_eq!(Toolchain::rust().parse_tests(out).unwrap().ran(), 0);
    }

    #[test]
    fn output_with_no_summary_is_not_a_count() {
        assert_eq!(
            Toolchain::rust().parse_tests("error[E0425]: cannot find value"),
            None
        );
    }

    #[test]
    fn pytest_short_summary_and_tail() {
        let out = "\
tests/test_dur.py .F.                                              [100%]
=========================== short test summary info ============================
PASSED tests/test_dur.py::test_days
FAILED tests/test_dur.py::test_hours - AssertionError: 3 != 4
PASSED tests/test_dur.py::test_weeks
==================== 1 failed, 2 passed, 1 skipped in 0.05s ====================
";
        let run = Toolchain::python().parse_tests(out).unwrap();
        assert_eq!((run.passed, run.failed, run.ignored), (2, 1, 1));
        assert!(run.failed_names.contains("tests/test_dur.py::test_hours"));
        assert_eq!(run.passed_names.len(), 2);
    }

    #[test]
    fn pytest_no_tests_ran() {
        let run = Toolchain::python()
            .parse_tests(
                "============================ no tests ran in 0.01s =============================",
            )
            .unwrap();
        assert_eq!(run.ran(), 0);
    }

    #[test]
    fn ctest_lines_and_tail() {
        let out = "\
    Start 1: parse_days
1/2 Test #1: parse_days .......................   Passed    0.01 sec
    Start 2: parse_hours
2/2 Test #2: parse_hours ......................***Failed    0.01 sec

50% tests passed, 1 tests failed out of 2
";
        let run = Toolchain::cmake().parse_tests(out).unwrap();
        assert_eq!((run.passed, run.failed), (1, 1));
        assert!(run.passed_names.contains("parse_days"));
        assert!(run.failed_names.contains("parse_hours"));
    }

    #[test]
    fn skips_and_asserts_are_counted_per_language() {
        let rust = "#[test]\n#[ignore]\nfn a() { assert_eq!(1, 1); assert!(true); }\n";
        assert_eq!(Toolchain::rust().count_skips(rust), 1);
        assert_eq!(
            Toolchain::rust().count_asserts(rust),
            1,
            "per line, not per call"
        );
        let py = "@pytest.mark.skip(reason='x')\ndef test_a():\n    assert f() == 1\n    self.assertEqual(1, 1)\n";
        assert_eq!(Toolchain::python().count_skips(py), 1);
        assert_eq!(Toolchain::python().count_asserts(py), 2);
        let c = "TEST(Dur, DISABLED_Days) { ASSERT_EQ(1, 1); assert(x); }\n";
        assert_eq!(Toolchain::cmake().count_skips(c), 1);
    }

    #[test]
    fn globs() {
        assert!(glob_match("tests/**/*.rs", "tests/cli.rs"));
        assert!(glob_match("tests/**/*.rs", "tests/a/b.rs"));
        assert!(glob_match("**/test_*.py", "test_x.py"));
        assert!(glob_match("**/test_*.py", "pkg/test_x.py"));
        assert!(!glob_match("tests/**/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/a/b.rs"));
    }

    #[test]
    fn detection_and_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Toolchain::for_project(tmp.path()).is_err());

        std::fs::write(tmp.path().join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        assert_eq!(Toolchain::for_project(tmp.path()).unwrap().name, "python");

        std::fs::create_dir(tmp.path().join(".smithy")).unwrap();
        std::fs::write(
            tmp.path().join(".smithy/checks.toml"),
            "test = \"py -m pytest -rA\"\nlint = \"ruff check .\"\n",
        )
        .unwrap();
        let tc = Toolchain::for_project(tmp.path()).unwrap();
        assert_eq!(tc.test, "py -m pytest -rA");
        assert_eq!(tc.lint.as_deref(), Some("ruff check ."));
        assert_eq!(
            tc.counter,
            Counter::Pytest,
            "untouched fields keep the adapter's"
        );
    }

    #[test]
    fn a_declared_toolchain_needs_no_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".smithy")).unwrap();
        std::fs::write(
            tmp.path().join(".smithy/checks.toml"),
            "test = \"make check\"\n",
        )
        .unwrap();
        let tc = Toolchain::for_project(tmp.path()).unwrap();
        assert_eq!(
            (tc.name.as_str(), tc.test.as_str(), tc.counter),
            ("custom", "make check", Counter::None)
        );
    }

    #[test]
    fn an_unknown_key_in_checks_toml_is_an_error_not_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::create_dir(tmp.path().join(".smithy")).unwrap();
        std::fs::write(tmp.path().join(".smithy/checks.toml"), "tset = \"oops\"\n").unwrap();
        assert!(Toolchain::for_project(tmp.path()).is_err());
    }
}
