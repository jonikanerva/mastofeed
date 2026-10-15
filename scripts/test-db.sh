#!/usr/bin/env bash
# The isolated PostgreSQL server of the tests and the SQLx query checks
# (STACK.md §3). It runs the pinned PostgreSQL 17 from mise, listens only on
# 127.0.0.1:55432, trusts local connections, and keeps its data in
# .tmp/postgres. It never touches another server.
#
# The server offers TLS with a self-signed X.509 v3 certificate, like the
# Railway PostgreSQL of production. It also accepts connections without TLS.
#
# Usage: scripts/test-db.sh start|stop|reset
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DATA="$ROOT/.tmp/postgres"
PORT=55432
URL="postgres://postgres@127.0.0.1:$PORT/mastofeed_dev"
BIN="$(mise where conda:postgresql)/bin"

start() {
  if [ ! -f "$DATA/PG_VERSION" ]; then
    mkdir -p "$DATA"
    "$BIN/initdb" --pgdata="$DATA" --username=postgres --auth=trust --encoding=UTF8 --no-locale >/dev/null
  fi
  if [ ! -f "$DATA/server.crt" ]; then
    # `req -x509` writes a version 3 certificate. rustls rejects older versions.
    "$BIN/openssl" req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=127.0.0.1" \
      -keyout "$DATA/server.key" -out "$DATA/server.crt" >/dev/null 2>&1
    chmod 600 "$DATA/server.key"
  fi
  # An older version of this script started the server without TLS.
  # Restart that server with TLS.
  if "$BIN/pg_ctl" --pgdata="$DATA" status >/dev/null 2>&1 &&
    ! "$BIN/psql" --host=127.0.0.1 --port=$PORT --username=postgres --quiet --tuples-only \
      --command="SHOW ssl" | grep -q on; then
    stop
  fi
  if ! "$BIN/pg_ctl" --pgdata="$DATA" status >/dev/null 2>&1; then
    "$BIN/pg_ctl" --pgdata="$DATA" --log="$DATA/server.log" --wait \
      -o "-p $PORT -c unix_socket_directories= -c listen_addresses=127.0.0.1 -c fsync=off -c ssl=on" start >/dev/null
  fi
  "$BIN/psql" --host=127.0.0.1 --port=$PORT --username=postgres --quiet --tuples-only \
    --command="SELECT 1 FROM pg_database WHERE datname = 'mastofeed_dev'" | grep -q 1 ||
    "$BIN/createdb" --host=127.0.0.1 --port=$PORT --username=postgres mastofeed_dev
  # --no-dotenv: never read a .env file.
  DATABASE_URL="$URL" sqlx migrate run --no-dotenv --source "$ROOT/migrations" >/dev/null
  echo "test database ready: $URL"
}

stop() {
  if [ -f "$DATA/PG_VERSION" ]; then
    "$BIN/pg_ctl" --pgdata="$DATA" --mode=fast stop >/dev/null 2>&1 || true
  fi
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  reset) stop; rm -rf "$DATA"; start ;;
  *) echo "usage: $0 start|stop|reset" >&2; exit 2 ;;
esac
