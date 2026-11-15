#!/usr/bin/env bash
# Rehearsal of a PostgreSQL major upgrade with pg_upgrade (STACK.md §15
# Infrastructure). It needs no credential and touches no other server. The
# PostgreSQL servers and pg_upgrade run in Apple containers of the official
# images; the Mac needs no PostgreSQL install.
#
# 1. Start a server of the old image on a new volume and fill it the way
#    production was filled: the TypeScript service of the revert path
#    (docs/adr/0002) migrates and polls, then the Rust service migrates and
#    polls. Both poll a local stub of the Mastodon timeline
#    (tests/fixtures/mastodon).
# 2. Copy the binaries of the old major version from the old image to the
#    volume. In a container of the new image, put them in their Debian
#    paths and run pg_upgrade from the old to the new data directory.
# 3. Compare the data before and after the upgrade.
# 4. Run the Rust service and the TypeScript service on the upgraded cluster
#    in a server of the new image. Both syncs must write 0 rows, and all four
#    feeds must be byte-equal.
#
# Limits: the old binaries run in the new image, so both images must have
# the same Debian release (the script checks it). The rehearsal does not
# exercise a data move between providers, such as the Neon move of
# STACK.md §15; a dump and restore shows other properties than pg_upgrade.
#
# Needs: the container tool, Node.js 24 with npm, python3, and
# `mise run build`. The HTTP ports are free ports of this run.
# REHEARSAL_KEEP=1 keeps the containers, the volume, and the work directory
# with its logs; `mise run db:gc` removes the kept containers and volume.
# Usage: scripts/pg-upgrade-rehearsal.sh <old image tag> <new image tag>
#   for example: mise run pg:rehearse 17.11 18.7
set -euo pipefail

