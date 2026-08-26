//! A lightweight semantic layer parsed from Gauge-style markdown spec files.
//!
//! Research on enterprise NL->SQL is consistent: the model's failures come from
//! business meaning, not SQL syntax, and the cure is a semantic layer that maps
//! business terms to verified query fragments. A `.spec.md` file does double
//! duty here:
//!   1. Grounding — glossary terms and verified example queries are injected
//!      into the system prompt so the model reuses known-good SQL.
//!   2. Test design (Gauge-style) — each example can carry an `Expect:` line, so
//!      `pg-mcp-agent verify <dir>` runs every example against the real database
//!      and reports pass/fail. Verified specs stay honest as the schema changes.
//!
//! ## Format
//! ```markdown
//! # Orders analytics
//!
//! ## Glossary
//! - **active customer**: a customer with an order in the last 90 days
//! - **revenue**: SUM(order_items.quantity * order_items.unit_price)
//!
//! ## Example: monthly revenue
//! Question: revenue by month
//! Question: how much did we make each month
//! Expect: contains revenue
//! Backend: clickhouse        (optional; default postgres)
//! Engine: SummingMergeTree() (optional; ClickHouse `materialize` engine)
//! Order by: month            (optional; ClickHouse MV sorting key)
//! ```sql
//! SELECT date_trunc('month', o.created_at) AS month,
//!        SUM(oi.quantity * oi.unit_price) AS revenue
//! FROM orders o JOIN order_items oi ON oi.order_id = o.id
//! GROUP BY 1 ORDER BY 1;
//! ```
//! ```

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

use crate::mcp::Tool;
use crate::router::McpRouter;

#[derive(Debug, Clone)]
pub struct GlossaryEntry {
    pub term: String,
    pub definition: String,
}

#[derive(Debug, Clone)]
pub enum Expectation {
    /// The query executes without error (default).
    RunsOk,
    /// The result is non-empty (not blank, not "no rows").
    NonEmpty,
    /// The result text contains this substring (case-insensitive).
    Contains(String),
}

impl Expectation {
    /// A human-readable phrasing, used in verify output and triage context.
    pub fn describe(&self) -> String {
        match self {
            Expectation::RunsOk => "runs without error".to_string(),
            Expectation::NonEmpty => "returns a non-empty result".to_string(),
            Expectation::Contains(s) => format!("result contains `{s}`"),
        }
    }
}

/// Which backend a metric materializes to. Drives `materialize` DDL generation
/// (Postgres `REFRESH` MVs vs ClickHouse incremental `ENGINE` MVs) and, once
/// per-server dialect lands, which server `verify` routes the query to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    #[default]
    Postgres,
    ClickHouse,
}

impl Backend {
    pub fn parse(s: &str) -> Backend {
        match s.trim().to_lowercase().as_str() {
            "clickhouse" | "ch" => Backend::ClickHouse,
            _ => Backend::Postgres,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Backend::Postgres => "postgres",
            Backend::ClickHouse => "clickhouse",
        }
    }
}

#[derive(Debug, Clone)]
pub struct QuerySpec {
    pub name: String,
    pub questions: Vec<String>,
    pub sql: String,
    pub expect: Expectation,
    /// Target backend for materialization (default Postgres).
    pub backend: Backend,
    /// ClickHouse only: table engine for the materialized view (e.g.
    /// `SummingMergeTree()`). Ignored for Postgres.
    pub engine: Option<String>,
    /// ClickHouse only: the `ORDER BY` sorting key for the MV's storage.
    pub order_by: Option<String>,
    /// If set, this spec joins a cross-engine parity group with this key: every
    /// spec sharing the key must compute to the same scalar. See `parity`.
    pub parity_key: Option<String>,
}

#[derive(Debug, Default, Clone)]
pub struct SemanticLayer {
    pub glossary: Vec<GlossaryEntry>,
    pub specs: Vec<QuerySpec>,
}

impl SemanticLayer {
    pub fn is_empty(&self) -> bool {
        self.glossary.is_empty() && self.specs.is_empty()
    }

