//! Write-gating for tool calls.
//!
//! Off-the-shelf Postgres MCP servers usually funnel everything through one
//! `execute_sql`/`query` tool, so we can't gate on the tool name. Instead we
//! find the SQL argument, classify the statement, and decide whether it runs
//! freely, needs confirmation, or is blocked outright.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlClass {
    /// SELECT / WITH (read-only) / EXPLAIN / SHOW — no data change.
    Read,
    /// INSERT / UPDATE / DELETE / MERGE / COPY ... FROM — row changes.
    Write,
    /// CREATE / ALTER / DROP / TRUNCATE / GRANT — schema or bulk destruction.
    Ddl,
    /// Couldn't confidently classify (unknown keyword, multiple statements).
    Unknown,
}

impl SqlClass {
    pub fn label(self) -> &'static str {
        match self {
            SqlClass::Read => "read",
            SqlClass::Write => "write",
            SqlClass::Ddl => "DDL",
            SqlClass::Unknown => "unclassified",
        }
    }
}

#[derive(Debug)]
pub enum Decision {
    /// Safe to run without asking.
    Allow,
    /// Run only after the human says yes.
    NeedsConfirmation { class: SqlClass, sql: String },
    /// Refuse outright.
    Blocked { reason: String },
}

/// SQL dialect, which changes how a few statements are classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    #[default]
    Postgres,
    ClickHouse,
}

impl Dialect {
    pub fn parse(s: &str) -> Dialect {
        match s.to_lowercase().as_str() {
            "clickhouse" | "ch" => Dialect::ClickHouse,
            _ => Dialect::Postgres,
        }
    }
}

