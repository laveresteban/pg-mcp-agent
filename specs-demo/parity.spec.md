# Cross-engine parity

The same metric defined once on each engine. `pg-mcp-agent parity
config.pgch.mock.json` runs both and proves they compute to the SAME number —
the check a CDC pipe can't give you (it moves rows; it doesn't prove the rollup
still equals the source). The `Parity:` key links the two specs into one group.

Both queries also pass ordinary `verify` on their own.

## Metric: total revenue (Postgres source)
Question: total revenue from the source of truth
Backend: postgres
Parity: total revenue
Expect: contains 4580
```sql
SELECT SUM(quantity * unit_price) AS revenue_total
FROM order_items;
```

## Metric: total revenue (ClickHouse rollup)
Question: total revenue from the analytics rollup
Backend: clickhouse
Parity: total revenue
Expect: contains 4580
```sql
SELECT sum(quantity * unit_price) AS revenue_total
FROM order_items;
```
