//! Generate a starter semantic-layer spec from schema introspection.
//!
//! `pg-mcp-agent init-specs` runs an `information_schema.columns` query through
//! the MCP server, then turns the result into a `*.spec.md` stub: a glossary
//! placeholder plus one verified "rows from <table>" example per table. The user
//! fills in real business definitions from there. See [`crate::semantics`].

use crate::analytics::Table;
use crate::catalog::Catalog;
use std::collections::BTreeMap;

/// Postgres query that lists user tables and their columns.
pub const INTROSPECTION_SQL: &str = "\
SELECT table_name, column_name, data_type \
FROM information_schema.columns \
WHERE table_schema = 'public' \
ORDER BY table_name, ordinal_position";

/// Turn an introspection result into a spec-file body.
///
/// If the rows carry `table_name`/`column_name` (the introspection shape), we
/// emit a section per table. Otherwise we degrade gracefully and emit a single
/// example over whatever columns are present, so the generator still produces
/// something useful against an unexpected result shape.
pub fn generate_spec(table: &Table) -> String {
    let ti = table.columns.iter().position(|c| c == "table_name");
    let ci = table.columns.iter().position(|c| c == "column_name");
    let di = table.columns.iter().position(|c| c == "data_type");

    match (ti, ci) {
        (Some(ti), Some(ci)) => from_information_schema(table, ti, ci, di),
        _ => from_generic_columns(table),
    }
}

fn cell(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn from_information_schema(table: &Table, ti: usize, ci: usize, di: Option<usize>) -> String {
    // table -> [(column, type)]
    let mut tables: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for row in &table.rows {
        let t = cell(&row[ti]);
        let c = cell(&row[ci]);
        let d = di.map(|i| cell(&row[i])).unwrap_or_default();
        if !t.is_empty() {
            tables.entry(t).or_default().push((c, d));
        }
    }

    let mut out = String::from(
        "# Generated schema spec\n\n\
         Auto-generated stub from information_schema. Replace the glossary \
         placeholders with real business definitions, then run `pg-mcp-agent \
         verify` to check the examples.\n\n## Glossary\n",
    );
    if tables.is_empty() {
        out.push_str("- **TODO**: no tables found in schema `public`.\n");
    } else {
        for (t, cols) in &tables {
            let names: Vec<&str> = cols.iter().map(|(c, _)| c.as_str()).collect();
            out.push_str(&format!(
                "- **{t}**: TODO describe. Columns: {}\n",
                names.join(", ")
            ));
        }
    }

    for t in tables.keys() {
        out.push_str(&format!(
            "\n## Example: rows from {t}\nQuestion: show recent rows from {t}\nExpect: runs\n```sql\nSELECT * FROM {t} LIMIT 20;\n```\n"
        ));
    }
    out
}

/// Build a spec from a data catalog. Unlike [`generate_spec`], this uses the
/// real business glossary and column descriptions, so the semantic layer is
/// grounded rather than guessed.
pub fn generate_spec_from_catalog(catalog: &Catalog) -> String {
    let mut out = String::from(
        "# Catalog-grounded spec\n\n\
         Generated from the data catalog: business glossary and column \
         descriptions are authoritative. Add verified example queries below, then \
         run `pg-mcp-agent verify`.\n\n## Glossary\n",
    );
    if catalog.glossary.is_empty() {
        out.push_str("- **TODO**: the catalog has no glossary terms yet.\n");
    } else {
        for g in &catalog.glossary {
            out.push_str(&format!("- **{}**: {}\n", g.term, g.definition));
        }
    }

    for t in &catalog.tables {
        let cols: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                if c.description.is_empty() {
                    c.name.clone()
                } else {
                    format!("{} ({})", c.name, c.description)
                }
            })
            .collect();
        out.push_str(&format!("\n## Example: rows from {}\n", t.name));
        if !t.description.is_empty() {
            out.push_str(&format!("Question: {} — {}\n", t.name, t.description));
        } else {
            out.push_str(&format!("Question: show recent rows from {}\n", t.name));
        }
        if !cols.is_empty() {
            out.push_str(&format!("<!-- columns: {} -->\n", cols.join("; ")));
        }
        if !t.downstream.is_empty() {
            out.push_str(&format!("<!-- feeds: {} -->\n", t.downstream.join(", ")));
        }
        out.push_str(&format!(
            "Expect: runs\n```sql\nSELECT * FROM {} LIMIT 20;\n```\n",
            t.name
        ));
    }
    out
}

fn from_generic_columns(table: &Table) -> String {
    let cols = table.columns.join(", ");
    format!(
        "# Generated spec\n\n\
         The introspection result did not look like information_schema output, \
         so this is a generic stub over the columns that came back: {cols}.\n\n\
         ## Glossary\n- **TODO**: describe your key business terms here.\n\n\
         ## Example: sample rows\nQuestion: show a sample of the data\nExpect: runs\n\
         ```sql\nSELECT {cols} FROM your_table LIMIT 20;\n```\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_per_table_sections() {
        let table = Table::from_tool_output(
            r#"[
              {"table_name":"orders","column_name":"id","data_type":"integer"},
              {"table_name":"orders","column_name":"total","data_type":"numeric"},
              {"table_name":"customers","column_name":"email","data_type":"text"}
            ]"#,
        )
        .unwrap();
        let spec = generate_spec(&table);
        assert!(spec.contains("## Example: rows from orders"));
        assert!(spec.contains("## Example: rows from customers"));
        assert!(spec.contains("SELECT * FROM orders LIMIT 20;"));
        assert!(spec.contains("**orders**"));
        assert!(spec.contains("id, total"));
    }

    #[test]
    fn degrades_on_unexpected_shape() {
        let table = Table::from_tool_output(r#"[{"region":"east","sales":100}]"#).unwrap();
        let spec = generate_spec(&table);
        assert!(spec.contains("generic stub"));
        assert!(spec.contains("region, sales"));
    }

    #[test]
    fn catalog_spec_uses_real_glossary_and_tables() {
        let catalog: Catalog = serde_json::from_str(
            r#"{
              "tables": [{"name":"orders","description":"one row per order",
                          "columns":[{"name":"total","data_type":"numeric","description":"order total"}],
                          "downstream":["mv_monthly_revenue"]}],
              "glossary": [{"term":"revenue","definition":"sum of line totals"}]
            }"#,
        )
        .unwrap();
        let spec = generate_spec_from_catalog(&catalog);
        assert!(spec.contains("**revenue**: sum of line totals"));
        assert!(spec.contains("## Example: rows from orders"));
        assert!(spec.contains("total (order total)"));
        assert!(spec.contains("feeds: mv_monthly_revenue"));
        // Must round-trip through the spec parser.
        let layer = crate::semantics::parse_str(&spec);
        assert_eq!(layer.glossary.len(), 1);
        assert_eq!(layer.specs.len(), 1);
    }

    #[test]
    fn generated_spec_parses_back_as_a_semantic_layer() {
        // The output must be valid input for the spec parser.
        let table = Table::from_tool_output(
            r#"[{"table_name":"orders","column_name":"id","data_type":"integer"}]"#,
        )
        .unwrap();
        let spec = generate_spec(&table);
        let layer = crate::semantics::parse_str(&spec);
        assert_eq!(layer.specs.len(), 1);
        assert!(layer.specs[0].sql.contains("SELECT * FROM orders"));
    }
}