    /// Load and merge every `*.spec.md` file in `dir`. Missing dir → empty layer.
    pub fn load_dir(dir: &Path) -> Result<Self> {
        let mut layer = SemanticLayer::default();
        if !dir.exists() {
            return Ok(layer);
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading specs dir {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with(".spec.md"))
            .collect();
        entries.sort();
        for path in entries {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let parsed = parse(&text);
            layer.glossary.extend(parsed.glossary);
            layer.specs.extend(parsed.specs);
        }
        Ok(layer)
    }

    /// Render the layer as a system-prompt section (grounding + few-shot).
    pub fn to_prompt(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from(
            "\nSEMANTIC LAYER (authoritative business definitions — prefer these over guessing):\n",
        );
        if !self.glossary.is_empty() {
            s.push_str("\nGlossary:\n");
            for g in &self.glossary {
                s.push_str(&format!("- {}: {}\n", g.term, g.definition));
            }
        }
        if !self.specs.is_empty() {
            s.push_str(
                "\nVerified example queries (reuse and adapt these; they are known-correct):\n",
            );
            for spec in &self.specs {
                let q = spec
                    .questions
                    .first()
                    .cloned()
                    .unwrap_or_else(|| spec.name.clone());
                s.push_str(&format!("\n# {q}\n{}\n", spec.sql.trim()));
            }
        }
        s
    }

    /// Run every spec against the database and collect a structured report.
    /// Each spec routes to a SQL tool on the server matching its backend, so a
    /// `Backend: clickhouse` spec runs against ClickHouse. The report drives both
    /// the human-readable CLI output and the JSON the spec-triage agent consumes.
    pub async fn verify(&self, router: &mut McpRouter) -> Result<VerifyReport> {
        let mut report = VerifyReport::default();
        for spec in &self.specs {
            let (tool_name, schema) = match router.sql_tool_for_dialect(spec.backend.into()) {
                Some(t) => t,
                None => {
                    anyhow::bail!("no SQL-executing tool found on any server; can't verify specs")
                }
            };
            let args = fill_sql_arg(&schema, &spec.sql);
            let outcome = router.call_tool(&tool_name, args).await;
            let (passed, detail) = check(&spec.expect, &outcome);
            let output_excerpt = match &outcome {
                Ok(text) => excerpt(text),
                Err(e) => format!("error: {e}"),
            };
            report.results.push(SpecResult {
                name: spec.name.clone(),
                backend: spec.backend,
                tool: tool_name,
                sql: spec.sql.clone(),
                expect: spec.expect.describe(),
                passed,
                detail: if passed { String::new() } else { detail },
                output_excerpt,
            });
        }
        Ok(report)
    }

    /// Cross-engine parity: group specs by their `Parity:` key, run each on the
    /// server matching its backend, extract a scalar, and check every member of
    /// a group agrees within tolerance. This is the check a CDC pipe can't give
    /// you — proof that the ClickHouse rollup still equals the Postgres source.
    pub async fn verify_parity(
        &self,
        router: &mut McpRouter,
        tolerance: f64,
    ) -> Result<crate::parity::ParityReport> {
        use crate::parity::{ParityGroup, ParityMember, ParityReport};

        // Preserve first-seen order of keys so output is stable.
        let mut order: Vec<String> = Vec::new();
        let mut groups: std::collections::HashMap<String, Vec<ParityMember>> =
            std::collections::HashMap::new();

        for spec in &self.specs {
            let Some(key) = &spec.parity_key else {
                continue;
            };
            if !groups.contains_key(key) {
                order.push(key.clone());
            }
            let (tool_name, schema) = match router.sql_tool_for_dialect(spec.backend.into()) {
                Some(t) => t,
                None => anyhow::bail!(
                    "no SQL tool for backend {} (parity spec `{}`)",
                    spec.backend.as_str(),
                    spec.name
                ),
            };
            let args = fill_sql_arg(&schema, &spec.sql);
            let outcome = router.call_tool(&tool_name, args).await;
            let (value, excerpt) = match &outcome {
                Ok(text) => (crate::parity::extract_number(text), excerpt(text)),
                Err(e) => (None, format!("error: {e}")),
            };
            groups.entry(key.clone()).or_default().push(ParityMember {
                name: spec.name.clone(),
                backend: spec.backend.as_str().to_string(),
                value,
                excerpt,
            });
        }

        let report_groups = order
            .into_iter()
            .map(|key| {
                let members = groups.remove(&key).unwrap_or_default();
                ParityGroup { key, members }
            })
            .collect();
        Ok(ParityReport::new(report_groups, tolerance))
    }
}

/// The outcome of verifying one spec — enough context for a human or the
/// spec-triage agent to act without re-running anything.
#[derive(Debug, Clone)]
pub struct SpecResult {
    pub name: String,
    pub backend: Backend,
    /// The exposed tool the query actually ran through (e.g. `run_select_query`).
    pub tool: String,
    pub sql: String,
    /// Human-readable assertion (e.g. "result contains `revenue`").
    pub expect: String,
    pub passed: bool,
    /// Why it failed (empty when it passed).
    pub detail: String,
    /// A truncated slice of the tool output (or the error) for diagnosis.
    pub output_excerpt: String,
}

/// Structured result of a `verify` run.
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub results: Vec<SpecResult>,
}

