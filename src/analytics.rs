//! Local analytics over a fetched result set.
//!
//! The agent already runs analytical SQL on Postgres. This module adds a
//! second, in-process path: once a query has returned rows, the model (or the
//! user) can ask for further analysis — describe, group-by aggregation, top-N —
//! without another database round-trip. It's exposed to the model as the
//! `analyze_last_result` tool.
//!
//! The default engine is dependency-free so the project always builds. A
//! heavier engine (Apache DataFusion) can be swapped in behind the `datafusion`
//! cargo feature; both satisfy the same [`AnalyticsEngine`] contract.

use anyhow::{anyhow, bail, Result};
use serde_json::Value;

/// A simple column-oriented table parsed from an MCP tool result.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Table {
    /// Best-effort parse of a tool result into a table.
    ///
    /// Handles the shapes Postgres MCP servers commonly emit: a JSON array of
    /// objects, or an object wrapping such an array under `rows`/`data`/`result`.
    pub fn from_tool_output(text: &str) -> Option<Table> {
        let value: Value = serde_json::from_str(text.trim()).ok()?;
        let array = match &value {
            Value::Array(a) => a.clone(),
            Value::Object(o) => o
                .get("rows")
                .or_else(|| o.get("data"))
                .or_else(|| o.get("result"))
                .or_else(|| o.get("records"))
                .and_then(Value::as_array)
                .cloned()?,
            _ => return None,
        };
        if array.is_empty() {
            return Some(Table::default());
        }
        // Column order from the first object; union in any later keys.
        let mut columns: Vec<String> = Vec::new();
        for item in &array {
            let obj = item.as_object()?; // not rows-of-objects; give up
            for k in obj.keys() {
                if !columns.iter().any(|c| c == k) {
                    columns.push(k.clone());
                }
            }
        }
        let rows = array
            .iter()
            .map(|item| {
                let obj = item.as_object().unwrap();
                columns
                    .iter()
                    .map(|c| obj.get(c).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();
        Some(Table { columns, rows })
    }

    fn col_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
}

/// Pull an f64 out of a JSON cell (number, or numeric string).
fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// The operation a caller wants to run against the last result set.
#[derive(Debug)]
pub enum Analysis {
    /// Per-column summary statistics.
    Describe,
    /// Group by one or more columns and aggregate a metric column.
    GroupBy {
        by: Vec<String>,
        agg: Agg,
        column: Option<String>,
    },
    /// Top N rows ordered by a column.
    Top {
        by: String,
        n: usize,
        descending: bool,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Agg {
    fn parse(s: &str) -> Result<Agg> {
        Ok(match s.to_lowercase().as_str() {
            "count" => Agg::Count,
            "sum" => Agg::Sum,
            "avg" | "mean" | "average" => Agg::Avg,
            "min" => Agg::Min,
            "max" => Agg::Max,
            other => bail!("unknown aggregate `{other}` (use count/sum/avg/min/max)"),
        })
    }
    fn label(self) -> &'static str {
        match self {
            Agg::Count => "count",
            Agg::Sum => "sum",
            Agg::Avg => "avg",
            Agg::Min => "min",
            Agg::Max => "max",
        }
    }
}

impl Analysis {
    /// Parse the `analyze_last_result` tool arguments.
    pub fn from_args(args: &Value) -> Result<Analysis> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("describe");
        match op {
            "describe" => Ok(Analysis::Describe),
            "group_by" | "groupby" => {
                let by = string_list(args.get("by"))
                    .ok_or_else(|| anyhow!("group_by needs `by` (a column or list of columns)"))?;
                let agg = Agg::parse(args.get("agg").and_then(Value::as_str).unwrap_or("count"))?;
                let column = args
                    .get("column")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Ok(Analysis::GroupBy { by, agg, column })
            }
            "top" => {
                let by = args
                    .get("by")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("top needs `by` (a column)"))?
                    .to_string();
                let n = args.get("n").and_then(Value::as_u64).unwrap_or(5) as usize;
                let descending = !matches!(
                    args.get("order").and_then(Value::as_str),
                    Some("asc") | Some("ascending")
                );
                Ok(Analysis::Top { by, n, descending })
            }
            other => bail!("unknown op `{other}` (use describe/group_by/top)"),
        }
    }
}

fn string_list(v: Option<&Value>) -> Option<Vec<String>> {
    match v? {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// A pluggable analytics engine. The built-in one is dependency-free; a
/// DataFusion-backed one can implement the same trait behind a feature.
pub trait AnalyticsEngine {
    fn run(&self, table: &Table, analysis: &Analysis) -> Result<String>;
}

/// Dependency-free engine implemented with plain Rust iteration.
pub struct BuiltinEngine;

impl AnalyticsEngine for BuiltinEngine {
    fn run(&self, table: &Table, analysis: &Analysis) -> Result<String> {
        if table.columns.is_empty() {
            return Ok("(the last result had no rows to analyze)".into());
        }
        match analysis {
            Analysis::Describe => Ok(describe(table)),
            Analysis::GroupBy { by, agg, column } => group_by(table, by, *agg, column.as_deref()),
            Analysis::Top { by, n, descending } => top(table, by, *n, *descending),
        }
    }
}

fn describe(t: &Table) -> String {
    let mut out = format!("{} rows, {} columns\n", t.rows.len(), t.columns.len());
    for (i, col) in t.columns.iter().enumerate() {
        let cells: Vec<&Value> = t.rows.iter().map(|r| &r[i]).collect();
        let nums: Vec<f64> = cells.iter().filter_map(|v| as_number(v)).collect();
        if !nums.is_empty() && nums.len() >= cells.len().saturating_sub(cells.len() / 5) {
            // Mostly numeric: report numeric stats.
            let count = nums.len();
            let sum: f64 = nums.iter().sum();
            let mean = sum / count as f64;
            let min = nums.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let var = nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / count as f64;
            out.push_str(&format!(
                "- {col}: n={count} min={min:.4} max={max:.4} mean={mean:.4} sum={sum:.4} stddev={:.4}\n",
                var.sqrt()
            ));
        } else {
            let non_null = cells.iter().filter(|v| !v.is_null()).count();
            let distinct = {
                let mut seen: Vec<String> = cells.iter().map(|v| v.to_string()).collect();
                seen.sort();
                seen.dedup();
                seen.len()
            };
            out.push_str(&format!(
                "- {col}: n={non_null} distinct={distinct} (non-numeric)\n"
            ));
        }
    }
    out
}

fn group_by(t: &Table, by: &[String], agg: Agg, column: Option<&str>) -> Result<String> {
    let by_idx: Vec<usize> = by
        .iter()
        .map(|c| {
            t.col_index(c)
                .ok_or_else(|| anyhow!("no such column `{c}`"))
        })
        .collect::<Result<_>>()?;
    let metric_idx = match (agg, column) {
        (Agg::Count, _) => None,
        (_, Some(col)) => Some(
            t.col_index(col)
                .ok_or_else(|| anyhow!("no such column `{col}`"))?,
        ),
        (_, None) => bail!("{} needs a `column` to aggregate", agg.label()),
    };

    // key -> (count, accumulated metric)
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, (u64, Vec<f64>)> = BTreeMap::new();
    for row in &t.rows {
        let key = by_idx
            .iter()
            .map(|&i| render_cell(&row[i]))
            .collect::<Vec<_>>()
            .join(" | ");
        let entry = groups.entry(key).or_default();
        entry.0 += 1;
        if let Some(mi) = metric_idx {
            if let Some(n) = as_number(&row[mi]) {
                entry.1.push(n);
            }
        }
    }

    let header = by.join(" | ");
    let mut out = format!("{header} :: {}\n", agg.label());
    for (key, (count, nums)) in &groups {
        let value = match agg {
            Agg::Count => *count as f64,
            Agg::Sum => nums.iter().sum(),
            Agg::Avg => {
                if nums.is_empty() {
                    f64::NAN
                } else {
                    nums.iter().sum::<f64>() / nums.len() as f64
                }
            }
            Agg::Min => nums.iter().cloned().fold(f64::INFINITY, f64::min),
            Agg::Max => nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
        };
        out.push_str(&format!("{key} :: {value:.4}\n"));
    }
    Ok(out)
}

fn top(t: &Table, by: &str, n: usize, descending: bool) -> Result<String> {
    let idx = t
        .col_index(by)
        .ok_or_else(|| anyhow!("no such column `{by}`"))?;
    let mut ranked: Vec<(&Vec<Value>, f64)> = t
        .rows
        .iter()
        .map(|r| (r, as_number(&r[idx]).unwrap_or(f64::NEG_INFINITY)))
        .collect();
    ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    if descending {
        ranked.reverse();
    }

    let mut out = format!("{}\n", t.columns.join(" | "));
    for (row, _) in ranked.into_iter().take(n) {
        let line = row.iter().map(render_cell).collect::<Vec<_>>().join(" | ");
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

fn render_cell(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// JSON schema advertised to the model for the `analyze_last_result` tool.
/// When the `datafusion` feature is on, an extra `sql` op is offered that runs
/// arbitrary analytical SQL over the last result set (registered as table `t`).
pub fn analyze_tool_schema() -> Value {
    #[cfg(feature = "datafusion")]
    let ops = serde_json::json!(["describe", "group_by", "top", "sql"]);
    #[cfg(not(feature = "datafusion"))]
    let ops = serde_json::json!(["describe", "group_by", "top"]);

    serde_json::json!({
        "type": "object",
        "properties": {
            "op": {
                "type": "string",
                "enum": ops,
                "description": "describe = per-column stats; group_by = aggregate a metric by columns; top = top-N rows by a column; sql = run analytical SQL over the last result set (table `t`)"
            },
            "query": {
                "type": "string",
                "description": "op=sql only: analytical SQL to run against the last result set, which is available as table `t`"
            },
            "by": {
                "description": "group_by: column or list of columns to group by. top: the column to rank by.",
                "type": ["string", "array"]
            },
            "agg": {
                "type": "string",
                "enum": ["count", "sum", "avg", "min", "max"],
                "description": "group_by aggregate function"
            },
            "column": { "type": "string", "description": "group_by: the metric column for sum/avg/min/max" },
            "n": { "type": "integer", "description": "top: how many rows" },
            "order": { "type": "string", "enum": ["asc", "desc"], "description": "top: sort direction (default desc)" }
        },
        "required": ["op"]
    })
}

pub const ANALYZE_TOOL_NAME: &str = "analyze_last_result";
pub const ANALYZE_TOOL_DESCRIPTION: &str =
    "Analyze the rows returned by the most recent SQL query, in memory, without hitting the database again. \
     Use op=describe for column stats, op=group_by (with by/agg/column) to aggregate, op=top (with by/n) for a ranking.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Table {
        Table::from_tool_output(
            r#"[
              {"region":"east","sales":100,"orders":3},
              {"region":"east","sales":50,"orders":1},
              {"region":"west","sales":200,"orders":4}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_json_rows() {
        let t = sample();
        assert_eq!(t.columns, vec!["region", "sales", "orders"]);
        assert_eq!(t.rows.len(), 3);
    }

    #[test]
    fn parses_wrapped_rows() {
        let t = Table::from_tool_output(r#"{"rows":[{"a":1},{"a":2}]}"#).unwrap();
        assert_eq!(t.rows.len(), 2);
    }

    #[test]
    fn group_by_sum() {
        let t = sample();
        let a = Analysis::GroupBy {
            by: vec!["region".into()],
            agg: Agg::Sum,
            column: Some("sales".into()),
        };
        let out = BuiltinEngine.run(&t, &a).unwrap();
        assert!(out.contains("east :: 150"));
        assert!(out.contains("west :: 200"));
    }

    #[test]
    fn top_by_sales() {
        let t = sample();
        let a = Analysis::Top {
            by: "sales".into(),
            n: 1,
            descending: true,
        };
        let out = BuiltinEngine.run(&t, &a).unwrap();
        assert!(out.contains("west"));
        assert!(!out.contains("east | 50"));
    }

    #[test]
    fn describe_reports_numeric_and_text() {
        let t = sample();
        let out = BuiltinEngine.run(&t, &Analysis::Describe).unwrap();
        assert!(out.contains("sales:"));
        assert!(out.contains("region:"));
        assert!(out.contains("non-numeric"));
    }

    #[test]
    fn parse_args_group_by() {
        let a = Analysis::from_args(
            &json!({"op":"group_by","by":"region","agg":"sum","column":"sales"}),
        )
        .unwrap();
        matches!(a, Analysis::GroupBy { .. });
    }
}
