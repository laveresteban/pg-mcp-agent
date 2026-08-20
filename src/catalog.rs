//! A file-backed data catalog: table/column descriptions, a business glossary,
//! and simple lineage. It grounds the semantic layer with real metadata instead
//! of `init-specs`'s schema-only guesses.
//!
//! The same structs are used two ways: the `catalog_mcp_server` binary loads a
//! catalog file and serves it over MCP, and the client side (`init-specs`)
//! parses the served JSON back into a [`Catalog`] to generate a richer spec.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub tables: Vec<TableMeta>,
    #[serde(default)]
    pub glossary: Vec<GlossaryTerm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub columns: Vec<ColumnMeta>,
    /// Upstream sources (lineage in).
    #[serde(default)]
    pub upstream: Vec<String>,
    /// Downstream consumers (lineage out).
    #[serde(default)]
    pub downstream: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    #[serde(default)]
    pub data_type: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlossaryTerm {
    pub term: String,
    pub definition: String,
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Catalog> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading catalog file {}", path.display()))?;
        let catalog: Catalog = serde_json::from_str(&text)
            .with_context(|| format!("parsing catalog file {}", path.display()))?;
        Ok(catalog)
    }

    pub fn table(&self, name: &str) -> Option<&TableMeta> {
        self.tables.iter().find(|t| t.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "tables": [
        {"name":"orders","description":"one row per order",
         "columns":[{"name":"id","data_type":"int","description":"pk"}],
         "upstream":["raw.orders"],"downstream":["mv_monthly_revenue"]}
      ],
      "glossary": [{"term":"revenue","definition":"sum of line totals"}]
    }"#;

    #[test]
    fn parses_catalog_json() {
        let c: Catalog = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(c.tables.len(), 1);
        assert_eq!(c.table("orders").unwrap().columns[0].name, "id");
        assert_eq!(c.glossary[0].term, "revenue");
        assert_eq!(c.table("orders").unwrap().upstream, vec!["raw.orders"]);
    }

    #[test]
    fn tolerates_minimal_entries() {
        let c: Catalog = serde_json::from_str(r#"{"tables":[{"name":"t"}]}"#).unwrap();
        assert_eq!(c.tables[0].name, "t");
        assert!(c.tables[0].columns.is_empty());
        assert!(c.glossary.is_empty());
    }
}