impl VerifyReport {
    pub fn total(&self) -> usize {
        self.results.len()
    }

    pub fn failures(&self) -> usize {
        self.results.iter().filter(|r| !r.passed).count()
    }

    pub fn passed(&self) -> usize {
        self.total() - self.failures()
    }

    /// The human-readable PASS/FAIL report printed by `pg-mcp-agent verify`.
    pub fn to_human(&self) -> String {
        let mut s = format!("Verifying {} spec(s)\n\n", self.total());
        for r in &self.results {
            if r.passed {
                s.push_str(&format!("  PASS  {}  [{}]\n", r.name, r.tool));
            } else {
                s.push_str(&format!(
                    "  FAIL  {}  [{}]  — {}\n",
                    r.name, r.tool, r.detail
                ));
            }
        }
        s.push_str(&format!(
            "\n{} passed, {} failed\n",
            self.passed(),
            self.failures()
        ));
        s
    }

    /// Machine-readable report for the spec-triage agent (only failures carry
    /// full SQL/output, to keep the payload focused on what needs fixing).
    pub fn to_json(&self) -> String {
        let results: Vec<Value> = self
            .results
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "backend": r.backend.as_str(),
                    "tool": r.tool,
                    "passed": r.passed,
                    "expect": r.expect,
                    "detail": r.detail,
                    "sql": r.sql,
                    "output_excerpt": r.output_excerpt,
                })
            })
            .collect();
        let value = json!({
            "summary": {
                "total": self.total(),
                "passed": self.passed(),
                "failed": self.failures(),
            },
            "results": results,
        });
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Truncate long tool output to a bounded excerpt for triage context.
fn excerpt(text: &str) -> String {
    const MAX: usize = 600;
    let t = text.trim();
    if t.len() <= MAX {
        t.to_string()
    } else {
        let mut cut = MAX;
        while !t.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}… (truncated, {} bytes total)", &t[..cut], t.len())
    }
}

fn check(expect: &Expectation, outcome: &Result<String>) -> (bool, String) {
    match outcome {
        Err(e) => (false, format!("query errored: {e}")),
        Ok(text) => match expect {
            Expectation::RunsOk => (true, String::new()),
            Expectation::NonEmpty => {
                let t = text.trim();
                let empty = t.is_empty()
                    || t.eq_ignore_ascii_case("no rows")
                    || t.eq_ignore_ascii_case("[]");
                (!empty, "expected non-empty result".into())
            }
            Expectation::Contains(sub) => {
                let hit = text.to_lowercase().contains(&sub.to_lowercase());
                (hit, format!("expected result to contain `{sub}`"))
            }
        },
    }
}

/// Heuristic: does this tool name look like a SQL-executing tool?
/// Covers Postgres (`execute_sql`/`query`) and ClickHouse (`run_select_query`,
/// `run_query`) MCP servers.
pub fn is_sql_tool(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("sql") || n.contains("query")
}

/// Choose a tool that executes SQL: prefer names hinting at SQL execution.
pub fn pick_sql_tool(tools: &[Tool]) -> Option<&Tool> {
    tools
        .iter()
        .find(|t| is_sql_tool(&t.name))
        .or_else(|| tools.first())
}

