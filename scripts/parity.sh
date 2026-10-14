#!/usr/bin/env bash
# Owner-run live parity check (STACK.md §14, "Live checks").
#
# The check polls the owner's live home timeline once with the TypeScript
# service of commit d17168d and once with the Rust service. Both use the same
# database on the isolated test server, never the database of .env. Then it
# compares the two /feed.json documents item by item.
#
# Needs: .env with MASTODON_BASE_URL, MASTODON_ACCESS_TOKEN, and
# HOME_PAGE_URL; Node.js 24 with npm; `mise run db:start`; `mise run build`.
# Run it with `mise run parity`. The output shows counts and status ids only.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TS_COMMIT="d17168d"
TS_PORT=48091
RUST_PORT=48092
DATABASE="parity_live"
PG_BIN="$(mise where conda:postgresql)/bin"
PSQL=("$PG_BIN/psql" --host=127.0.0.1 --port=55432 --username=postgres --quiet)
WORK="$(mktemp -d)"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null || true
  done
  "${PSQL[@]}" --command="DROP DATABASE IF EXISTS $DATABASE" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

for name in MASTODON_BASE_URL MASTODON_ACCESS_TOKEN HOME_PAGE_URL; do
  if [ -z "${!name:-}" ]; then
    echo "parity: $name is not set (.env)" >&2
    exit 2
  fi
done
command -v npm >/dev/null || { echo "parity: npm is required" >&2; exit 2; }

# Only the isolated server. This replaces any DATABASE_URL of .env.
export DATABASE_URL="postgres://postgres@127.0.0.1:55432/$DATABASE"
"${PSQL[@]}" --command="DROP DATABASE IF EXISTS $DATABASE" --command="CREATE DATABASE $DATABASE"

echo "parity: preparing the TypeScript service of $TS_COMMIT"
mkdir -p "$WORK/ts"
git -C "$ROOT" archive "$TS_COMMIT" | tar -x -C "$WORK/ts"
(cd "$WORK/ts" && npm ci --no-audit --no-fund --loglevel=error >/dev/null)

wait_for_sync() { # $1 = log file, $2 = text
  for _ in $(seq 1 60); do
    if grep -q "$2" "$1" 2>/dev/null; then return 0; fi
    sleep 1
  done
  echo "parity: timeout while waiting for: $2" >&2
  return 1
}

echo "parity: polling with the TypeScript service"
(cd "$WORK/ts" && PORT=$TS_PORT CRON_SCHEDULE="0 0 1 1 *" node_modules/.bin/tsx src/index.ts) >"$WORK/ts.log" 2>&1 &
PIDS+=("$!")
wait_for_sync "$WORK/ts.log" "Server listening"
# The TypeScript service logs nothing after a sync; wait until rows exist.
for _ in $(seq 1 60); do
  rows="$("${PSQL[@]}" --dbname=$DATABASE --tuples-only --no-align \
    --command="SELECT count(*) FROM mastodon_statuses" 2>/dev/null || echo 0)"
  [ "${rows:-0}" -gt 0 ] && break
  sleep 1
done
sleep 2
curl --silent --fail "http://127.0.0.1:$TS_PORT/feed.json" >"$WORK/ts-feed.json"
kill -TERM "${PIDS[0]}"
wait "${PIDS[0]}" 2>/dev/null || true
PIDS=()

echo "parity: polling with the Rust service"
LOG_FORMAT=json "$ROOT/target/release/mastofeed" migrate >"$WORK/rust.log"
# JSON logs, so the comparison below can read the sync result.
PORT=$RUST_PORT CRON_SCHEDULE="0 0 1 1 *" LOG_FORMAT=json "$ROOT/target/release/mastofeed" serve >>"$WORK/rust.log" 2>&1 &
PIDS+=("$!")
wait_for_sync "$WORK/rust.log" "timeline sync"
curl --silent --fail "http://127.0.0.1:$RUST_PORT/feed.json" >"$WORK/rust-feed.json"

python3 - "$WORK/ts-feed.json" "$WORK/rust-feed.json" "$WORK/rust.log" <<'PY'
import json, sys

ts = json.load(open(sys.argv[1]))
rust = json.load(open(sys.argv[2]))
sync = [json.loads(line) for line in open(sys.argv[3]) if '"timeline sync' in line]

top_ts = {k: v for k, v in ts.items() if k != "items"}
top_rust = {k: v for k, v in rust.items() if k != "items"}
items_ts = {item["id"]: item for item in ts["items"]}
items_rust = {item["id"]: item for item in rust["items"]}
common = sorted(set(items_ts) & set(items_rust))
different = [i for i in common if items_ts[i] != items_rust[i]]

print(f"feed-level fields equal: {top_ts == top_rust}")
print(f"items: TypeScript {len(items_ts)}, Rust {len(items_rust)}, common {len(common)}")
print(f"common items that differ: {len(different)}")
for status_id in different:
    keys = sorted(k for k in set(items_ts[status_id]) | set(items_rust[status_id])
                  if items_ts[status_id].get(k) != items_rust[status_id].get(k))
    print(f"  {status_id}: {', '.join(keys)}")
print(f"only in TypeScript: {sorted(set(items_ts) - set(items_rust))}")
print(f"only in Rust (new since the TypeScript poll): {sorted(set(items_rust) - set(items_ts))}")
if sync:
    print(f"Rust sync on the TypeScript rows: {sync[0].get('message')}, "
          f"written={sync[0].get('written')}, code={sync[0].get('code')}")
ok = top_ts == top_rust and not different
print("PARITY: PASS" if ok else "PARITY: FAIL")
sys.exit(0 if ok else 1)
PY
