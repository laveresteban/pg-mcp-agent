# Postgres + ClickHouse copilot demo

Verified metrics that run against the bundled mock servers — no database needed.
Postgres specs route to the mock `pg` server; the ClickHouse spec routes to the
mock `ch` server. Run:

```
cargo run -- verify config.pgch.mock.json
```

## Glossary
- **revenue**: SUM(order_items.quantity * order_items.unit_price)
- **region**: the sales territory a row belongs to (east / west)

## Example: sales by region (Postgres)
Question: total sales by region
Backend: postgres
Expect: contains east
```sql
SELECT region, SUM(sales) AS sales
FROM sales
GROUP BY region
ORDER BY sales DESC;
```

## Metric: daily revenue rollup (ClickHouse)
Question: daily revenue for the dashboard
Backend: clickhouse
Engine: SummingMergeTree()
Order by: day
Expect: contains revenue
```sql
SELECT toDate(created_at) AS day,
       sum(quantity * unit_price) AS revenue,
       count() AS orders
FROM order_items
GROUP BY day
ORDER BY day;
```
