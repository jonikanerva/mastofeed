#!/usr/bin/env bash
# Local PostgreSQL in Apple containers (STACK.md §3, docs/adr/0004).
#
# The Mac has no PostgreSQL install. Each database server is a container of
# the image in MASTOFEED_PG_IMAGE (mise.toml). There are two kinds:
#
# - Dev server: one per worktree. It keeps its data in a named volume, uses
#   the durable PostgreSQL defaults, and stays until `db:stop`. `dev` and
#   `migrate` use it.
# - Test server: one per independent run of `test`, `sqlx:check`,
#   `sqlx:prepare`, `verify`, or `parity`. Its data is in memory (tmpfs) with
#   fsync off. The run removes it at the end.
#
# The worktree id is a hash of the absolute checkout path, so two worktrees
# never share a container, even on the same branch. A test server name adds
# a random run id. Every server offers TLS with a self-signed X.509 v3
# certificate and also accepts connections without TLS. Ports are published
# on 127.0.0.1 only. Every connection needs the password of its server.
#
# Usage: scripts/db.sh <command>
#   setup                       check the container tool, fetch the image
#   start | stop | info | psql  the dev server of this worktree
#   with-dev-db -- CMD...       run CMD with DATABASE_URL of the dev server
#   with-test-db [--share] -- CMD...
#                               run CMD with DATABASE_URL of a new test server
#   gc [--remove-orphaned-dev]  remove abandoned test servers and the kept
#                               test servers of this worktree; stop dev
#                               servers of deleted worktrees (remove them and
#                               their data with the flag)
#   sqlx prepare|check          cargo sqlx prepare under a worktree lock
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
IMAGE="${MASTOFEED_PG_IMAGE:?MASTOFEED_PG_IMAGE is not set; run the command through mise}"
WORKTREE_ID="$(printf '%s' "$ROOT" | shasum -a 256 | cut -c1-12)"
STATE="$ROOT/.tmp/db"
# Shared by all worktrees of this repository: the test server slots.
COMMON="$(cd "$ROOT" && cd "$(git rev-parse --git-common-dir)" && pwd -P)/mastofeed-db"
DEV_NAME="mastofeed-dev-$WORKTREE_ID"
DEV_VOLUME="mastofeed-dev-$WORKTREE_ID"
DEV_DATABASE="mastofeed"
TEST_DATABASE="mastofeed_dev"
# Host ports for published servers. A port in use fails `container run`;
# the script then tries another one.
PORT_MIN=41000
PORT_SPAN=7000
# Resource limits of one server (one lightweight VM each).
CPUS="${MASTOFEED_DB_CPUS:-2}"
MEMORY="${MASTOFEED_DB_MEMORY:-1G}"
# At most this many test servers run at the same time on this Mac, over all
# worktrees of this repository. A run waits for a free slot.
MAX_TEST="${MASTOFEED_DB_MAX_TEST:-4}"
# A lock or a slot wait stops after 2 hours (0.5 s steps). One verify can
# take longer than 10 minutes on a loaded Mac.
WAIT_LIMIT=14400
# Dev, test, and rehearsal servers use the same TLS arguments.
TLS_DIR=/var/lib/postgresql-tls
PG_TLS_ARGS=(-c ssl=on -c "ssl_cert_file=$TLS_DIR/server.crt" -c "ssl_key_file=$TLS_DIR/server.key")

say() { echo "db: $*" >&2; }
die() { say "$*"; exit 1; }

require_container() {
  command -v container >/dev/null ||
    die "the Apple container tool is missing; install it from https://github.com/apple/container/releases"
  container system status >/dev/null 2>&1 ||
    die "the container system service is not running; run: container system start"
}

# A process identity that survives PID reuse: PID plus start time. The
# start time text depends on the locale and the time zone, so fix both:
# a run started in one shell must look alive to a check from another shell.
owner_of() { # $1 = pid
  local started
  started="$(LC_ALL=C TZ=UTC ps -o lstart= -p "$1" 2>/dev/null | tr -s ' ' '_' | sed 's/_$//')" || return 1
  [ -n "$started" ] || return 1
  printf '%s@%s' "$1" "$started"
}

owner_alive() { # $1 = owner token
  local pid="${1%%@*}"
  [ "$(owner_of "$pid" 2>/dev/null || true)" = "$1" ]
}

