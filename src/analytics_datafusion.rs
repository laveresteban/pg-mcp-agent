//! Optional DataFusion-backed analytics engine (feature = "datafusion").
//!
//! This lets the agent run arbitrary analytical SQL locally over the last
//! result set: the fetched rows are loaded into an in-memory table named `t`,
//! and the caller's SQL runs against it with DataFusion's engine — window
//! functions, GROUP BY, joins, the lot — without another database round-trip.
//!
//! It's off by default because current DataFusion needs a newer rustc than the
//! 1.85 this project targets; enable with `--features datafusion` on a new
//! enough toolchain. The build-independent [`crate::analytics::BuiltinEngine`]
//! covers the common cases without this dependency.

use anyhow::{anyhow, Result};
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, Float64Array, StringArray};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::arrow::util::pretty::pretty_format_batches;
use datafusion::datasource::MemTable;
use datafusion::prelude::SessionContext;

use crate::analytics::Table;
use serde_json::Value;

/// Convert our JSON-y [`Table`] into an Arrow [`RecordBatch`]. Columns whose
/// non-null cells are all numeric become Float64; everything else becomes Utf8.
fn to_record_batch(table: &Table) -> Result<RecordBatch> {
    let mut fields = Vec::with_capacity(table.columns.len());
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(table.columns.len());

    for (i, name) in table.columns.iter().enumerate() {
        let cells: Vec<&Value> = table.rows.iter().map(|r| &r[i]).collect();
        let numeric =
            !cells.is_empty() && cells.iter().all(|v| v.is_null() || as_number(v).is_some());

        if numeric {
            let vals: Vec<Option<f64>> = cells.iter().map(|v| as_number(v)).collect();
            fields.push(Field::new(name, DataType::Float64, true));
            arrays.push(Arc::new(Float64Array::from(vals)) as ArrayRef);
        } else {
            let vals: Vec<Option<String>> = cells
                .iter()
                .map(|v| match v {
                    Value::Null => None,
                    Value::String(s) => Some(s.clone()),
                    other => Some(other.to_string()),
                })
                .collect();
            fields.push(Field::new(name, DataType::Utf8, true));
            arrays.push(Arc::new(StringArray::from(vals)) as ArrayRef);
        }
    }

    RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)
        .map_err(|e| anyhow!("building record batch: {e}"))
}

fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Run `query` against the last result set (registered as table `t`).
pub async fn run_sql(table: &Table, query: &str) -> Result<String> {
    if table.columns.is_empty() {
        return Ok("(the last result had no rows to analyze)".into());
    }
    let batch = to_record_batch(table)?;
    let schema = batch.schema();

    let ctx = SessionContext::new();
    let mem = MemTable::try_new(schema, vec![vec![batch]])
        .map_err(|e| anyhow!("registering table: {e}"))?;
    ctx.register_table("t", Arc::new(mem))
        .map_err(|e| anyhow!("register_table: {e}"))?;

    let df = ctx
        .sql(query)
        .await
        .map_err(|e| anyhow!("planning SQL: {e}"))?;
    let results = df
        .collect()
        .await
        .map_err(|e| anyhow!("executing SQL: {e}"))?;
    let pretty = pretty_format_batches(&results).map_err(|e| anyhow!("formatting: {e}"))?;
    Ok(pretty.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_analytical_group_by() {
        let table = Table::from_tool_output(
            r#"[
              {"region":"east","sales":100},
              {"region":"east","sales":50},
              {"region":"west","sales":200}
            ]"#,
        )
        .unwrap();
        let out = run_sql(
            &table,
            "SELECT region, sum(sales) AS total FROM t GROUP BY region ORDER BY region",
        )
        .await
        .unwrap();
        assert!(out.contains("east"), "output: {out}");
        assert!(out.contains("150"), "expected east total 150 in: {out}");
        assert!(out.contains("west"), "output: {out}");
        assert!(out.contains("200"), "expected west total 200 in: {out}");
    }

    #[tokio::test]
    async fn runs_window_function() {
        let table = Table::from_tool_output(
            r#"[{"day":"1","rev":10},{"day":"2","rev":20},{"day":"3","rev":30}]"#,
        )
        .unwrap();
        let out = run_sql(
            &table,
            "SELECT day, sum(rev) OVER (ORDER BY day) AS running FROM t ORDER BY day",
        )
        .await
        .unwrap();
        // running totals: 10, 30, 60
        assert!(out.contains("60"), "expected cumulative 60 in: {out}");
    }
}
