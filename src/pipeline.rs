//! Turn verified specs into pipeline artifacts.
//!
//! A verified query in the semantic layer is a known-good SELECT. This module
//! productizes it into a materialized view: `CREATE MATERIALIZED VIEW` plus a
//! `REFRESH` statement. `pg-mcp-agent materialize` prints the DDL for every spec
//! so you can review it and add it to a migration. It only generates SQL — it
//! does not apply it — so it is always safe to run.

use crate::semantics::{Backend, QuerySpec, SemanticLayer};

/// Derive a Postgres-safe materialized-view name from a spec name.
pub fn view_name(spec_name: &str) -> String {
    let mut slug = String::from("mv_");
    let mut last_underscore = true; // avoid leading underscore after the prefix
    for c in spec_name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            slug.push('_');
            last_underscore = true;
        }
    }
    let trimmed = slug.trim_end_matches('_').to_string();
    if trimmed == "mv" {
        "mv_view".to_string()
    } else {
        trimmed
    }
}

/// `CREATE MATERIALIZED VIEW … AS <select> WITH DATA;`
pub fn materialized_view_ddl(spec_name: &str, select_sql: &str) -> String {
    let name = view_name(spec_name);
    let body = select_sql.trim().trim_end_matches(';').trim();
    format!("CREATE MATERIALIZED VIEW {name} AS\n{body}\nWITH DATA;")
}

/// `REFRESH MATERIALIZED VIEW …;`
pub fn refresh_ddl(spec_name: &str) -> String {
    format!("REFRESH MATERIALIZED VIEW {};", view_name(spec_name))
}

/// ClickHouse materialized views are *incremental*: they need a storage engine
/// and a sorting key, and update as new rows are inserted (no manual REFRESH).
/// `engine`/`order_by` fall back to sensible, clearly-flagged defaults so the
/// generated DDL is valid and reviewable even before the spec pins them down.
pub fn clickhouse_view_ddl(
    spec_name: &str,
    select_sql: &str,
    engine: Option<&str>,
    order_by: Option<&str>,
) -> String {
    let name = view_name(spec_name);
    let body = select_sql.trim().trim_end_matches(';').trim();
    let engine = engine.unwrap_or("MergeTree()");
    let order = order_by.unwrap_or("tuple()");
    format!(
        "CREATE MATERIALIZED VIEW {name}\nENGINE = {engine}\nORDER BY ({order})\nPOPULATE AS\n{body};"
    )
}

/// Render CREATE (+ REFRESH for Postgres) DDL for one spec, with a comment
/// header that records the target backend.
pub fn spec_to_ddl(spec: &QuerySpec) -> String {
    match spec.backend {
        Backend::Postgres => format!(
            "-- {name}  [postgres]\n{create}\n{refresh}\n",
            name = spec.name,
            create = materialized_view_ddl(&spec.name, &spec.sql),
            refresh = refresh_ddl(&spec.name),
        ),
        Backend::ClickHouse => {
            let mut out = format!(
                "-- {name}  [clickhouse]\n{create}\n",
                name = spec.name,
                create = clickhouse_view_ddl(
                    &spec.name,
                    &spec.sql,
                    spec.engine.as_deref(),
                    spec.order_by.as_deref(),
                ),
            );
            if spec.order_by.is_none() {
                out.push_str(
                    "-- NOTE: no `Order by:` in the spec — defaulted to tuple(). \
                     Set a sorting key for real workloads.\n",
                );
            }
            out
        }
    }
}

/// Render DDL for every spec in a layer.
pub fn layer_to_ddl(layer: &SemanticLayer) -> String {
    let mut out = String::from(
        "-- Materialized views generated from verified specs by `pg-mcp-agent materialize`.\n\
         -- Review before applying. Adjust refresh cadence to your needs.\n\n",
    );
    for spec in &layer.specs {
        out.push_str(&spec_to_ddl(spec));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_name_is_slugified_and_prefixed() {
        assert_eq!(view_name("monthly revenue"), "mv_monthly_revenue");
        assert_eq!(view_name("Top 5 products!!"), "mv_top_5_products");
        assert_eq!(view_name("  weird--name  "), "mv_weird_name");
        assert_eq!(view_name(""), "mv_view");
    }

    #[test]
    fn create_ddl_wraps_the_select() {
        let ddl = materialized_view_ddl("monthly revenue", "SELECT 1 AS x;");
        assert!(ddl.starts_with("CREATE MATERIALIZED VIEW mv_monthly_revenue AS"));
        assert!(ddl.contains("SELECT 1 AS x"));
        assert!(!ddl.contains(";\nWITH")); // trailing ; on the select was stripped
        assert!(ddl.trim_end().ends_with("WITH DATA;"));
    }

    #[test]
    fn refresh_ddl_targets_the_same_view() {
        assert_eq!(
            refresh_ddl("monthly revenue"),
            "REFRESH MATERIALIZED VIEW mv_monthly_revenue;"
        );
    }

    #[test]
    fn clickhouse_ddl_is_incremental_with_engine_and_order() {
        let ddl = clickhouse_view_ddl(
            "daily events",
            "SELECT day, count() AS c FROM events GROUP BY day;",
            Some("SummingMergeTree()"),
            Some("day"),
        );
        assert!(ddl.starts_with("CREATE MATERIALIZED VIEW mv_daily_events"));
        assert!(ddl.contains("ENGINE = SummingMergeTree()"));
        assert!(ddl.contains("ORDER BY (day)"));
        assert!(ddl.contains("POPULATE AS"));
        assert!(!ddl.contains("REFRESH")); // ClickHouse MVs update incrementally
    }

    #[test]
    fn clickhouse_ddl_defaults_are_valid_and_flagged() {
        let ddl = clickhouse_view_ddl("m", "SELECT 1", None, None);
        assert!(ddl.contains("ENGINE = MergeTree()"));
        assert!(ddl.contains("ORDER BY (tuple())"));
    }

    #[test]
    fn spec_to_ddl_routes_on_backend() {
        let layer = crate::semantics::parse_str(
            "## Metric: ch rollup\nBackend: clickhouse\nEngine: SummingMergeTree()\nOrder by: month\n```sql\nSELECT 1;\n```",
        );
        let ddl = spec_to_ddl(&layer.specs[0]);
        assert!(ddl.contains("[clickhouse]"));
        assert!(ddl.contains("ENGINE = SummingMergeTree()"));
        assert!(!ddl.contains("REFRESH MATERIALIZED VIEW"));
    }

    #[test]
    fn spec_to_ddl_postgres_default_still_refreshes() {
        let layer = crate::semantics::parse_str("## Metric: pg\n```sql\nSELECT 1;\n```");
        let ddl = spec_to_ddl(&layer.specs[0]);
        assert!(ddl.contains("[postgres]"));
        assert!(ddl.contains("REFRESH MATERIALIZED VIEW mv_pg;"));
    }

    #[test]
    fn layer_ddl_covers_all_specs() {
        let layer = crate::semantics::parse_str(
            "## Example: a\n```sql\nSELECT 1;\n```\n## Example: b\n```sql\nSELECT 2;\n```",
        );
        let ddl = layer_to_ddl(&layer);
        assert!(ddl.contains("mv_a"));
        assert!(ddl.contains("mv_b"));
        assert_eq!(ddl.matches("CREATE MATERIALIZED VIEW").count(), 2);
    }
}