# A directory lock with owner identity. A lock of a dead owner is stale.
lock() { # $1 = lock directory, $2 = description
  local dir="$1" waited=0 me
  me="$(owner_of $$)"
  mkdir -p "$(dirname "$dir")"
  until mkdir "$dir" 2>/dev/null; do
    if [ -f "$dir/owner" ] && ! owner_alive "$(cat "$dir/owner" 2>/dev/null)"; then
      rm -rf "$dir"
      continue
    fi
    [ $waited -eq 0 ] && say "waiting for $2"
    waited=$((waited + 1))
    [ $waited -gt $WAIT_LIMIT ] && die "timeout while waiting for $2 ($dir)"
    sleep 0.5
  done
  echo "$me" >"$dir/owner"
}

unlock() { rm -rf "$1"; }

# Lines: name<TAB>role<TAB>worktree<TAB>owner<TAB>path, for every volume
# with a mastofeed role. An empty field is "-": `read` merges adjacent tabs.
list_volumes() {
  container volume list --format json | python3 -c '
import json, sys
for v in json.load(sys.stdin):
    cfg = v.get("configuration", {})
    labels = cfg.get("labels") or {}
    if labels.get("mastofeed.role") is None:
        continue
    print("\t".join(str(x) or "-" for x in (cfg.get("name"), labels.get("mastofeed.role", ""),
        labels.get("mastofeed.worktree", ""), labels.get("mastofeed.owner", ""),
        labels.get("mastofeed.path", ""))))
'
}

# Lines: name<TAB>state<TAB>role<TAB>worktree<TAB>owner<TAB>port<TAB>run<TAB>path.
# An empty field is "-".
list_servers() {
  container list --all --format json | python3 -c '
import json, sys
for c in json.load(sys.stdin):
    cfg = c.get("configuration", {})
    labels = cfg.get("labels") or {}
    if labels.get("mastofeed.role") is None:
        continue
    ports = cfg.get("publishedPorts") or []
    port = ports[0].get("hostPort", "") if ports else ""
    print("\t".join(str(v) or "-" for v in (cfg.get("id"), c.get("status", {}).get("state", ""),
        labels.get("mastofeed.role", ""), labels.get("mastofeed.worktree", ""),
        labels.get("mastofeed.owner", ""), port, labels.get("mastofeed.run", ""),
        labels.get("mastofeed.path", ""))))
'
}

server_line() { # $1 = name
  list_servers | awk -F '\t' -v n="$1" '$1 == n'
}

remove_server() { # $1 = name
  container delete --force "$1" >/dev/null 2>&1 || true
}

# The container command: generate the TLS key pair in the container, then
# start the image entrypoint. A key from a bind mount would have the owner
# and mode of macOS; PostgreSQL requires a key that only its user can read.
server_command() {
  # `req -x509` writes a version 3 certificate. rustls rejects older versions.
  printf '%s' "set -e
mkdir -p $TLS_DIR
if [ ! -f $TLS_DIR/server.key ]; then
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj /CN=127.0.0.1 \
    -keyout $TLS_DIR/server.key -out $TLS_DIR/server.crt >/dev/null 2>&1
fi
chown -R postgres:postgres $TLS_DIR
chmod 600 $TLS_DIR/server.key
exec docker-entrypoint.sh \"\$@\""
}

