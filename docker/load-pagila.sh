#!/bin/sh
# One-shot loader: download the public Pagila dataset and load it into the
# Postgres container the first time only. Idempotent — if the `film` table
# already exists we assume the data is loaded and exit cleanly.
set -eu

SCHEMA_URL="https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-schema.sql"
DATA_URL="https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-data.sql"

export PGPASSWORD="$POSTGRES_PASSWORD"
PSQL="psql -v ON_ERROR_STOP=1 -h $POSTGRES_HOST -U $POSTGRES_USER -d $POSTGRES_DB -q"

echo "load-pagila: installing curl + postgresql-client..."
apk add --no-cache curl postgresql-client >/dev/null

echo "load-pagila: waiting for Postgres at $POSTGRES_HOST..."
until pg_isready -h "$POSTGRES_HOST" -U "$POSTGRES_USER" -d "$POSTGRES_DB" >/dev/null 2>&1; do
  sleep 1
done

# Already loaded? (to_regclass returns NULL when the table is absent)
LOADED=$($PSQL -tAc "SELECT to_regclass('public.film') IS NOT NULL;")
if [ "$LOADED" = "t" ]; then
  echo "load-pagila: film table already present — nothing to do."
  exit 0
fi

# Pagila's dump assigns object ownership to a `postgres` role. Our container's
# superuser is $POSTGRES_USER, so create that role first (idempotent).
echo "load-pagila: ensuring 'postgres' role exists..."
$PSQL -c "DO \$\$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='postgres') THEN CREATE ROLE postgres SUPERUSER LOGIN; END IF; END \$\$;"

echo "load-pagila: downloading schema..."
curl -sSL "$SCHEMA_URL" -o /tmp/pagila-schema.sql
echo "load-pagila: downloading data (~13 MB)..."
curl -sSL "$DATA_URL" -o /tmp/pagila-data.sql

echo "load-pagila: resetting public schema for a clean load..."
$PSQL -c "DROP SCHEMA IF EXISTS public CASCADE; CREATE SCHEMA public AUTHORIZATION postgres;"

echo "load-pagila: loading schema..."
$PSQL -f /tmp/pagila-schema.sql
echo "load-pagila: loading data..."
$PSQL -f /tmp/pagila-data.sql

echo "load-pagila: done. Row counts:"
$PSQL -c "SELECT 'film' t, count(*) FROM film UNION ALL SELECT 'rental', count(*) FROM rental UNION ALL SELECT 'payment', count(*) FROM payment;"
