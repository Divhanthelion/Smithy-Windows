//! `cite_check` and `find_notes`: the research procedure's two tools.
//!
//! `cite_check` is how a Note stops being the model's word for it: each
//! finding's quote is looked up in the page `web_fetch` saved, or in the
//! Project file it cites. See [`crate::research`]. It only reads.
//!
//! `find_notes` is how earlier research is found again before it is redone.

use async_trait::async_trait;
use serde_json::Value;

use crate::registry::{Tool, ToolCtx};
use crate::research::{check_note, find_notes, list_notes, SourceStore, NOTES_DIR};
use crate::schema::{arg_str, arg_str_opt, ToolDefinition, ToolOutput, ToolParameter};

pub struct CiteCheck {
    store: Option<SourceStore>,
}

impl CiteCheck {
    pub fn new() -> CiteCheck {
        CiteCheck {
            store: SourceStore::default_location(),
        }
    }

    pub fn with_store(store: SourceStore) -> CiteCheck {
        CiteCheck { store: Some(store) }
    }
}

impl Default for CiteCheck {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CiteCheck {
    fn name(&self) -> &'static str {
        "cite_check"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "cite_check",
            "Verify a research Note's findings against the pages `web_fetch` saved and the \
             Project's files. Each finding under `## Findings` is a bullet like \
             `- [spec] (key) claim — https://url {src:<id>} \"a quote copied exactly from the \
             page\"` (or `{repo:path/file.rs:42}` for this Project). Reports, per finding, whether \
             the source exists, the URL matches what was fetched, and every quote is really on \
             the page (`...` may elide), and whether the (key) findings rest on two domains. \
             Fix or drop what fails, then run it again. Reads only.",
            vec![ToolParameter::string(
                "path",
                "The Note, Project-relative (usually .smithy/research/<date>-<slug>.md).",
                true,
            )],
        )
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutput {
        let path = match arg_str(args, "path") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let Some(store) = &self.store else {
            return ToolOutput::err("no source store: saved pages have no home directory here");
        };
        let text = match ctx.workspace.read_to_string(path) {
            Ok(t) => t,
            Err(e) => return ToolOutput::err(e),
        };
        ToolOutput::ok(check_note(&text, store, ctx.workspace.root()).render())
    }
}

pub struct FindNotes;

#[async_trait]
impl Tool for FindNotes {
    fn name(&self) -> &'static str {
        "find_notes"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "find_notes",
            "Find research Notes already written in this Project (.smithy/research/) by their \
             question. Check before researching something: if a Note answers it, `read` the \
             Note instead of searching again. Without a query, lists every Note.",
            vec![ToolParameter::string(
                "query",
                "What you want to know, in a few words.",
                false,
            )],
        )
    }

    async fn run(&self, args: &Value, ctx: &ToolCtx) -> ToolOutput {
        let root = ctx.workspace.root();
        let rows: Vec<(Option<f64>, crate::research::NoteMeta)> = match arg_str_opt(args, "query")
            .map(str::trim)
            .filter(|q| !q.is_empty())
        {
            Some(q) => find_notes(root, q, 8)
                .into_iter()
                .map(|(s, n)| (Some(s), n))
                .collect(),
            None => list_notes(root).into_iter().map(|n| (None, n)).collect(),
        };
        if rows.is_empty() {
            return ToolOutput::ok(format!("No matching Notes in {NOTES_DIR}/."));
        }
        let mut out = String::new();
        for (score, n) in rows {
            let status = if n.status.is_empty() {
                String::new()
            } else {
                format!(" [{}]", n.status)
            };
            let score = score
                .map(|s| format!(" (overlap {s:.2})"))
                .unwrap_or_default();
            out.push_str(&format!("{}{status}{score}\n  {}\n", n.path, n.question));
        }
        ToolOutput::ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::Workspace;

    #[tokio::test]
    async fn cite_check_reports_per_finding_and_a_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SourceStore::new(tmp.path().join("sources"));
        let page = "Tokio's runtime is a work-stealing scheduler with one queue per worker thread.";
        let meta = store
            .save(
                "https://tokio.rs/blog",
                "https://tokio.rs/blog",
                200,
                "text/html",
                page,
            )
            .unwrap();
        let project = tmp.path().join("p");
        std::fs::create_dir_all(project.join(NOTES_DIR)).unwrap();
        std::fs::write(
            project.join(NOTES_DIR).join("n.md"),
            format!(
                "# q\n\n## Findings\n- [owner] (key) work stealing — https://tokio.rs/blog {{src:{}}} \"a work-stealing scheduler with one queue\"\n",
                meta.id
            ),
        )
        .unwrap();
        let ctx = ToolCtx::new(Workspace::open(&project).unwrap());
        let out = CiteCheck::with_store(store)
            .run(&serde_json::json!({"path": ".smithy/research/n.md"}), &ctx)
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("✓ line 4 [owner] (key) verified"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("FAIL"),
            "one domain is not enough: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn find_notes_lists_and_searches() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(NOTES_DIR)).unwrap();
        std::fs::write(
            tmp.path().join(NOTES_DIR).join("a.md"),
            "# How are ISO 8601 durations written?\n\n**Status:** verified\n",
        )
        .unwrap();
        let ctx = ToolCtx::new(Workspace::open(tmp.path()).unwrap());
        let all = FindNotes.run(&serde_json::json!({}), &ctx).await;
        assert!(
            all.content.contains(".smithy/research/a.md [verified]"),
            "{}",
            all.content
        );
        let hit = FindNotes
            .run(&serde_json::json!({"query": "duration syntax iso"}), &ctx)
            .await;
        assert!(hit.content.contains("overlap"), "{}", hit.content);
        let miss = FindNotes
            .run(&serde_json::json!({"query": "tls ciphers"}), &ctx)
            .await;
        assert!(miss.content.starts_with("No matching"), "{}", miss.content);
    }
}
