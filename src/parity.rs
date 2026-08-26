//! Cross-engine parity: prove a metric computes to the SAME number on Postgres
//! (the source of truth) and on its ClickHouse rollup.
//!
//! This is the feature that only lives in this project's square — a *verified
//! semantic layer that spans both engines*. The market pain is explicit: numbers
//! disagree across tools and metrics drift between dashboards. A CDC pipe (e.g.
//! ClickPipes) moves the rows but never proves the rollup still equals the
//! source. We do: two specs that share a `Parity:` key are run on their
//! respective backends, a scalar is extracted from each result, and we assert
//! they agree within tolerance.
//!
//! The types and math here are pure (no MCP, no IO) so they unit-test cleanly;
//! `semantics::SemanticLayer::verify_parity` does the routing and calls in.

/// Default relative tolerance for a match (1e-6 — floats from JSON round-trips).
pub const DEFAULT_TOLERANCE: f64 = 1e-6;

/// One backend's contribution to a parity group.
#[derive(Debug, Clone)]
pub struct ParityMember {
    /// The spec name (e.g. "revenue total (Postgres)").
    pub name: String,
    /// The backend the query ran on ("postgres" / "clickhouse").
    pub backend: String,
    /// The scalar extracted from the result, or `None` if none could be parsed.
    pub value: Option<f64>,
    /// A short slice of the raw result, for diagnosis when values disagree.
    pub excerpt: String,
}

/// A set of members that should all agree on one metric.
#[derive(Debug, Clone)]
pub struct ParityGroup {
    /// The shared `Parity:` key that links the members.
    pub key: String,
    pub members: Vec<ParityMember>,
}

impl ParityGroup {
    /// Largest relative difference between any two members' values. `None` if
    /// fewer than two members carry a parseable value.
    pub fn max_rel_diff(&self) -> Option<f64> {
        let vals: Vec<f64> = self.members.iter().filter_map(|m| m.value).collect();
        if vals.len() < 2 {
            return None;
        }
        let mut worst = 0.0_f64;
        for i in 0..vals.len() {
            for j in (i + 1)..vals.len() {
                worst = worst.max(rel_diff(vals[i], vals[j]));
            }
        }
        Some(worst)
    }

    /// A group passes when it has ≥2 parseable values and they all agree within
    /// `tolerance`. A group that couldn't parse enough values fails (loudly — a
    /// parity check you can't evaluate is not a pass).
    pub fn passed(&self, tolerance: f64) -> bool {
        matches!(self.max_rel_diff(), Some(d) if d <= tolerance)
    }

    /// Why the group failed, or empty when it passed.
    pub fn detail(&self, tolerance: f64) -> String {
        match self.max_rel_diff() {
            None => {
                let missing: Vec<&str> = self
                    .members
                    .iter()
                    .filter(|m| m.value.is_none())
                    .map(|m| m.name.as_str())
                    .collect();
                if self.members.len() < 2 {
                    format!(
                        "parity group `{}` needs ≥2 specs (has {})",
                        self.key,
                        self.members.len()
                    )
                } else {
                    format!("could not extract a number from: {}", missing.join(", "))
                }
            }
            Some(d) if d <= tolerance => String::new(),
            Some(d) => format!(
                "values disagree by {:.4}% (tolerance {:.4}%)",
                d * 100.0,
                tolerance * 100.0
            ),
        }
    }
}

/// A full parity run.
#[derive(Debug, Default, Clone)]
pub struct ParityReport {
    pub groups: Vec<ParityGroup>,
    pub tolerance: f64,
}

impl ParityReport {
    pub fn new(groups: Vec<ParityGroup>, tolerance: f64) -> Self {
        ParityReport { groups, tolerance }
    }

    pub fn total(&self) -> usize {
        self.groups.len()
    }

    pub fn failures(&self) -> usize {
        self.groups
            .iter()
            .filter(|g| !g.passed(self.tolerance))
            .count()
    }

    pub fn passed(&self) -> usize {
        self.total() - self.failures()
    }

    /// Human-readable report printed by `pg-mcp-agent parity`.
    pub fn to_human(&self) -> String {
        if self.groups.is_empty() {
            return "No parity groups found. Add a `Parity: <key>` line to two \
                    specs on different backends to cross-check them.\n"
                .to_string();
        }
        let mut s = format!("Checking {} parity group(s)\n\n", self.total());
        for g in &self.groups {
            let cells: Vec<String> = g
                .members
                .iter()
                .map(|m| match m.value {
                    Some(v) => format!("{} {}", m.backend, trim_num(v)),
                    None => format!("{} ?", m.backend),
                })
                .collect();
            if g.passed(self.tolerance) {
                s.push_str(&format!("  MATCH  {}  ({})\n", g.key, cells.join(" == ")));
            } else {
                s.push_str(&format!(
                    "  DIFF   {}  ({})  — {}\n",
                    g.key,
                    cells.join(" vs "),
                    g.detail(self.tolerance)
                ));
            }
        }
        s.push_str(&format!(
            "\n{} matched, {} differed\n",
            self.passed(),
            self.failures()
        ));
        s
    }
}

