//! A compact, always-on cheatsheet of analytical Postgres patterns.
//!
//! Research on NL->SQL (see the project notes) is blunt: frontier models write
//! syntactically valid SQL, but they pick the wrong *shape* for analytical
//! questions and invent business meaning. Grounding the model with canonical
//! Postgres idioms for the common analytical intents measurably helps. This
//! text is injected into the system prompt.

pub const ANALYTICS_PATTERNS: &str = r#"
POSTGRES ANALYTICAL PATTERNS (prefer these shapes; adapt table/column names to the real schema):

- Time bucketing: GROUP BY date_trunc('month', ts) — swap 'day'/'week'/'hour' as asked.
- Gap-free time series (no missing buckets):
    SELECT g.bucket, count(t.*) FROM generate_series(:start, :end, interval '1 day') g(bucket)
    LEFT JOIN events t ON date_trunc('day', t.ts) = g.bucket GROUP BY 1 ORDER BY 1;
- Running / cumulative total:
    SUM(amount) OVER (ORDER BY day ROWS UNBOUNDED PRECEDING)
- Top-N per group (e.g. top 3 products per category):
    SELECT * FROM (
      SELECT *, ROW_NUMBER() OVER (PARTITION BY category ORDER BY revenue DESC) rn FROM p
    ) s WHERE rn <= 3;
- Subtotals / grand totals: GROUP BY ROLLUP (region, product)  (or GROUPING SETS / CUBE).
- Conditional aggregate (one pass, many metrics):
    count(*) FILTER (WHERE status = 'paid') AS paid, count(*) FILTER (WHERE status = 'refunded') AS refunded
- Median / percentiles: percentile_cont(0.5) WITHIN GROUP (ORDER BY latency_ms)  -- 0.95 for p95.
- Period-over-period (MoM, WoW): LAG(metric) OVER (ORDER BY month) then compute the delta / ratio.
- Distinct counts by dimension: count(DISTINCT user_id).
- Cohort / retention: CTE for each user's first-event month, then join later activity and bucket by months-since-first.
- Funnel: count(*) FILTER (WHERE reached_step_2) over count(*) FILTER (WHERE reached_step_1).

RULES:
- One statement per tool call. Never chain with ';'.
- Read-only for questions; only write when the user clearly asks to change data.
- When a question is ambiguous or a needed table/column isn't visible in the schema, ASK or say you can't answer — do NOT invent columns or guess business definitions. A confident wrong number is worse than a clarifying question.
- Add explicit ORDER BY when you return ranked or time-series results.
- Use LIMIT on exploratory queries.
"#;
