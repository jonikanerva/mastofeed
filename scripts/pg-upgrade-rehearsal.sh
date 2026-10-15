#!/usr/bin/env bash
# Rehearsal of a PostgreSQL major upgrade with pg_upgrade (STACK.md §15
# Infrastructure). It needs no credential and touches no other server.
#
# 1. Start a cluster of the old major version and fill it the way production
#    was filled: the TypeScript service of the revert path (docs/adr/0002)
#    migrates and polls, then the Rust service migrates and polls. Both poll
#    a local stub of the Mastodon timeline (tests/fixtures/mastodon).
# 2. Upgrade the cluster with pg_upgrade to the new major version.
# 3. Compare the data before and after the upgrade.
# 4. Run the Rust service and the TypeScript service on the upgraded cluster.
#    Both syncs must write 0 rows, and all four feeds must be byte-equal.
#
# The script does not exercise the upgrade job of Railway. It shows that the
# schema, the data, and both services work across pg_upgrade.
#
# Needs: both versions installed by mise (`mise install
# conda:postgresql@<version>`), Node.js 24 with npm, python3, and
# `mise run build`. Uses 127.0.0.1 ports 55433 and 48091-48093.
# REHEARSAL_KEEP=1 keeps the work directory with its logs.
# Usage: scripts/pg-upgrade-rehearsal.sh <old version> <new version>
set -euo pipefail

OLD_VERSION="${1:?usage: $0 <old version> <new version>}"
NEW_VERSION="${2:?usage: $0 <old version> <new version>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TS_COMMIT="d17168d"
PG_PORT=55433
TS_PORT=48091
RUST_PORT=48092
STUB_PORT=48093
OLD_BIN="$(mise where "conda:postgresql@$OLD_VERSION")/bin"
NEW_BIN="$(mise where "conda:postgresql@$NEW_VERSION")/bin"
WORK="$(mktemp -d)"
STUB_PID=""
SERVICE_PID=""
RUNNING_BIN=""

