# Pagila (DVD rental) analytics

Semantic layer for the **Pagila** sample database (the Postgres port of Sakila —
a DVD-rental store). Loaded by the agent as grounding, and checked by
`pg-mcp-agent verify` against the live database.

Schema highlights: `film`, `category`, `film_category`, `actor`, `film_actor`,
`inventory`, `rental`, `payment`, `customer`, `store`, `staff`, `address`,
`city`, `country`.

## Glossary
- **revenue**: SUM(payment.amount) — money actually collected from customers.
- **rental**: one row in `rental`; a customer checking out one `inventory` copy.
- **active customer**: a customer with `customer.active = 1`.
- **catalog**: the set of distinct `film` titles (not physical `inventory` copies).
- **inventory copy**: a physical DVD in a store (`inventory`), linked to a `film`.
- **category**: a genre in `category.name`, joined via `film_category`.

## Example: revenue by month
Question: revenue by month
Question: how much money did we take in each month
Expect: contains revenue
```sql
SELECT date_trunc('month', p.payment_date) AS month,
       SUM(p.amount)                        AS revenue
FROM payment p
GROUP BY 1
ORDER BY 1;
```

## Example: top 10 films by revenue
Question: what are the top 10 films by revenue?
Question: which movies made the most money
Expect: non-empty
```sql
SELECT f.title,
       SUM(p.amount) AS revenue
FROM payment p
JOIN rental r    ON r.rental_id = p.rental_id
JOIN inventory i ON i.inventory_id = r.inventory_id
JOIN film f      ON f.film_id = i.film_id
GROUP BY f.title
ORDER BY revenue DESC
LIMIT 10;
```

## Example: revenue by category
Question: revenue by category
Question: which genres earn the most
Expect: contains revenue
```sql
SELECT c.name AS category,
       SUM(p.amount) AS revenue
FROM payment p
JOIN rental r        ON r.rental_id = p.rental_id
JOIN inventory i     ON i.inventory_id = r.inventory_id
JOIN film_category fc ON fc.film_id = i.film_id
JOIN category c      ON c.category_id = fc.category_id
GROUP BY c.name
ORDER BY revenue DESC;
```

## Example: top 10 customers by spend
Question: who are our top 10 customers by spend?
Expect: non-empty
```sql
SELECT c.first_name || ' ' || c.last_name AS customer,
       SUM(p.amount)                       AS total_spent,
       COUNT(*)                            AS payments
FROM payment p
JOIN customer c ON c.customer_id = p.customer_id
GROUP BY c.customer_id, customer
ORDER BY total_spent DESC
LIMIT 10;
```

## Example: revenue by store
Question: how does revenue compare between stores?
Expect: contains revenue
```sql
SELECT s.store_id,
       ci.city         AS store_city,
       SUM(p.amount)   AS revenue
FROM payment p
JOIN staff st  ON st.staff_id = p.staff_id
JOIN store s   ON s.store_id = st.store_id
JOIN address a ON a.address_id = s.address_id
JOIN city ci   ON ci.city_id = a.city_id
GROUP BY s.store_id, ci.city
ORDER BY revenue DESC;
```

## Example: rentals per day
Question: how many rentals per day
Expect: contains rentals
```sql
SELECT date_trunc('day', r.rental_date) AS day,
       COUNT(*)                          AS rentals
FROM rental r
GROUP BY 1
ORDER BY 1;
```

## Example: most rented films
Question: what are the most rented films?
Expect: non-empty
```sql
SELECT f.title,
       COUNT(*) AS rentals
FROM rental r
JOIN inventory i ON i.inventory_id = r.inventory_id
JOIN film f      ON f.film_id = i.film_id
GROUP BY f.title
ORDER BY rentals DESC
LIMIT 10;
```