/// Build tool arguments carrying `sql`, matching the tool's input schema key.
pub fn fill_sql_arg(schema: &Value, sql: &str) -> Value {
    let key = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|props| {
            for candidate in ["sql", "query", "statement", "command"] {
                if props.contains_key(candidate) {
                    return Some(candidate.to_string());
                }
            }
            // else: the single string-typed property, if there's exactly one
            let string_props: Vec<&String> = props
                .iter()
                .filter(|(_, v)| v.get("type").and_then(Value::as_str) == Some("string"))
                .map(|(k, _)| k)
                .collect();
            if string_props.len() == 1 {
                Some(string_props[0].clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "sql".to_string());
    json!({ key: sql })
}

// --- markdown parsing ------------------------------------------------------

/// Parse a single spec document's text into a [`SemanticLayer`].
pub fn parse_str(text: &str) -> SemanticLayer {
    parse(text)
}

fn parse(text: &str) -> SemanticLayer {
    let mut layer = SemanticLayer::default();
    let mut in_glossary = false;
    let mut current: Option<QuerySpec> = None;
    let mut in_sql = false;
    let mut sql_buf = String::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Fenced SQL block boundaries.
        if trimmed.starts_with("```") {
            let fence = trimmed.trim_start_matches('`').trim();
            if !in_sql
                && (fence.eq_ignore_ascii_case("sql") || fence.is_empty())
                && current.is_some()
            {
                in_sql = true;
                sql_buf.clear();
                continue;
            } else if in_sql {
                in_sql = false;
                if let Some(spec) = current.as_mut() {
                    spec.sql = sql_buf.trim().to_string();
                }
                continue;
            }
        }
        if in_sql {
            sql_buf.push_str(line);
            sql_buf.push('\n');
            continue;
        }

        // Headings switch sections.
        if let Some(heading) = trimmed
            .strip_prefix("## ")
            .or_else(|| trimmed.strip_prefix("# "))
        {
            // Close any spec we were building.
            flush(&mut current, &mut layer);
            let lower = heading.to_lowercase();
            if lower.starts_with("glossary") {
                in_glossary = true;
            } else if let Some(name) = heading
                .strip_prefix("Example:")
                .or_else(|| heading.strip_prefix("Metric:"))
                .or_else(|| heading.strip_prefix("example:"))
                .or_else(|| heading.strip_prefix("metric:"))
            {
                in_glossary = false;
                current = Some(QuerySpec {
                    name: name.trim().to_string(),
                    questions: Vec::new(),
                    sql: String::new(),
                    expect: Expectation::RunsOk,
                    backend: Backend::default(),
                    engine: None,
                    order_by: None,
                    parity_key: None,
                });
            } else {
                in_glossary = false;
            }
            continue;
        }

        // Glossary bullet: - **term**: definition
        if in_glossary {
            if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                if let Some((term, def)) = rest.split_once(':') {
                    layer.glossary.push(GlossaryEntry {
                        term: clean_term(term),
                        definition: def.trim().to_string(),
                    });
                }
            }
            continue;
        }

        // Spec metadata lines.
        if let Some(spec) = current.as_mut() {
            if let Some(q) = trimmed
                .strip_prefix("Question:")
                .or_else(|| trimmed.strip_prefix("Plain:"))
            {
                spec.questions.push(q.trim().to_string());
            } else if let Some(e) = trimmed.strip_prefix("Expect:") {
                spec.expect = parse_expectation(e.trim());
            } else if let Some(b) = trimmed.strip_prefix("Backend:") {
                spec.backend = Backend::parse(b);
            } else if let Some(eng) = trimmed.strip_prefix("Engine:") {
                spec.engine = Some(eng.trim().to_string());
            } else if let Some(ob) = trimmed
                .strip_prefix("Order by:")
                .or_else(|| trimmed.strip_prefix("Order By:"))
                .or_else(|| trimmed.strip_prefix("Order:"))
            {
                spec.order_by = Some(ob.trim().to_string());
            } else if let Some(pk) = trimmed
                .strip_prefix("Parity:")
                .or_else(|| trimmed.strip_prefix("parity:"))
            {
                spec.parity_key = Some(pk.trim().to_string());
            }
        }
    }

    flush(&mut current, &mut layer);
    layer
}

fn flush(current: &mut Option<QuerySpec>, layer: &mut SemanticLayer) {
    if let Some(spec) = current.take() {
        if !spec.sql.is_empty() {
            layer.specs.push(spec);
        }
    }
}

fn parse_expectation(s: &str) -> Expectation {
    let lower = s.to_lowercase();
    if lower == "runs" || lower == "runs-ok" || lower == "ok" {
        Expectation::RunsOk
    } else if lower == "non-empty" || lower == "nonempty" || lower == "not-empty" {
        Expectation::NonEmpty
    } else if let Some(rest) = lower.strip_prefix("contains") {
        Expectation::Contains(rest.trim_start_matches(':').trim().to_string())
    } else {
        Expectation::Contains(s.to_string())
    }
}