cleanup() {
  for pid in "$SERVICE_PID" "$STUB_PID"; do
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null || true
  done
  if [ -n "$RUNNING_BIN" ]; then
    "$RUNNING_BIN/pg_ctl" --pgdata="$WORK/$RUNNING_DATA" --mode=fast stop >/dev/null 2>&1 || true
  fi
  if [ -n "${REHEARSAL_KEEP:-}" ]; then echo "rehearsal: kept $WORK" >&2; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

[ -x "$ROOT/target/release/mastofeed" ] || { echo "rehearsal: run mise run build first" >&2; exit 2; }
command -v npm >/dev/null || { echo "rehearsal: npm is required" >&2; exit 2; }
for port in $PG_PORT $TS_PORT $RUST_PORT $STUB_PORT; do
  if nc -z 127.0.0.1 "$port" 2>/dev/null; then
    echo "rehearsal: port $port is in use" >&2
    exit 2
  fi
done

export DATABASE_URL="postgres://postgres@127.0.0.1:$PG_PORT/mastofeed"
export MASTODON_BASE_URL="http://127.0.0.1:$STUB_PORT"
# A placeholder: the stub accepts any token.
export MASTODON_ACCESS_TOKEN="rehearsal-placeholder"
export HOME_PAGE_URL="https://example.invalid/"
export CRON_SCHEDULE="0 0 1 1 *"
export LOG_FORMAT=json

pg_start() { # $1 = bin directory, $2 = data directory name
  "$1/pg_ctl" --pgdata="$WORK/$2" --log="$WORK/$2.log" --wait \
    -o "-p $PG_PORT -c unix_socket_directories= -c listen_addresses=127.0.0.1" start >/dev/null
  RUNNING_BIN="$1"
  RUNNING_DATA="$2"
}

pg_stop() {
  "$RUNNING_BIN/pg_ctl" --pgdata="$WORK/$RUNNING_DATA" --mode=fast stop >/dev/null
  RUNNING_BIN=""
}

psql_value() { # $1 = SQL
  "$NEW_BIN/psql" --host=127.0.0.1 --port=$PG_PORT --username=postgres --dbname=mastofeed \
    --quiet --tuples-only --no-align --command="$1"
}

# Every column of every row, and the bookkeeping of both migrators. xmin is
# not compared: pg_upgrade keeps it, but the comparison must not depend on it.
snapshot() { # $1 = output file
  {
    psql_value "SELECT current_setting('server_version')" | sed 's/^/# server /'
    psql_value "SELECT id, created_at, url, content, spoiler_text, account_id, account_username,
      account_display_name, account_url, raw::text, edited_at FROM mastodon_statuses ORDER BY id"
    psql_value "SELECT id, hash, created_at FROM drizzle.__drizzle_migrations ORDER BY id"
    psql_value "SELECT version, description, success, checksum FROM _sqlx_migrations ORDER BY version"
    psql_value "SELECT indexname, indexdef FROM pg_indexes WHERE tablename = 'mastodon_statuses' ORDER BY 1"
  } >"$1"
}

# The row versions. A rewrite of a row changes its xmin, so equal xmin
# values after a poll show that the poll wrote nothing. pg_upgrade keeps xmin.
xmin_snapshot() { # $1 = output file
  psql_value "SELECT id, xmin FROM mastodon_statuses ORDER BY id" >"$1"
}

wait_for() { # $1 = file, $2 = text
  for _ in $(seq 1 90); do
    if grep -q "$2" "$1" 2>/dev/null; then return 0; fi
    sleep 1
  done
  echo "rehearsal: timeout while waiting for: $2" >&2
  return 1
}

stop_service() {
  kill -TERM "$SERVICE_PID"
  wait "$SERVICE_PID" 2>/dev/null || true
  SERVICE_PID=""
}

ts_poll() { # $1 = feed output file, $2 = log name
  local requests
  requests="$(wc -l <"$WORK/stub.log")"
  (cd "$WORK/ts" && PORT=$TS_PORT node_modules/.bin/tsx src/index.ts) >"$WORK/ts-$2.log" 2>&1 &
  SERVICE_PID="$!"
  wait_for "$WORK/ts-$2.log" "Server listening"
  # The TypeScript service logs nothing after a sync. Wait until the stub
  # has answered a new request, then give the upsert time to commit.
  for _ in $(seq 1 90); do
    [ "$(wc -l <"$WORK/stub.log")" -gt "$requests" ] && break
    sleep 1
  done
  sleep 3
  # The feed answers 500 while the table is empty.
  for _ in $(seq 1 30); do
    curl --silent --fail "http://127.0.0.1:$TS_PORT/feed.json" >"$1" && break
    sleep 1
  done
  [ -s "$1" ] || { echo "rehearsal: no TypeScript feed" >&2; return 1; }
  stop_service
}

rust_poll() { # $1 = feed output file, $2 = log name
  "$ROOT/target/release/mastofeed" migrate >"$WORK/rust-$2.log" 2>&1
  PORT=$RUST_PORT "$ROOT/target/release/mastofeed" serve >>"$WORK/rust-$2.log" 2>&1 &
  SERVICE_PID="$!"
  wait_for "$WORK/rust-$2.log" "timeline sync finished"
  curl --silent --fail "http://127.0.0.1:$RUST_PORT/feed.json" >"$1"
  stop_service
  grep '"timeline sync finished"' "$WORK/rust-$2.log" | tee "$WORK/rust-$2.sync" | python3 -c \
    "import json,sys; e=json.loads(sys.stdin.readline()); print('rust sync $2: received=%s written=%s rejected=%s' % (e.get('received'), e.get('written'), e.get('rejected')))"
}

echo "rehearsal: Mastodon stub on port $STUB_PORT"
python3 - "$ROOT/crates/server/tests/fixtures/mastodon/recent.json" "$STUB_PORT" >"$WORK/stub.log" 2>&1 <<'PY' &
import http.server, sys
body = open(sys.argv[1], "rb").read()
class Stub(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        ok = self.path.startswith("/api/v1/timelines/home")
        self.send_response(200 if ok else 404)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body if ok else b"{}")
        print(self.path.split("?")[0], flush=True)
    def log_message(self, *args):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[2])), Stub).serve_forever()
PY
STUB_PID="$!"

echo "rehearsal: TypeScript service of $TS_COMMIT"
mkdir -p "$WORK/ts"
git -C "$ROOT" archive "$TS_COMMIT" | tar -x -C "$WORK/ts"
(cd "$WORK/ts" && npm ci --no-audit --no-fund --loglevel=error >/dev/null)