# Start a server container on a free host port and set SERVER_PORT.
# $1 = name; then extra `container run` arguments, `--`, postgres arguments.
# macOS has bash 3.2: a command substitution does not inherit `set -e`, so
# the functions here return values in globals, not on standard output.
SERVER_PORT=""
run_server() {
  local name="$1" port attempt run_args=() pg_args=()
  shift
  while [ $# -gt 0 ] && [ "$1" != "--" ]; do run_args+=("$1"); shift; done
  [ $# -gt 0 ] && shift
  pg_args=("$@")
  for attempt in $(seq 1 20); do
    port=$((PORT_MIN + RANDOM % PORT_SPAN))
    if container run --detach --name "$name" --cpus "$CPUS" --memory "$MEMORY" \
      --publish "127.0.0.1:$port:5432" ${run_args[@]+"${run_args[@]}"} \
      --entrypoint /bin/sh "$IMAGE" -c "$(server_command)" postgres \
      postgres "${PG_TLS_ARGS[@]}" ${pg_args[@]+"${pg_args[@]}"} >"$STATE/run-$name.log" 2>&1; then
      rm -f "$STATE/run-$name.log"
      SERVER_PORT="$port"
      return 0
    fi
    remove_server "$name"
    if ! grep -q "Address already in use" "$STATE/run-$name.log"; then
      cat "$STATE/run-$name.log" >&2
      rm -f "$STATE/run-$name.log"
      die "container run failed for $name"
    fi
  done
  rm -f "$STATE/run-$name.log"
  die "no free host port after 20 attempts"
}

# Wait until the final server accepts TCP connections in the container and
# the published port answers on the host. The image runs a temporary server
# without TCP during the first initialization; it does not count.
wait_ready() { # $1 = name, $2 = port
  local i
  for i in $(seq 1 240); do
    if container exec "$1" pg_isready --quiet --host=127.0.0.1 --username=postgres 2>/dev/null &&
      nc -z 127.0.0.1 "$2" 2>/dev/null; then
      return 0
    fi
    sleep 0.25
  done
  container logs "$1" >&2 2>/dev/null || true
  die "server $1 did not become ready in 60 s"
}

psql_in() { # $1 = name, $2 = database, then psql arguments
  local name="$1" database="$2"
  shift 2
  container exec "$name" psql --quiet --no-psqlrc --username=postgres --dbname="$database" \
    --tuples-only --no-align -v ON_ERROR_STOP=1 "$@"
}

url_for() { # $1 = password, $2 = port, $3 = database
  printf 'postgres://postgres:%s@127.0.0.1:%s/%s' "$1" "$2" "$3"
}

# --- dev server ------------------------------------------------------------

dev_password() {
  mkdir -p "$STATE"
  if [ ! -s "$STATE/dev-password" ]; then
    (umask 077 && openssl rand -hex 16 >"$STATE/dev-password")
  fi
  cat "$STATE/dev-password"
}

# Ensure that the dev server of this worktree runs and set SERVER_PORT.
dev_ensure() {
  local line state port password
  require_container
  mkdir -p "$STATE"
  lock "$STATE/dev.lock" "the dev server lock of this worktree"
  dev_password >/dev/null
  password="$(cat "$STATE/dev-password")"
  if ! container volume inspect "$DEV_VOLUME" >/dev/null 2>&1; then
    container volume create --label mastofeed.role=dev --label "mastofeed.worktree=$WORKTREE_ID" \
      --label "mastofeed.path=$ROOT" "$DEV_VOLUME" >/dev/null
  fi
  line="$(server_line "$DEV_NAME")"
  state="$(cut -f2 <<<"$line")"
  port="$(cut -f6 <<<"$line")"
  if [ -n "$line" ] && [ "$state" != "running" ]; then
    # The published port can be in use now. Then recreate the container on
    # another port; the data stays in the volume.
    container start "$DEV_NAME" >/dev/null 2>&1 || { remove_server "$DEV_NAME"; line=""; }
  fi
  if [ -z "$line" ]; then
    # Durable PostgreSQL defaults: no fsync or commit settings here.
    run_server "$DEV_NAME" \
      --volume "$DEV_VOLUME:/var/lib/postgresql" \
      --label mastofeed.role=dev --label "mastofeed.worktree=$WORKTREE_ID" \
      --label "mastofeed.path=$ROOT" \
      --env "POSTGRES_PASSWORD=$password" --env "POSTGRES_DB=$DEV_DATABASE" --
    port="$SERVER_PORT"
  fi
  wait_ready "$DEV_NAME" "$port"
  # The password file is the source of truth. The image sets the password
  # only at the first start, so apply it again after a lost file.
  psql_in "$DEV_NAME" postgres --command="ALTER ROLE postgres PASSWORD '$password'" >/dev/null
  unlock "$STATE/dev.lock"
  SERVER_PORT="$port"
}

dev_url() { # $1 = port
  url_for "$(cat "$STATE/dev-password")" "$1" "$DEV_DATABASE"
}

cmd_start() {
  dev_ensure
  say "dev server ready: $DEV_NAME on 127.0.0.1:$SERVER_PORT, database $DEV_DATABASE (mise run db:info)"
}

cmd_stop() {
  require_container
  if [ -n "$(server_line "$DEV_NAME")" ]; then
    container stop "$DEV_NAME" >/dev/null
    say "dev server stopped: $DEV_NAME (the data stays in volume $DEV_VOLUME)"
  else
    say "no dev server for this worktree"
  fi
}

cmd_with_dev_db() {
  local port
  [ "${1:-}" = "--" ] && shift
  [ $# -gt 0 ] || die "usage: $0 with-dev-db -- CMD..."
  if [ -n "${MASTOFEED_DATABASE_URL:-}" ]; then
    # An explicit choice of the owner, for example another database. A
    # DATABASE_URL in .env.local never selects a database by itself.
    say "using MASTOFEED_DATABASE_URL, not the dev server of this worktree"
    DATABASE_URL="$MASTOFEED_DATABASE_URL" exec "$@"
  fi
  dev_ensure
  say "using the dev server $DEV_NAME on 127.0.0.1:$SERVER_PORT, database $DEV_DATABASE"
  DATABASE_URL="$(dev_url "$SERVER_PORT")" exec "$@"
}

cmd_psql() {
  local terminal=(--interactive)
  # A TTY only for a terminal, so `db:psql -- -c 'SQL'` also works in scripts.
  [ -t 0 ] && [ -t 1 ] && terminal=(--interactive --tty)
  dev_ensure
  exec container exec "${terminal[@]}" "$DEV_NAME" \
    psql --username=postgres --dbname="$DEV_DATABASE" "$@"
}

cmd_info() {
  local line found=0
  require_container
  echo "worktree:   $ROOT"
  echo "worktree id: $WORKTREE_ID"
  echo "image:      $IMAGE"
  line="$(server_line "$DEV_NAME")"
  if [ -n "$line" ]; then
    echo "dev server: $DEV_NAME ($(cut -f2 <<<"$line"))"
    echo "  volume:   $DEV_VOLUME"
    echo "  port:     127.0.0.1:$(cut -f6 <<<"$line")"
    echo "  database: $DEV_DATABASE (user postgres, password in .tmp/db/dev-password)"
    if [ "$(cut -f2 <<<"$line")" = "running" ]; then
      echo "  version:  $(psql_in "$DEV_NAME" postgres --command='SHOW server_version')"
      echo "  ssl:      $(psql_in "$DEV_NAME" postgres --command='SHOW ssl')"
    fi
  else
    echo "dev server: none (mise run db:start creates it)"
  fi
  while IFS=$'\t' read -r name state role worktree owner port run path; do
    [ "$role" = "test" ] && [ "$worktree" = "$WORKTREE_ID" ] || continue
    found=1
    echo "test server: $name ($state) on 127.0.0.1:$port, database $TEST_DATABASE"
  done < <(list_servers)
  [ $found -eq 1 ] || echo "test servers: none running for this worktree"
}

# --- test servers ----------------------------------------------------------

SLOT=""
TEST_NAME=""
TEST_URL=""
CHILD=""

acquire_slot() {
  local i waited=0 me
  me="$(owner_of $$)"
  mkdir -p "$COMMON/slots"
  while :; do
    for i in $(seq 1 "$MAX_TEST"); do
      if mkdir "$COMMON/slots/$i" 2>/dev/null; then
        echo "$me" >"$COMMON/slots/$i/owner"
        SLOT="$COMMON/slots/$i"
        return 0
      fi
      if [ -f "$COMMON/slots/$i/owner" ] && ! owner_alive "$(cat "$COMMON/slots/$i/owner" 2>/dev/null)"; then
        rm -rf "$COMMON/slots/$i"
      fi
    done
    [ $waited -eq 0 ] && say "waiting for a test server slot (MASTOFEED_DB_MAX_TEST=$MAX_TEST)"
    waited=$((waited + 1))
    [ $waited -gt $WAIT_LIMIT ] && die "timeout while waiting for a test server slot"
    sleep 0.5
  done
}

# Remove test and rehearsal servers whose owner process is gone, in every
# worktree of this Mac. A kept server has a marker and stays.
reap_abandoned() {
  local name state role worktree owner port run path
  while IFS=$'\t' read -r name state role worktree owner port run path; do
    case "$role" in test | rehearsal) ;; *) continue ;; esac
    [ "$owner" != "-" ] && ! owner_alive "$owner" || continue
    [ -e "$COMMON/kept/$name" ] && continue
    say "removing abandoned $role server $name"
    remove_server "$name"
    container volume delete "$name" >/dev/null 2>&1 || true
  done < <(list_servers)
  # A rehearsal volume can outlive its server, for example after SIGKILL
  # between two servers of the rehearsal.
  while IFS=$'\t' read -r name role worktree owner path; do
    [ "$role" = "rehearsal" ] && [ "$owner" != "-" ] && ! owner_alive "$owner" || continue
    [ -e "$COMMON/kept/$name" ] && continue
    [ -n "$(server_line "$name")" ] && continue
    say "removing abandoned rehearsal volume $name"
    container volume delete "$name" >/dev/null 2>&1 || true
  done < <(list_volumes)
}

test_cleanup() {
  local status=$?
  trap - EXIT INT TERM HUP
  # A signal ends the run: stop the command and all its descendants (its
  # own process group) before its server goes away.
  if [ -n "$CHILD" ] && kill -0 "$CHILD" 2>/dev/null; then
    kill -TERM -- "-$CHILD" 2>/dev/null || kill -TERM "$CHILD" 2>/dev/null || true
    wait "$CHILD" 2>/dev/null || true
  fi
  if [ -n "$TEST_NAME" ]; then
    local keep="${MASTOFEED_DB_KEEP:-}"
    if [ "$keep" = "always" ] || { [ "$keep" = "failed" ] && [ $status -ne 0 ]; }; then
      mkdir -p "$COMMON/kept" "$STATE/logs"
      echo "$ROOT" >"$COMMON/kept/$TEST_NAME"
      container logs "$TEST_NAME" >"$STATE/logs/$TEST_NAME.log" 2>&1 || true
      say "kept $TEST_NAME: DATABASE_URL=$TEST_URL"
      say "server log: .tmp/db/logs/$TEST_NAME.log; remove with: mise run db:gc"
    else
      remove_server "$TEST_NAME"
    fi
  fi
  [ -n "$SLOT" ] && unlock "$SLOT"
  exit $status
}

cmd_with_test_db() {
  local share=0 port password status
  if [ "${1:-}" = "--share" ]; then share=1; shift; fi
  [ "${1:-}" = "--" ] && shift
  [ $# -gt 0 ] || die "usage: $0 with-test-db [--share] -- CMD..."
  # A nested call reuses the server of the enclosing run only when that run
  # asked for it with --share (verify does).
  if [ "${MASTOFEED_DB_SHARE:-}" = "1" ] && [ -n "${MASTOFEED_TEST_DATABASE_URL:-}" ]; then
    DATABASE_URL="$MASTOFEED_TEST_DATABASE_URL" exec "$@"
  fi
  require_container
  mkdir -p "$STATE"
  reap_abandoned
  acquire_slot
  TEST_NAME="mastofeed-test-$WORKTREE_ID-$(openssl rand -hex 4)"
  trap test_cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  password="$(openssl rand -hex 16)"
  # Speed settings for disposable data only. The dev server never gets them.
  run_server "$TEST_NAME" \
    --tmpfs /var/lib/postgresql \
    --label mastofeed.role=test --label "mastofeed.worktree=$WORKTREE_ID" \
    --label "mastofeed.owner=$(owner_of $$)" --label "mastofeed.run=$TEST_NAME" \
    --env "POSTGRES_PASSWORD=$password" --env "POSTGRES_DB=$TEST_DATABASE" -- \
    -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=200
  port="$SERVER_PORT"
  wait_ready "$TEST_NAME" "$port"
  TEST_URL="$(url_for "$password" "$port" "$TEST_DATABASE")"
  # --no-dotenv: never read a .env file.
  DATABASE_URL="$TEST_URL" sqlx migrate run --no-dotenv --source "$ROOT/migrations" >/dev/null
  say "test server ready: $TEST_NAME on 127.0.0.1:$port"
  status=0
  # The command runs in the background and the script waits for it, so a
  # signal to this script runs the cleanup at once. Bash runs a trap only
  # after a foreground command ends. Job control (set -m) gives the command
  # its own process group, so the cleanup can stop cargo and the test
  # binaries too, and the command does not ignore SIGINT.
  set -m
  if [ $share -eq 1 ]; then
    MASTOFEED_DB_SHARE=1 MASTOFEED_TEST_DATABASE_URL="$TEST_URL" \
      MASTOFEED_DB_CONTAINER="$TEST_NAME" DATABASE_URL="$TEST_URL" "$@" <&0 &
  else
    MASTOFEED_DB_SHARE="" MASTOFEED_TEST_DATABASE_URL="" \
      MASTOFEED_DB_CONTAINER="$TEST_NAME" DATABASE_URL="$TEST_URL" "$@" <&0 &
  fi
  CHILD=$!
  set +m
  wait "$CHILD" || status=$?
  CHILD=""
  exit $status
}

cmd_gc() {
  local name state role worktree owner port run path remove_dev=0
  [ "${1:-}" = "--remove-orphaned-dev" ] && remove_dev=1
  require_container
  reap_abandoned
  # Dev servers whose worktree no longer exists. Stop them; remove them and
  # their data only on request.
  while IFS=$'\t' read -r name state role worktree owner port run path; do
    [ "$role" = "dev" ] && [ "$path" != "-" ] && [ ! -d "$path" ] || continue
    if [ $remove_dev -eq 1 ]; then
      say "removing orphaned dev server $name and volume $name (worktree $path is gone)"
      remove_server "$name"
      container volume delete "$name" >/dev/null 2>&1 || true
    elif [ "$state" = "running" ]; then
      say "stopping orphaned dev server $name (worktree $path is gone)"
      container stop "$name" >/dev/null 2>&1 || true
    fi
  done < <(list_servers)
  while IFS=$'\t' read -r name role worktree owner path; do
    [ "$role" = "dev" ] && [ "$path" != "-" ] && [ ! -d "$path" ] || continue
    if [ $remove_dev -eq 1 ]; then
      [ -n "$(server_line "$name")" ] && continue
      say "removing orphaned dev volume $name (worktree $path is gone)"
      container volume delete "$name" >/dev/null 2>&1 || true
    else
      say "orphaned dev volume $name (worktree $path is gone);" \
        "remove it and its data with: mise run db:gc -- --remove-orphaned-dev"
    fi
  done < <(list_volumes)
  while IFS=$'\t' read -r name state role worktree owner port run path; do
    case "$role" in test | rehearsal) ;; *) continue ;; esac
    [ "$worktree" = "$WORKTREE_ID" ] && [ -e "$COMMON/kept/$name" ] || continue
    say "removing kept $role server $name"
    remove_server "$name"
    container volume delete "$name" >/dev/null 2>&1 || true
    rm -f "$COMMON/kept/$name"
  done < <(list_servers)
  while IFS=$'\t' read -r name role worktree owner path; do
    [ "$role" = "rehearsal" ] && [ "$worktree" = "$WORKTREE_ID" ] && [ -e "$COMMON/kept/$name" ] || continue
    [ -n "$(server_line "$name")" ] && continue
    say "removing kept rehearsal volume $name"
    container volume delete "$name" >/dev/null 2>&1 || true
    rm -f "$COMMON/kept/$name"
  done < <(list_volumes)
  # Markers of servers and volumes that no longer exist.
  for marker in "$COMMON"/kept/*; do
    [ -e "$marker" ] || continue
    name="$(basename "$marker")"
    [ -n "$(server_line "$name")" ] || container volume inspect "$name" >/dev/null 2>&1 ||
      rm -f "$marker"
  done
}

# `cargo sqlx prepare` of this worktree, one at a time: prepare rewrites
# .sqlx, and both modes write target/sqlx-tmp, and check mode clears and
# writes target/sqlx-prepare-check. Runs inside with-test-db, which sets
# DATABASE_URL.
cmd_sqlx() { # $1 = prepare | check
  local check=()
  case "${1:-}" in
    prepare) ;;
    check) check=(--check) ;;
    *) die "usage: $0 sqlx prepare|check" ;;
  esac
  [ -n "${MASTOFEED_DB_CONTAINER:-}" ] || die "run sqlx through with-test-db"
  mkdir -p "$STATE"
  lock "$STATE/sqlx.lock" "the SQLx metadata lock of this worktree"
  trap 'unlock "$STATE/sqlx.lock"' EXIT
  cargo sqlx prepare --no-dotenv --workspace "${check[@]}" -- --all-targets --locked
}

cmd_setup() {
  local version
  command -v container >/dev/null ||
    die "the Apple container tool is missing; install it from https://github.com/apple/container/releases"
  version="$(container --version | sed -E 's/.*version ([0-9.]+).*/\1/')"
  case "$version" in
    0.*) die "container $version is too old; version 1.0.0 or later is required" ;;
  esac
  say "container $version"
  container system status >/dev/null 2>&1 ||
    die "the container system service is not running; run: container system start, then run setup again"
  container image pull --platform linux/arm64 --progress none "$IMAGE" >/dev/null
  say "image ready: $IMAGE"
}

# scripts/pg-upgrade-rehearsal.sh sources this file for its functions.
[ "${BASH_SOURCE[0]}" = "$0" ] || return 0

case "${1:-}" in
  setup) cmd_setup ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  info) cmd_info ;;
  psql) shift; cmd_psql "$@" ;;
  with-dev-db) shift; cmd_with_dev_db "$@" ;;
  with-test-db) shift; cmd_with_test_db "$@" ;;
  gc) shift; cmd_gc "$@" ;;
  sqlx) shift; cmd_sqlx "$@" ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d' >&2; exit 2 ;;
esac