/// Relative difference between two numbers, robust around zero.
fn rel_diff(a: f64, b: f64) -> f64 {
    let denom = a.abs().max(b.abs());
    if denom == 0.0 {
        0.0
    } else {
        (a - b).abs() / denom
    }
}

/// Render a float without a trailing `.0` for whole numbers.
fn trim_num(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Extract the first numeric value from tool output. Result text is typically
/// JSON like `[{"revenue_total":4580}]`, but this also handles a bare number,
/// CSV, or a single-column table. Skips numbers that are part of an identifier
/// (e.g. the `2` in a column named `col2` or a date like `2026-08-14`).
pub fn extract_number(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        let starts_num = c.is_ascii_digit()
            || (c == '-' && i + 1 < bytes.len() && (bytes[i + 1] as char).is_ascii_digit());
        // Don't start a number in the middle of an identifier/date.
        let prev_ok = i == 0 || {
            let p = bytes[i - 1] as char;
            !(p.is_ascii_alphanumeric() || p == '_' || p == '-' || p == '.')
        };
        if starts_num && prev_ok {
            let start = i;
            if c == '-' {
                i += 1;
            }
            let mut saw_dot = false;
            while i < bytes.len() {
                let d = bytes[i] as char;
                if d.is_ascii_digit() {
                    i += 1;
                } else if d == '.' && !saw_dot {
                    saw_dot = true;
                    i += 1;
                } else {
                    break;
                }
            }
            // Reject if the number runs straight into letters (e.g. `12ab`,
            // `2026-08` would already be split, but `3d` shouldn't parse as 3).
            let next_is_ident = i < bytes.len() && {
                let n = bytes[i] as char;
                n.is_ascii_alphabetic() || n == '_' || n == '-'
            };
            if !next_is_ident {
                if let Ok(v) = text[start..i].parse::<f64>() {
                    return Some(v);
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_number_from_json() {
        assert_eq!(extract_number(r#"[{"revenue_total":4580}]"#), Some(4580.0));
        assert_eq!(extract_number(r#"[{"total":1234.5}]"#), Some(1234.5));
        assert_eq!(extract_number("no rows"), None);
        assert_eq!(extract_number(""), None);
    }

    #[test]
    fn extracts_negative_and_bare() {
        assert_eq!(extract_number("-42"), Some(-42.0));
        assert_eq!(extract_number("  2.5 "), Some(2.5));
    }

    #[test]
    fn skips_numbers_inside_identifiers_and_dates() {
        // The value we want is the revenue, not the date parts or a col name.
        assert_eq!(
            extract_number(r#"[{"day":"2026-08-14","revenue":4580}]"#),
            Some(4580.0)
        );
        assert_eq!(extract_number(r#"{"col2":99}"#), Some(99.0));
    }

    fn group(key: &str, a: Option<f64>, b: Option<f64>) -> ParityGroup {
        ParityGroup {
            key: key.into(),
            members: vec![
                ParityMember {
                    name: format!("{key} (Postgres)"),
                    backend: "postgres".into(),
                    value: a,
                    excerpt: String::new(),
                },
                ParityMember {
                    name: format!("{key} (ClickHouse)"),
                    backend: "clickhouse".into(),
                    value: b,
                    excerpt: String::new(),
                },
            ],
        }
    }

    #[test]
    fn equal_values_match() {
        let g = group("revenue", Some(4580.0), Some(4580.0));
        assert!(g.passed(DEFAULT_TOLERANCE));
        assert_eq!(g.detail(DEFAULT_TOLERANCE), "");
    }

    #[test]
    fn tiny_float_drift_still_matches() {
        let g = group("revenue", Some(100.0), Some(100.0 + 1e-9));
        assert!(g.passed(DEFAULT_TOLERANCE));
    }

    #[test]
    fn disagreement_fails_with_percent_detail() {
        let g = group("revenue", Some(100.0), Some(110.0));
        assert!(!g.passed(DEFAULT_TOLERANCE));
        assert!(g.detail(DEFAULT_TOLERANCE).contains("%"));
    }

    #[test]
    fn unparseable_member_fails_loudly() {
        let g = group("revenue", Some(100.0), None);
        assert!(!g.passed(DEFAULT_TOLERANCE));
        assert!(g.detail(DEFAULT_TOLERANCE).contains("could not extract"));
    }

    #[test]
    fn report_counts_and_human() {
        let report = ParityReport::new(
            vec![
                group("revenue", Some(4580.0), Some(4580.0)),
                group("orders", Some(156.0), Some(158.0)),
            ],
            DEFAULT_TOLERANCE,
        );
        assert_eq!(report.total(), 2);
        assert_eq!(report.passed(), 1);
        assert_eq!(report.failures(), 1);
        let human = report.to_human();
        assert!(human.contains("MATCH  revenue"));
        assert!(human.contains("DIFF   orders"));
        assert!(human.contains("1 matched, 1 differed"));
    }
}