echo "rehearsal: PostgreSQL $OLD_VERSION"
# The same initdb options as the Railway image: no data checksums before 18.
"$OLD_BIN/initdb" --pgdata="$WORK/old" --username=postgres --auth=trust --encoding=UTF8 >/dev/null
pg_start "$OLD_BIN" old
"$OLD_BIN/createdb" --host=127.0.0.1 --port=$PG_PORT --username=postgres mastofeed
ts_poll "$WORK/feed-ts-old.json" old
xmin_snapshot "$WORK/xmin-ts-old.txt"
rust_poll "$WORK/feed-rust-old.json" old
xmin_snapshot "$WORK/xmin-rust-old.txt"
snapshot "$WORK/before.txt"
pg_stop

echo "rehearsal: pg_upgrade $OLD_VERSION -> $NEW_VERSION"
CHECKSUMS="--no-data-checksums"
"$OLD_BIN/pg_controldata" "$WORK/old" | grep -q 'Data page checksum version: *0' || CHECKSUMS="--data-checksums"
"$NEW_BIN/initdb" --pgdata="$WORK/new" --username=postgres --auth=trust --encoding=UTF8 "$CHECKSUMS" >/dev/null
(cd "$WORK" && "$NEW_BIN/pg_upgrade" --old-datadir="$WORK/old" --new-datadir="$WORK/new" \
  --old-bindir="$OLD_BIN" --new-bindir="$NEW_BIN" --username=postgres --check >"$WORK/check.log")
tail -n 1 "$WORK/check.log"
(cd "$WORK" && "$NEW_BIN/pg_upgrade" --old-datadir="$WORK/old" --new-datadir="$WORK/new" \
  --old-bindir="$OLD_BIN" --new-bindir="$NEW_BIN" --username=postgres >"$WORK/upgrade.log")
tail -n 1 "$WORK/upgrade.log"

echo "rehearsal: PostgreSQL $NEW_VERSION"
pg_start "$NEW_BIN" new
snapshot "$WORK/after.txt"
xmin_snapshot "$WORK/xmin-upgraded.txt"
rust_poll "$WORK/feed-rust-new.json" new
ts_poll "$WORK/feed-ts-new.json" new
xmin_snapshot "$WORK/xmin-polls-new.txt"
snapshot "$WORK/after-polls.txt"
pg_stop

python3 - "$WORK" <<'PY'
import sys, hashlib
w = sys.argv[1]
def rows(name):
    return [l for l in open(f"{w}/{name}") if not l.startswith("# server")]
def server(name):
    return next(l for l in open(f"{w}/{name}") if l.startswith("# server")).split()[-1]
before, after, polls = rows("before.txt"), rows("after.txt"), rows("after-polls.txt")
feeds = {n: open(f"{w}/feed-{n}.json", "rb").read() for n in ("ts-old", "rust-old", "rust-new", "ts-new")}
print(f"server before: {server('before.txt')}, after: {server('after.txt')}")
print(f"snapshot lines: {len(before)}; equal after pg_upgrade: {before == after}; "
      f"equal after both polls on the new version: {before == polls}")
for name, data in feeds.items():
    print(f"feed {name}: {len(data)} bytes, sha256 {hashlib.sha256(data).hexdigest()[:16]}")
same = len(set(feeds.values())) == 1
print(f"all feeds byte-equal: {same}")
import json
written = [json.loads(open(f"{w}/rust-{n}.sync").readline()).get("written") for n in ("old", "new")]
print(f"rust syncs written: {written}")
xmin = {n: open(f"{w}/xmin-{n}.txt").read() for n in ("ts-old", "rust-old", "upgraded", "polls-new")}
no_rewrite_old = xmin["ts-old"] == xmin["rust-old"]
no_rewrite_new = xmin["upgraded"] == xmin["polls-new"]
print(f"rows rewritten: by the Rust poll on the old version: {not no_rewrite_old}; "
      f"by both polls on the new version: {not no_rewrite_new}")
ok = (before == after == polls and same and written == [0, 0]
      and no_rewrite_old and no_rewrite_new)
print("REHEARSAL: PASS" if ok else "REHEARSAL: FAIL")
sys.exit(0 if ok else 1)
PY