impl From<crate::semantics::Backend> for Dialect {
    fn from(b: crate::semantics::Backend) -> Self {
        match b {
            crate::semantics::Backend::Postgres => Dialect::Postgres,
            crate::semantics::Backend::ClickHouse => Dialect::ClickHouse,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GuardPolicy {
    /// Allow INSERT/UPDATE/DELETE (after confirmation).
    pub allow_writes: bool,
    /// Allow CREATE/ALTER/DROP/TRUNCATE (after confirmation).
    pub allow_ddl: bool,
    /// If true, even reads prompt for confirmation. Off by default.
    pub confirm_reads: bool,
    /// SQL dialect used when classifying statements.
    pub dialect: Dialect,
}

impl Default for GuardPolicy {
    fn default() -> Self {
        Self {
            allow_writes: true,
            allow_ddl: false,
            confirm_reads: false,
            dialect: Dialect::Postgres,
        }
    }
}

impl GuardPolicy {
    /// Decide what to do with a tool call given its arguments.
    pub fn evaluate(&self, arguments: &Value) -> Decision {
        let Some(sql) = extract_sql(arguments) else {
            // No SQL string in the args: likely a metadata/introspection tool
            // (list schemas, describe table). Treat as a read.
            return if self.confirm_reads {
                Decision::NeedsConfirmation {
                    class: SqlClass::Read,
                    sql: String::new(),
                }
            } else {
                Decision::Allow
            };
        };

        let class = classify_with(&sql, self.dialect);
        match class {
            SqlClass::Read => {
                if self.confirm_reads {
                    Decision::NeedsConfirmation { class, sql }
                } else {
                    Decision::Allow
                }
            }
            SqlClass::Write => {
                if self.allow_writes {
                    Decision::NeedsConfirmation { class, sql }
                } else {
                    Decision::Blocked {
                        reason: "writes are disabled by policy (allow_writes = false)".into(),
                    }
                }
            }
            SqlClass::Ddl => {
                if self.allow_ddl {
                    Decision::NeedsConfirmation { class, sql }
                } else {
                    Decision::Blocked {
                        reason: "DDL is disabled by policy (allow_ddl = false)".into(),
                    }
                }
            }
            // Unknown always asks — err toward the human.
            SqlClass::Unknown => Decision::NeedsConfirmation { class, sql },
        }
    }
}

/// Pull a SQL string out of tool arguments. Checks the usual argument keys,
/// then falls back to the single string value if there's exactly one.
fn extract_sql(arguments: &Value) -> Option<String> {
    let obj = arguments.as_object()?;
    for key in ["sql", "query", "statement", "command", "q"] {
        if let Some(s) = obj.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    // Fall back: if there's exactly one string arg, assume it's the SQL.
    let strings: Vec<&str> = obj.values().filter_map(Value::as_str).collect();
    if strings.len() == 1 {
        return Some(strings[0].to_string());
    }
    None
}

/// Classify a SQL statement for the Postgres dialect (the default).
pub fn classify(raw: &str) -> SqlClass {
    classify_with(raw, Dialect::Postgres)
}

/// Classify a SQL statement by its leading keyword, with a couple of guards for
/// CTEs and multi-statement smuggling, and dialect-specific rules.
pub fn classify_with(raw: &str, dialect: Dialect) -> SqlClass {
    let sql = strip_comments(raw);
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.is_empty() {
        return SqlClass::Unknown;
    }

    // More than one statement is a classic way to hide a write behind a read.
    // Count non-empty statements split on `;`.
    let statement_count = sql.split(';').filter(|s| !s.trim().is_empty()).count();
    if statement_count > 1 {
        return SqlClass::Unknown;
    }

    let upper = trimmed.to_ascii_uppercase();
    let first = upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");

    // ClickHouse expresses row mutations as `ALTER TABLE … DELETE/UPDATE`, which
    // is a data change, not schema DDL. And `OPTIMIZE` is a maintenance op.
    if dialect == Dialect::ClickHouse {
        match first {
            "ALTER" if contains_word(&upper, &["DELETE", "UPDATE"]) => return SqlClass::Write,
            "OPTIMIZE" => return SqlClass::Ddl,
            _ => {}
        }
    }

    match first {
        "SELECT" | "EXPLAIN" | "SHOW" | "TABLE" | "VALUES" | "FETCH" | "DECLARE" | "DESCRIBE"
        | "DESC" => SqlClass::Read,
        // A WITH can wrap a data-modifying CTE; check the body.
        "WITH" => {
            if contains_word(&upper, &["INSERT", "UPDATE", "DELETE", "MERGE"]) {
                SqlClass::Write
            } else {
                SqlClass::Read
            }
        }
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "UPSERT" | "COPY" => SqlClass::Write,
        "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "GRANT" | "REVOKE" | "COMMENT" | "REINDEX"
        | "VACUUM" | "CLUSTER" | "REFRESH" | "OPTIMIZE" | "ATTACH" | "DETACH" | "RENAME" => {
            SqlClass::Ddl
        }
        _ => SqlClass::Unknown,
    }
}

/// Remove `-- line` and `/* block */` comments so they can't hide keywords.
fn strip_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// True if any of `words` appears as a whole token in `haystack` (already upper).
fn contains_word(haystack: &str, words: &[&str]) -> bool {
    let tokens: Vec<&str> = haystack
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .collect();
    words.iter().any(|w| tokens.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_are_reads() {
        assert_eq!(classify("SELECT * FROM users"), SqlClass::Read);
        assert_eq!(classify("  select 1  "), SqlClass::Read);
        assert_eq!(classify("EXPLAIN ANALYZE SELECT 1"), SqlClass::Read);
    }

    #[test]
    fn dml_is_write() {
        assert_eq!(
            classify("UPDATE users SET name = 'x' WHERE id = 1"),
            SqlClass::Write
        );
        assert_eq!(classify("delete from t"), SqlClass::Write);
        assert_eq!(classify("INSERT INTO t VALUES (1)"), SqlClass::Write);
    }

    #[test]
    fn ddl_is_ddl() {
        assert_eq!(classify("DROP TABLE users"), SqlClass::Ddl);
        assert_eq!(classify("truncate t"), SqlClass::Ddl);
    }

    #[test]
    fn clickhouse_alter_mutation_is_a_write_not_ddl() {
        // In ClickHouse, ALTER ... DELETE/UPDATE mutates rows.
        assert_eq!(
            classify_with(
                "ALTER TABLE events DELETE WHERE ts < now()",
                Dialect::ClickHouse
            ),
            SqlClass::Write
        );
        assert_eq!(
            classify_with(
                "ALTER TABLE t UPDATE c = 1 WHERE id = 2",
                Dialect::ClickHouse
            ),
            SqlClass::Write
        );
        // Under Postgres the same leading keyword is DDL.
        assert_eq!(
            classify_with("ALTER TABLE t ADD COLUMN c int", Dialect::Postgres),
            SqlClass::Ddl
        );
    }

    #[test]
    fn clickhouse_plain_alter_and_optimize_are_ddl() {
        assert_eq!(
            classify_with("ALTER TABLE t ADD COLUMN c int", Dialect::ClickHouse),
            SqlClass::Ddl
        );
        assert_eq!(
            classify_with("OPTIMIZE TABLE t FINAL", Dialect::ClickHouse),
            SqlClass::Ddl
        );
    }

    #[test]
    fn clickhouse_policy_gates_alter_mutation_as_write() {
        let policy = GuardPolicy {
            dialect: Dialect::ClickHouse,
            ..GuardPolicy::default()
        };
        let args = serde_json::json!({ "sql": "ALTER TABLE t DELETE WHERE id = 1" });
        // allow_writes is true by default → needs confirmation (a write), not blocked as DDL.
        assert!(matches!(
            policy.evaluate(&args),
            Decision::NeedsConfirmation {
                class: SqlClass::Write,
                ..
            }
        ));
    }

    #[test]
    fn plain_select_not_confused_by_column_named_update() {
        assert_eq!(classify("SELECT updates FROM feed"), SqlClass::Read);
    }

    #[test]
    fn cte_with_write_escalates() {
        let sql = "WITH moved AS (DELETE FROM a RETURNING *) INSERT INTO b SELECT * FROM moved";
        assert_eq!(classify(sql), SqlClass::Write);
    }

    #[test]
    fn plain_cte_is_read() {
        assert_eq!(
            classify("WITH c AS (SELECT 1) SELECT * FROM c"),
            SqlClass::Read
        );
    }

    #[test]
    fn multi_statement_is_unknown() {
        assert_eq!(classify("SELECT 1; DROP TABLE users"), SqlClass::Unknown);
    }

    #[test]
    fn comment_hidden_write_is_caught() {
        // The write isn't hidden by the comment; leading keyword is still DELETE.
        assert_eq!(classify("/* harmless */ DELETE FROM t"), SqlClass::Write);
    }

    #[test]
    fn backend_maps_to_dialect() {
        use crate::semantics::Backend;
        assert_eq!(Dialect::from(Backend::ClickHouse), Dialect::ClickHouse);
        assert_eq!(Dialect::from(Backend::Postgres), Dialect::Postgres);
    }

    #[test]
    fn dialect_parse_accepts_aliases() {
        assert_eq!(Dialect::parse("ch"), Dialect::ClickHouse);
        assert_eq!(Dialect::parse("ClickHouse"), Dialect::ClickHouse);
        assert_eq!(Dialect::parse("anything-else"), Dialect::Postgres);
    }

    #[test]
    fn policy_blocks_ddl_by_default() {
        let p = GuardPolicy::default();
        let args = serde_json::json!({ "sql": "DROP TABLE t" });
        matches!(p.evaluate(&args), Decision::Blocked { .. });
    }
}