fn clean_term(t: &str) -> String {
    t.trim().trim_matches('*').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r#"
# Orders

## Glossary
- **active customer**: a customer with an order in the last 90 days
- **revenue**: sum of quantity times unit price

## Example: monthly revenue
Question: revenue by month
Question: how much each month
Expect: contains revenue
```sql
SELECT date_trunc('month', created_at) AS month, sum(total) AS revenue
FROM orders GROUP BY 1 ORDER BY 1;
```
"#;

    #[test]
    fn parses_glossary_and_spec() {
        let layer = parse(SPEC);
        assert_eq!(layer.glossary.len(), 2);
        assert_eq!(layer.glossary[0].term, "active customer");
        assert_eq!(layer.specs.len(), 1);
        let spec = &layer.specs[0];
        assert_eq!(spec.name, "monthly revenue");
        assert_eq!(spec.questions.len(), 2);
        assert!(spec.sql.contains("date_trunc"));
        matches!(spec.expect, Expectation::Contains(_));
    }

    #[test]
    fn prompt_includes_examples() {
        let layer = parse(SPEC);
        let p = layer.to_prompt();
        assert!(p.contains("SEMANTIC LAYER"));
        assert!(p.contains("revenue by month"));
        assert!(p.contains("date_trunc"));
    }

    #[test]
    fn parses_backend_and_clickhouse_engine() {
        let text = "## Metric: daily events\nBackend: clickhouse\nEngine: SummingMergeTree()\nOrder by: day\n```sql\nSELECT day, count() FROM events GROUP BY day;\n```";
        let layer = parse(text);
        let spec = &layer.specs[0];
        assert_eq!(spec.backend, Backend::ClickHouse);
        assert_eq!(spec.engine.as_deref(), Some("SummingMergeTree()"));
        assert_eq!(spec.order_by.as_deref(), Some("day"));
    }

    #[test]
    fn backend_defaults_to_postgres() {
        let layer = parse("## Metric: m\n```sql\nSELECT 1;\n```");
        assert_eq!(layer.specs[0].backend, Backend::Postgres);
    }

    fn sample_report() -> VerifyReport {
        VerifyReport {
            results: vec![
                SpecResult {
                    name: "sales by region".into(),
                    backend: Backend::Postgres,
                    tool: "execute_sql".into(),
                    sql: "SELECT region FROM sales".into(),
                    expect: "result contains `east`".into(),
                    passed: true,
                    detail: String::new(),
                    output_excerpt: "[{\"region\":\"east\"}]".into(),
                },
                SpecResult {
                    name: "daily revenue".into(),
                    backend: Backend::ClickHouse,
                    tool: "run_select_query".into(),
                    sql: "SELECT day, revenue FROM t".into(),
                    expect: "result contains `revenue`".into(),
                    passed: false,
                    detail: "expected result to contain `revenue`".into(),
                    output_excerpt: "[{\"day\":\"2026-01-01\"}]".into(),
                },
            ],
        }
    }

    #[test]
    fn report_counts_are_correct() {
        let r = sample_report();
        assert_eq!(r.total(), 2);
        assert_eq!(r.passed(), 1);
        assert_eq!(r.failures(), 1);
    }

    #[test]
    fn report_human_shows_pass_and_fail() {
        let human = sample_report().to_human();
        assert!(human.contains("PASS  sales by region  [execute_sql]"));
        assert!(human.contains("FAIL  daily revenue  [run_select_query]  — expected"));
        assert!(human.contains("1 passed, 1 failed"));
    }

    #[test]
    fn report_json_is_structured_for_triage() {
        let json_str = sample_report().to_json();
        let v: Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(v["summary"]["failed"], 1);
        assert_eq!(v["results"][1]["backend"], "clickhouse");
        assert_eq!(v["results"][1]["passed"], false);
        // Failing result carries the SQL and the excerpt the agent needs.
        assert!(v["results"][1]["sql"].as_str().unwrap().contains("SELECT"));
        assert!(v["results"][1]["detail"]
            .as_str()
            .unwrap()
            .contains("revenue"));
    }

    #[test]
    fn excerpt_truncates_long_output() {
        let long = "x".repeat(5000);
        let e = excerpt(&long);
        assert!(e.len() < 700);
        assert!(e.contains("truncated"));
    }

    #[test]
    fn expectation_describe_is_human_readable() {
        assert_eq!(
            Expectation::Contains("revenue".into()).describe(),
            "result contains `revenue`"
        );
        assert_eq!(
            Expectation::NonEmpty.describe(),
            "returns a non-empty result"
        );
    }

    #[test]
    fn fill_sql_arg_matches_query_key() {
        let schema = json!({ "properties": { "query": { "type": "string" } } });
        let v = fill_sql_arg(&schema, "SELECT 1");
        assert_eq!(v.get("query").and_then(Value::as_str), Some("SELECT 1"));
    }
}
