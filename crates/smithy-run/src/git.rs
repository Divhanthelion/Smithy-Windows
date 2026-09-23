//! Git, as the runner uses it. The model does not.
//!
//! A Run's history is its checkpoints, so git belongs to the runner: the
//! unattended shell hook denies the model every command that moves a ref or
//! rewrites the tree (see [`model_may_run`]). Nothing here pushes, and nothing
//! here discards work — a dirty tree the runner did not expect is stashed, and
//! the stash is named in the Report.
//!
//! Plain `git` subprocesses, not the `bash` tool: no shell parsing between the
//! runner and an argument that came from a task title.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Git {
    root: PathBuf,
}

impl Git {
    pub fn open(root: &Path) -> Result<Git, String> {
        let git = Git {
            root: root.to_path_buf(),
        };
        let top = git.run(&["rev-parse", "--show-toplevel"])?;
        let top = dunce_like(Path::new(top.trim()));
        let here = dunce_like(root);
        if top != here {
            return Err(format!(
                "{} is inside the git repository at {}, not its root; a Run needs the Project to \
                 be the repository",
                root.display(),
                top
            ));
        }
        Ok(git)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!(
                "git {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    pub fn head(&self) -> Result<String, String> {
        Ok(self.run(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    pub fn branch(&self) -> Result<String, String> {
        Ok(self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    /// Tracked files with changes, staged or not.
    pub fn modified(&self) -> Result<Vec<String>, String> {
        Ok(self
            .run(&["status", "--porcelain=v1", "--untracked-files=no"])?
            .lines()
            .filter_map(porcelain_path)
            .collect())
    }

    /// Untracked files that are not ignored.
    pub fn untracked(&self) -> Result<BTreeSet<String>, String> {
        Ok(self
            .run(&["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .map(str::to_string)
            .collect())
    }

    pub fn branch_exists(&self, name: &str) -> bool {
        self.run(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .is_ok()
    }

    /// A new branch at HEAD, checked out.
    pub fn create_branch(&self, name: &str) -> Result<(), String> {
        if self.branch_exists(name) {
            return Err(format!("branch {name} already exists"));
        }
        self.run(&["switch", "-q", "-c", name]).map(|_| ())
    }

    pub fn switch(&self, name: &str) -> Result<(), String> {
        self.run(&["switch", "-q", name]).map(|_| ())
    }

    /// Commit everything in the tree except `exclude` (untracked files that
    /// were there before the Run, which are not the Run's to commit).
    ///
    /// Empty commits are allowed: a Task whose Checks already passed still
    /// gets its checkpoint, so every Task has a commit to point at.
    pub fn checkpoint(&self, message: &str, exclude: &BTreeSet<String>) -> Result<String, String> {
        self.run(&["add", "-A"])?;
        let staged: BTreeSet<String> = self
            .run(&["diff", "--cached", "--name-only"])?
            .lines()
            .map(str::to_string)
            .collect();
        let unstage: Vec<&str> = staged.intersection(exclude).map(String::as_str).collect();
        if !unstage.is_empty() {
            let mut args = vec!["reset", "-q", "--"];
            args.extend(unstage);
            self.run(&args)?;
        }
        let msg_path = self.git_dir()?.join("SMITHY_MSG");
        std::fs::write(&msg_path, message)
            .map_err(|e| format!("could not write {}: {e}", msg_path.display()))?;
        let msg = msg_path.to_string_lossy().into_owned();
        self.run(&["commit", "-q", "--allow-empty", "--no-verify", "-F", &msg])?;
        self.head()
    }

    /// Stash tracked changes and untracked files under `label`. `false` when
    /// there was nothing to stash. Pre-Run untracked files are left alone, and
    /// so is anything under `keep` (the Run's own records, which a crash must
    /// not take with it).
    pub fn stash(
        &self,
        label: &str,
        exclude: &BTreeSet<String>,
        keep: &str,
    ) -> Result<bool, String> {
        let mut paths: Vec<String> = self.modified()?;
        paths.extend(self.untracked()?.difference(exclude).cloned());
        paths.retain(|p| keep.is_empty() || !p.replace('\\', "/").starts_with(keep));
        if paths.is_empty() {
            return Ok(false);
        }
        let mut args: Vec<&str> = vec!["stash", "push", "-q", "-u", "-m", label, "--"];
        args.extend(paths.iter().map(String::as_str));
        self.run(&args)?;
        Ok(true)
    }

    /// Files changed between `base` and the working tree, untracked included.
    pub fn changed_since(&self, base: &str) -> Result<Vec<Change>, String> {
        let mut out = Vec::new();
        for line in self
            .run(&["diff", "--name-status", "--no-renames", base])?
            .lines()
        {
            let mut parts = line.splitn(2, '\t');
            let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
                continue;
            };
            let kind = match status.chars().next() {
                Some('A') => ChangeKind::Added,
                Some('D') => ChangeKind::Deleted,
                _ => ChangeKind::Modified,
            };
            out.push(Change {
                path: path.to_string(),
                kind,
            });
        }
        for path in self.untracked()? {
            if !out.iter().any(|c| c.path == path) {
                out.push(Change {
                    path,
                    kind: ChangeKind::Added,
                });
            }
        }
        Ok(out)
    }

    /// `path` as it was at `rev`, or `None` if it did not exist there.
    pub fn show(&self, rev: &str, path: &str) -> Option<String> {
        self.run(&["show", &format!("{rev}:{path}")]).ok()
    }

    /// A unified diff of `paths` from `base` to the working tree.
    pub fn diff(&self, base: &str, paths: &[&str]) -> Result<String, String> {
        let mut args = vec!["diff", "--no-color", "-U3", base, "--"];
        args.extend(paths);
        self.run(&args)
    }

    fn git_dir(&self) -> Result<PathBuf, String> {
        let dir = self.run(&["rev-parse", "--git-dir"])?;
        let dir = PathBuf::from(dir.trim());
        Ok(if dir.is_absolute() {
            dir
        } else {
            self.root.join(dir)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
}

fn porcelain_path(line: &str) -> Option<String> {
    let path = line.get(3..)?;
    // A rename is `old -> new`; the new path is the one in the tree.
    let path = path.rsplit(" -> ").next().unwrap_or(path);
    Some(path.trim_matches('"').to_string())
}

/// `C:\a\b` and `C:/a/b` compare equal, as do different cases of a drive.
fn dunce_like(p: &Path) -> String {
    let s = std::fs::canonicalize(p)
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_else(|_| p.to_string_lossy().into_owned());
    s.trim_start_matches(r"\\?\")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

/// Whether the model's `bash` may run this git command during a Run.
///
/// `Err` carries the reason it hears. Read-only git is fine and useful
/// (`status`, `diff`, `log`, `show`, `blame`, `grep`); anything that moves a
/// ref, rewrites the tree, or talks to a remote is the runner's.
pub fn model_may_run(command: &str) -> Result<(), String> {
    const RUNNER_OWNS: &[&str] = &[
        "commit",
        "checkout",
        "switch",
        "reset",
        "rebase",
        "merge",
        "stash",
        "tag",
        "push",
        "pull",
        "fetch",
        "clean",
        "restore",
        "cherry-pick",
        "revert",
        "am",
        "apply",
        "branch",
        "worktree",
        "filter-branch",
        "update-ref",
        "gc",
        "remote",
        "config",
        "rm",
        "mv",
        "init",
        "clone",
        "submodule",
    ];
    // Every `git <sub>` in the line, however it is chained or prefixed.
    let words: Vec<&str> = command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`' | '$'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut i = 0;
    while i < words.len() {
        let w = words[i].trim_matches(|c| c == '"' || c == '\'');
        let base = w.rsplit(['/', '\\']).next().unwrap_or(w);
        if base == "git" || base.eq_ignore_ascii_case("git.exe") {
            // Skip global options (`-C dir`, `-c k=v`, `--no-pager`).
            let mut j = i + 1;
            while j < words.len() && words[j].starts_with('-') {
                if matches!(words[j], "-C" | "-c") {
                    j += 1;
                }
                j += 1;
            }
            if let Some(sub) = words.get(j) {
                let is_branch_listing = *sub == "branch"
                    && words[j + 1..]
                        .iter()
                        .take_while(|w| w.starts_with('-'))
                        .all(|f| {
                            matches!(*f, "-a" | "-r" | "-v" | "-vv" | "--list" | "--show-current")
                        });
                if RUNNER_OWNS.contains(sub) && !is_branch_listing {
                    return Err(format!(
                        "`git {sub}` is not available during a Run: the runner owns git and \
                         commits for you when this Task's Checks pass. Read-only git (status, \
                         diff, log, show) is fine."
                    ));
                }
            }
            i = j;
        }
        i += 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, Git) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "run@test"]);
        git(&["config", "user.name", "run"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
        let g = Git::open(root).unwrap();
        (tmp, g)
    }

    #[test]
    fn a_subdirectory_is_not_a_run_root() {
        let (tmp, _) = repo();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        assert!(Git::open(&tmp.path().join("sub")).is_err());
    }

    #[test]
    fn checkpoints_commit_the_runs_work_and_not_what_was_already_lying_around() {
        let (tmp, g) = repo();
        std::fs::write(tmp.path().join("scratch.txt"), "the user's").unwrap();
        let before = g.untracked().unwrap();
        g.create_branch("smithy/run-1").unwrap();

        std::fs::write(tmp.path().join("a.txt"), "two\n").unwrap();
        std::fs::write(tmp.path().join("new.rs"), "fn x() {}\n").unwrap();
        let sha = g.checkpoint("T1: thing", &before).unwrap();

        assert_eq!(g.head().unwrap(), sha);
        assert!(g.modified().unwrap().is_empty());
        assert_eq!(
            g.untracked().unwrap(),
            before,
            "scratch.txt stays untracked"
        );
        assert_eq!(g.show("HEAD", "new.rs").as_deref(), Some("fn x() {}\n"));
        assert_eq!(g.branch().unwrap(), "smithy/run-1");
        let log = g.run(&["log", "-1", "--format=%s"]).unwrap();
        assert_eq!(log.trim(), "T1: thing");
    }

    #[test]
    fn an_empty_checkpoint_still_commits() {
        let (_tmp, g) = repo();
        let before = g.head().unwrap();
        let after = g.checkpoint("T2: already true", &BTreeSet::new()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn stash_keeps_the_work_and_leaves_preexisting_files() {
        let (tmp, g) = repo();
        std::fs::write(tmp.path().join("mine.txt"), "user").unwrap();
        let before = g.untracked().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "half-done\n").unwrap();
        std::fs::write(tmp.path().join("partial.rs"), "fn").unwrap();

        assert!(g
            .stash("smithy-run-1-crash-T1a1", &before, ".smithy/runs/")
            .unwrap());
        assert!(g.modified().unwrap().is_empty());
        assert_eq!(g.untracked().unwrap(), before);
        let list = g.run(&["stash", "list"]).unwrap();
        assert!(list.contains("smithy-run-1-crash-T1a1"), "{list}");
        assert!(
            !g.stash("again", &before, ".smithy/runs/").unwrap(),
            "nothing left to stash"
        );
    }

    #[test]
    fn changes_since_a_commit_include_new_and_deleted_files() {
        let (tmp, g) = repo();
        let base = g.head().unwrap();
        std::fs::remove_file(tmp.path().join("a.txt")).unwrap();
        std::fs::write(tmp.path().join("b.txt"), "b").unwrap();
        let changes = g.changed_since(&base).unwrap();
        assert!(changes.contains(&Change {
            path: "a.txt".into(),
            kind: ChangeKind::Deleted
        }));
        assert!(changes.contains(&Change {
            path: "b.txt".into(),
            kind: ChangeKind::Added
        }));
        assert_eq!(g.show(&base, "a.txt").as_deref(), Some("one\n"));
        assert_eq!(g.show(&base, "b.txt"), None);
    }

    #[test]
    fn the_model_may_read_git_but_not_move_it() {
        for ok in [
            "git status",
            "git diff HEAD~1 -- src/lib.rs",
            "git log --oneline -5",
            "git --no-pager show HEAD",
            "git branch --show-current",
            "git branch -a",
            "cargo test && git diff",
            "echo commit",
        ] {
            assert!(model_may_run(ok).is_ok(), "{ok}");
        }
        for no in [
            "git commit -am wip",
            "git -C . reset --hard",
            "cargo fmt && git checkout -- .",
            "git stash",
            "git push --force origin main",
            "git branch -D main",
            "(cd src; git clean -fd)",
            "git -c user.name=x commit -m y",
            "\"C:/Program Files/Git/cmd/git.exe\" switch main",
        ] {
            assert!(model_may_run(no).is_err(), "{no}");
        }
    }
}