OLD_TAG="${1:?usage: $0 <old image tag> <new image tag>}"
NEW_TAG="${2:?usage: $0 <old image tag> <new image tag>}"
# shellcheck source=scripts/db.sh
. "$(dirname "$0")/db.sh"
image_ref() { # $1 = tag or full reference
  case "$1" in */* | *@*) echo "$1" ;; *) echo "docker.io/library/postgres:$1" ;; esac
}
OLD_IMAGE="$(image_ref "$OLD_TAG")"
NEW_IMAGE="$(image_ref "$NEW_TAG")"
TS_COMMIT="d17168d"
RUN="mastofeed-rehearsal-$WORKTREE_ID-$(openssl rand -hex 4)"
OWNER="$(owner_of $$)"
PASSWORD="$(openssl rand -hex 16)"
WORK="$(mktemp -d)"
STUB_PID=""
SERVICE_PID=""
PG_PORT=""

free_port() {
  python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}
TS_PORT="$(free_port)"
RUST_PORT="$(free_port)"
STUB_PORT="$(free_port)"

cleanup() {
  for pid in "$SERVICE_PID" "$STUB_PID"; do
    [ -n "$pid" ] && kill -TERM "$pid" 2>/dev/null || true
  done
  if [ -n "${REHEARSAL_KEEP:-}" ]; then
    mkdir -p "$COMMON/kept"
    echo "$ROOT" >"$COMMON/kept/$RUN"
    echo "rehearsal: kept $WORK, container $RUN, and volume $RUN" >&2
  else
    remove_server "$RUN"
    container volume delete "$RUN" >/dev/null 2>&1 || true
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

[ -x "$ROOT/target/release/mastofeed" ] || { echo "rehearsal: run mise run build first" >&2; exit 2; }
command -v npm >/dev/null || { echo "rehearsal: npm is required" >&2; exit 2; }
require_container
mkdir -p "$STATE"
reap_abandoned

export MASTODON_BASE_URL="http://127.0.0.1:$STUB_PORT"
# A placeholder: the stub accepts any token.
export MASTODON_ACCESS_TOKEN="rehearsal-placeholder"
export HOME_PAGE_URL="https://example.invalid/"
export CRON_SCHEDULE="0 0 1 1 *"
export LOG_FORMAT=json

LABELS=(--label mastofeed.role=rehearsal --label "mastofeed.worktree=$WORKTREE_ID"
  --label "mastofeed.owner=$OWNER" --label "mastofeed.run=$RUN")

pg_start() { # $1 = image, $2 = data directory name on the volume
  IMAGE="$1"
  run_server "$RUN" --volume "$RUN:/var/lib/postgresql" "${LABELS[@]}" \
    --env "PGDATA=/var/lib/postgresql/$2" --env "POSTGRES_PASSWORD=$PASSWORD" \
    --env POSTGRES_DB=mastofeed --
  PG_PORT="$SERVER_PORT"
  wait_ready "$RUN" "$PG_PORT"
  export DATABASE_URL="postgres://postgres:$PASSWORD@127.0.0.1:$PG_PORT/mastofeed"
}

pg_stop() {
  container stop "$RUN" >/dev/null
  container delete "$RUN" >/dev/null
}

# A one-off container on the volume. $1 = image, $2 = shell script.
on_volume() {
  container run --rm --name "$RUN-step" --cpus "$CPUS" --memory "$MEMORY" "${LABELS[@]}" \
    --volume "$RUN:/var/lib/postgresql" --entrypoint /bin/bash "$1" -c "$2"
}

psql_value() { # $1 = SQL
  psql_in "$RUN" mastofeed --command="$1"
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

echo "rehearsal: PostgreSQL $OLD_IMAGE"
container image pull --platform linux/arm64 --progress none "$OLD_IMAGE" >/dev/null
container image pull --platform linux/arm64 --progress none "$NEW_IMAGE" >/dev/null
container volume create "${LABELS[@]}" "$RUN" >/dev/null
# The image entrypoint runs initdb with its defaults, like the Railway image
# (ghcr.io/railwayapp-templates/postgres-ssl): data checksums only from 18.
pg_start "$OLD_IMAGE" old
ts_poll "$WORK/feed-ts-old.json" old
xmin_snapshot "$WORK/xmin-ts-old.txt"
rust_poll "$WORK/feed-rust-old.json" old
xmin_snapshot "$WORK/xmin-rust-old.txt"
snapshot "$WORK/before.txt"
pg_stop

echo "rehearsal: pg_upgrade $OLD_IMAGE -> $NEW_IMAGE"
# The old binaries and their share directory, from the old image.
on_volume "$OLD_IMAGE" 'set -e
cd /var/lib/postgresql
major="$(cat old/PG_VERSION)"
rm -rf oldbin && mkdir oldbin
cp -a "/usr/lib/postgresql/$major" oldbin/lib
cp -a "/usr/share/postgresql/$major" oldbin/share
cut -d. -f1 /etc/debian_version >oldbin/debian_release'
# pg_upgrade in the new image. The old binaries go to their Debian paths, so
# they find their share directory and libraries as in the old image.
on_volume "$NEW_IMAGE" 'set -e
cd /var/lib/postgresql
old_major="$(cat old/PG_VERSION)"
if [ "$(cut -d. -f1 /etc/debian_version)" != "$(cat oldbin/debian_release)" ]; then
  echo "rehearsal: the images have different Debian releases" >&2
  exit 1
fi
cp -a oldbin/lib "/usr/lib/postgresql/$old_major"
cp -a oldbin/share "/usr/share/postgresql/$old_major"
checksums=--no-data-checksums
"/usr/lib/postgresql/$old_major/bin/pg_controldata" old | grep -q "Data page checksum version: *0" ||
  checksums=--data-checksums
rm -rf new upgrade && mkdir new upgrade && chown postgres:postgres new upgrade
# The locale of the image entrypoint: LANG=en_US.utf8.
gosu postgres initdb --pgdata=new --username=postgres --encoding=UTF8 --locale=en_US.utf8 \
  "$checksums" >/dev/null
cd upgrade
gosu postgres pg_upgrade --old-datadir=../old --new-datadir=../new \
  --old-bindir="/usr/lib/postgresql/$old_major/bin" --new-bindir="/usr/lib/postgresql/$PG_MAJOR/bin" \
  --username=postgres --check >check.log
tail -n 1 check.log
gosu postgres pg_upgrade --old-datadir=../old --new-datadir=../new \
  --old-bindir="/usr/lib/postgresql/$old_major/bin" --new-bindir="/usr/lib/postgresql/$PG_MAJOR/bin" \
  --username=postgres >upgrade.log
tail -n 1 upgrade.log
# pg_upgrade copies no configuration. The client authentication of the old
# cluster requires a password for connections through the published port.
cp ../old/pg_hba.conf ../new/pg_hba.conf'

echo "rehearsal: PostgreSQL $NEW_IMAGE"
pg_start "$NEW_IMAGE" new
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
    return next(l for l in open(f"{w}/{name}") if l.startswith("# server"))[len("# server "):].strip()
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
