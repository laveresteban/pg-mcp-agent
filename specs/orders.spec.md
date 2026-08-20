# Orders analytics

A sample semantic-layer spec. Edit the glossary and examples to match your real
schema, then run `pg-mcp-agent verify` to check them against the database.

## Glossary
- **active customer**: a customer with at least one order in the last 90 days
- **revenue**: SUM(order_items.quantity * order_items.unit_price)
- **AOV**: average order value, revenue divided by the number of distinct orders

## Example: monthly revenue
Question: revenue by month
Question: how much did we make each month
Expect: contains revenue
```sql
SELECT date_trunc('month', o.created_at) AS month,
       SUM(oi.quantity * oi.unit_price)  AS revenue
FROM orders o
JOIN order_items oi ON oi.order_id = o.id
GROUP BY 1
ORDER BY 1;
```

## Example: top 5 products last quarter
Question: what were the top 5 products by revenue last quarter?
Expect: non-empty
```sql
SELECT p.name,
       SUM(oi.quantity * oi.unit_price) AS revenue
FROM order_items oi
JOIN products p ON p.id = oi.product_id
JOIN orders  o ON o.id = oi.order_id
WHERE o.created_at >= date_trunc('quarter', now()) - interval '3 months'
  AND o.created_at <  date_trunc('quarter', now())
GROUP BY p.name
ORDER BY revenue DESC
LIMIT 5;
```

## Metric: daily sales rollup (ClickHouse)
Question: daily sales totals for the dashboard
Expect: contains revenue
Backend: clickhouse
Engine: SummingMergeTree()
Order by: day
```sql
SELECT toDate(created_at) AS day,
       sum(quantity * unit_price) AS revenue,
       count() AS orders
FROM order_items
GROUP BY day
ORDER BY day;
```

## Example: new customers per week
Question: how many new customers each week
Expect: contains week
```sql
SELECT date_trunc('week', first_order) AS week, count(*) AS new_customers
FROM (
  SELECT customer_id, min(created_at) AS first_order
  FROM orders
  GROUP BY customer_id
) f
GROUP BY 1
ORDER BY 1;
```
