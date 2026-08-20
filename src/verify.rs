//! Deterministic answer verification.
//!
//! Small local models sometimes state a wrong figure in their prose even when
//! the tool returned correct data (we saw a model summarize "$300" for a $150
//! sum). This module recomputes the plausible aggregate values from the last
//! result set and flags numbers in the answer that match none of them. It is a
//! soft advisory: it never blocks, only cautions.

use crate::analytics::Table;
use serde_json::Value;
use std::collections::BTreeMap;

/// Numbers in `answer` that don't match any aggregate computed from `table`.
///
/// Only figures with magnitude >= `min_magnitude` are checked (small ordinals
/// like "top 5" are ignored), and four-digit years / percentages are skipped to
/// cut false positives.
pub fn unverified(answer: &str, table: &Table, min_magnitude: f64) -> Vec<f64> {
    if table.columns.is_empty() || table.rows.is_empty() {
        return Vec::new();
    }
    let candidates = candidate_values(table);
    let mut flagged = Vec::new();
    for n in extract_numbers(answer) {
        if n.abs() < min_magnitude {
            continue;
        }
        let matched = candidates.iter().any(|c| close(n, *c));
        if !matched && !flagged.iter().any(|f| close(n, *f)) {
            flagged.push(n);
        }
    }
    flagged
}

/// Match within 0.5 absolute or 1% relative — loose enough for rounding/units.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= f64::max(0.5, 0.01 * b.abs())
}

/// Plausible aggregate values a truthful answer might cite: raw numeric cells,
/// per-column sum/min/max/mean/count, and per-group sums for low-cardinality
/// dimension columns.
pub fn candidate_values(table: &Table) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::new();
    out.push(table.rows.len() as f64); // total row count

    let numeric_cols: Vec<usize> = (0..table.columns.len())
        .filter(|&i| is_numeric_column(table, i))
        .collect();
    let dim_cols: Vec<usize> = (0..table.columns.len())
        .filter(|&i| !numeric_cols.contains(&i))
        .collect();

    for &i in &numeric_cols {
        let vals: Vec<f64> = table.rows.iter().filter_map(|r| as_number(&r[i])).collect();
        if vals.is_empty() {
            continue;
        }
        let sum: f64 = vals.iter().sum();
        let count = vals.len() as f64;
        out.push(sum);
        out.push(count);
        out.push(sum / count);
        out.push(vals.iter().cloned().fold(f64::INFINITY, f64::min));
        out.push(vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max));
        out.extend(vals.iter().cloned()); // raw cell values
    }

    // Per-group sums: group by each dimension column, sum each numeric column.
    for &d in &dim_cols {
        // Skip high-cardinality columns to keep this bounded.
        let distinct: std::collections::HashSet<String> =
            table.rows.iter().map(|r| r[d].to_string()).collect();
        if distinct.len() > 50 {
            continue;
        }
        for &n in &numeric_cols {
            let mut groups: BTreeMap<String, f64> = BTreeMap::new();
            for r in &table.rows {
                if let Some(v) = as_number(&r[n]) {
                    *groups.entry(r[d].to_string()).or_insert(0.0) += v;
                }
            }
            out.extend(groups.values().cloned());
        }
    }

    out.iter().map(|v| round2(*v)).collect()
}

fn is_numeric_column(table: &Table, i: usize) -> bool {
    let cells: Vec<&Value> = table.rows.iter().map(|r| &r[i]).collect();
    let numeric = cells.iter().filter(|v| as_number(v).is_some()).count();
    numeric > 0 && numeric >= cells.len().saturating_sub(cells.len() / 5)
}

fn as_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Extract numeric literals from prose. Strips `$` and thousands commas, skips
/// percentages and four-digit years.
pub fn extract_numbers(text: &str) -> Vec<f64> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || bytes[i] == b',' || bytes[i] == b'.')
            {
                i += 1;
            }
            let raw = &text[start..i];
            // Strip a trailing dot (sentence period) and commas.
            let cleaned: String = raw
                .trim_end_matches('.')
                .chars()
                .filter(|c| *c != ',')
                .collect();
            let followed_by_percent = i < bytes.len() && bytes[i] == b'%';
            if let Ok(n) = cleaned.parse::<f64>() {
                let is_year =
                    n.fract() == 0.0 && (1900.0..=2100.0).contains(&n) && cleaned.len() == 4;
                if !followed_by_percent && !is_year {
                    out.push(n);
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales() -> Table {
        Table::from_tool_output(
            r#"[
              {"region":"east","sales":100},
              {"region":"east","sales":50},
              {"region":"west","sales":200}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn extract_handles_currency_and_commas() {
        let n = extract_numbers("East: $150, West: $1,200.");
        assert!(n.contains(&150.0));
        assert!(n.contains(&1200.0));
    }

    #[test]
    fn extract_skips_years_and_percents() {
        let n = extract_numbers("In 2024 sales grew 20% to 500.");
        assert!(!n.contains(&2024.0));
        assert!(!n.contains(&20.0));
        assert!(n.contains(&500.0));
    }

    #[test]
    fn correct_figures_are_not_flagged() {
        // east sum = 150, west sum = 200, both are valid aggregates.
        let flagged = unverified("East: $150 and West: $200.", &sales(), 10.0);
        assert!(flagged.is_empty(), "flagged: {flagged:?}");
    }

    #[test]
    fn hallucinated_figure_is_flagged() {
        // 300 is not any aggregate of the data (this is the real bug we saw).
        let flagged = unverified("East had $300 in sales.", &sales(), 10.0);
        assert_eq!(flagged, vec![300.0]);
    }

    #[test]
    fn small_ordinals_are_ignored() {
        // "top 3" — 3 is below min_magnitude and shouldn't be flagged.
        let flagged = unverified("The top 3 regions.", &sales(), 10.0);
        assert!(flagged.is_empty());
    }

    #[test]
    fn candidate_values_include_group_sums_and_total() {
        let c = candidate_values(&sales());
        assert!(c.iter().any(|v| close(*v, 150.0)), "east group sum");
        assert!(c.iter().any(|v| close(*v, 200.0)), "west group sum");
        assert!(c.iter().any(|v| close(*v, 350.0)), "overall sum");
        assert!(c.iter().any(|v| close(*v, 3.0)), "row count");
    }
}
