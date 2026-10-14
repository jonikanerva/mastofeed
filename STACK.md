# STACK.md — Mastofeed: Rust / Axum service with PostgreSQL

> One Rust service polls the owner's Mastodon home timeline into PostgreSQL
> and serves it as a JSON Feed. Axum, Tokio, and Tower provide HTTP and
> concurrency. SQLx gives checked SQL. Jiff represents time. There is no
> browser application: the landing page is one static HTML file.

This file adapts the agent-setup profile `stacks/STACK-RUST.md` (selection of
13 October 2026) to this repository. It is used with `DOCTRINE.md` (P1–P9)
and `CLAUDE.md`. MUST, MUST NOT, SHOULD, and MAY have the meanings in the
doctrine. A rule of the profile that this file does not adopt is listed in
§0 "Not applicable" with its reason, or in §13 as an exception.

---

## 0. Project shape

- **Project risk rationale:** one user (the owner) reads the feed in a feed
  reader. A failure delays or breaks the owner's reading; nobody else depends
  on the service. The data is a copy of the owner's home timeline: other
  people's public and unlisted statuses. The highest-risk asset is the
  Mastodon access token, which gives read access to the owner's account. A
  leak of the token or the status data is the worst outcome. A broken feed is
  recoverable by a rollback in minutes. Feed readers key their read state by
  item id, so a change of item ids or links is visible to the owner and hard
  to undo.
- **Change risk:** assess each change against that rationale. Changes to the
  feed output, the stored `raw` format, the change guard, migrations, the
  token handling, or the release configuration are material.
- **Shape:** one Rust service in a two-crate workspace. No SPA, no API client.
- **Critical execution paths:** (1) poll: timeline request, key conversion,
  normalization, upsert; (2) feed: query, render, conditional response.
  Blocking work must not stall async I/O.
- **Reason for Rust:** owner decision (2026-10-14) to align with the owner's
  other Rust services on Railway (commute, psstore). The profile notes that a
  small integration service may cost less as one TypeScript system; the owner
  accepted that trade-off.

### Not applicable (with reasons)

| Profile part | Reason |
| --- | --- |
| Frontend: TypeScript 7, React, Vite, TanStack, pnpm, Oxlint, Vitest (§1, §2, §2.4) | No browser application. `public/index.html` is static and unchanged. |
| OpenAPI, utoipa, Hey API, Zod, `contract:check` (§2.1) | No application API and no generated client. The public contract is JSON Feed version 1, checked by parity tests against the TypeScript output (§14). |
| Playwright and axe journeys (§11) | No interactive UI. HTTP tests check the static files and headers. |
| Authentication and sessions (§2.5) | No accounts. `/feed.json` is public by design (`VISION.md`). |
| Node.js and Prettier | The repository has no JavaScript. Markdown and JSON files have no formatting check (§14 gap). |
| Tailwind, uuid, rust_decimal, proptest | No current need. Request ids are a process counter (§8). |

### Repository layout and boundaries

```text
crates/
  domain/src/         # status normalization, JSON Feed rendering, HTML escaping, JS value rules
  server/src/         # config, Mastodon adapter, store, poller, HTTP, logging, lifecycle
  server/tests/       # integration tests and fixtures (golden files of the TypeScript service)
  server/sqlx.toml    # SQLx macro type overrides (TIMESTAMPTZ -> jiff_sqlx::Timestamp)
migrations/           # SQLx migrations
.sqlx/                # committed SQLx query metadata
public/               # static landing page and favicon
scripts/              # test-db.sh (isolated PostgreSQL), parity.sh (owner-run live check)
docs/adr/             # decisions
Cargo.toml  Cargo.lock  rust-toolchain.toml  clippy.toml  deny.toml  mise.toml
railpack.json  .gitleaks.toml
STACK.md  VISION.md  DOCTRINE.md  CLAUDE.md
```

- `mastofeed-domain` depends only on `jiff`, `serde`, and `serde_json`. It
  MUST NOT depend on Axum, Tokio, SQLx, reqwest, or the server crate. It reads
  no clock, environment, or file; the server passes `now` and the settings.
- `mastofeed-server` owns all effects: configuration, the Mastodon adapter
  (`mastodon.rs`, `mastodon/keys.rs`), the store (`store.rs`), the poller
  (`poller.rs`), HTTP (`http.rs`), logging (`observability.rs`), and the
  process lifecycle (`lifecycle.rs`, `main.rs`).
- Cargo enforces the crate dependencies. Purity inside the domain crate (no
  `std::env`, `std::fs`, `Timestamp::now`) is a review check; there is no
  Clippy disallowed-method rule yet.

---

## 1. Language & Runtime

- **Language:** Rust 1.99.0, edition 2024, pinned in `rust-toolchain.toml`
  with rustfmt and Clippy. `rust-version = "1.99"` in the workspace; the
  server crate repeats it (§15, Railpack).
- **Dependency MSRVs:** SQLx 0.9.0 and jiff-sqlx 0.2.0 declare 1.94; the
  others are lower or undeclared and build with 1.99.0.
- **Runtime:** a native Linux binary in the Railpack runtime image. A local
  macOS build does not validate the Linux artifact.
- **Package manager:** Cargo with workspace dependency inheritance, committed
  `Cargo.lock`, `--locked` on every task.
- **Strictness:** the workspace lints of the profile (`unsafe_code = "forbid"`,
  Clippy pedantic, and the deny list) plus `missing_debug_implementations`.
  `clippy.toml` allows unwrap, expect, panic, and print in tests. The
  integration test binary has one `#![expect(clippy::unwrap_used)]` with a
  reason, for its helper functions.

### Provisioning

Install mise and rustup. Then run in the repository root:

```bash
mise trust && mise run setup
```

`rust-toolchain.toml` owns the Rust version. `mise.toml` owns the other
tools: PostgreSQL 17.11 (conda package), sqlx-cli 0.9.0 (built from source
with the `postgres` feature), cargo-deny 0.20.2, cargo-nextest 0.9.146,
cargo-shear 1.14.0, and Gitleaks 8.30.1.

---

## 2. Frameworks

| Concern | Technology | Role |
| --- | --- | --- |
| HTTP | Axum 0.8, tower-http 0.6 | Routes, static files (`ServeDir`), security headers, panic containment |
| Concurrency | Tokio 1 (multi-thread, 2 workers) | Async I/O, the poller task, signals |
| Errors | `Result`, enums, thiserror | Fixed-text error kinds per adapter |
| Wire representation | serde, serde_json (`preserve_order`) | Mastodon JSON, the feed document |
| Time | Jiff 0.2, jiff-sqlx 0.2 | `Timestamp` everywhere; cron times in UTC |
| Schedule | croner 4 (`jiff` feature) | `CRON_SCHEDULE` |
| Persistence | PostgreSQL 17, SQLx 0.9 | Checked query macros, migrations, transactions |
| Outbound HTTP | reqwest 0.13 (rustls, gzip) | The Mastodon timeline request |
| Logging | tracing, tracing-subscriber | One line per event with a field allowlist |
| Tests | nextest, `#[sqlx::test]`, Tokio test utilities | Unit, integration, parity, lifecycle |

### 2.1 Public contract: JSON Feed parity

`/feed.json` MUST stay equal to the output of the TypeScript service
(commit `d17168d`, `feed` 5.1.0 `json1()`), because feed readers depend on
item ids, links, and HTML. The rules are in `crates/domain/src/feed.rs`:
JSON Feed version 1, four-space indentation, fixed key order, keys omitted
by JavaScript truthiness, dates with exactly three fraction digits, items
oldest first, `escapeHtml` of `& < > "` only, and links of the form
`MASTODON_BASE_URL/@acct/id`. `crates/server/tests/fixtures/golden` holds the
TypeScript output for a synthetic timeline; `parity.rs` compares the parsed
JSON (gate) and the bytes. A change of the output is a product change
(`VISION.md`).

### 2.2 Errors and failure containment

- Public failures use RFC 9457 `application/problem+json` with `type`
  (`about:blank`), `title`, `status`, and a stable `code`: `not_found` (404,
  also for an unsupported method), `unavailable` (503, the feed query failed
  or passed its 20 s deadline), `internal_error` (500, a panic or a render
  failure). Bodies contain no SQL, URL, or error text.
- `CatchPanicLayer` turns a handler panic into the 500 problem. The poller
  catches a panic of one sync and continues with the next schedule time.
- A failed sync writes nothing: the upsert runs in one transaction.
- Adapters map every failure to a fixed kind (`MastodonError`, `StoreError`)
  with a fixed `code()` for logs. Transport error text never leaves the
  adapter, because it can contain the request URL.
- An invalid timeline entry (no id, no url, invalid time) is left out and
  counted as `rejected`; the other entries are stored. This is the partial
  success of the TypeScript service.

### 2.3 Outbound source: Mastodon

`MastodonClient` (`crates/server/src/mastodon.rs`):

- `GET {MASTODON_BASE_URL}/api/v1/timelines/home?limit=40[&since_id=…]`
  with `Authorization: Bearer` (a sensitive header value), `Accept:
  application/json`, and `User-Agent: mastofeed/<version>`.
- Redirects are off, so the token never goes to another host.
- Connect timeout 10 s. The request timeout of 30 s covers the body. The
  body limit is 8 MiB after decompression. One sync has a 120 s deadline.
- Any 2xx status is a success, like masto.js. The body MUST be a JSON array.
  `serde_json::Value` keeps the last of repeated keys, like `JSON.parse`.
- `mastodon/keys.rs` converts every object key to camelCase with the rules
  of `change-case` 5, except keys with `:` or a leading `_` (docs/adr/0001).
- No retry inside a sync. The next scheduled sync is the retry.

---

## 3. Build & verify commands

| Variable | Command |
| --- | --- |
| `$FORMAT_CMD` | `mise run format` |
| `$LINT_CMD` | `mise run lint` |
| `$BUILD_CMD` | `mise run build` |
| `$TEST_CMD` | `mise run test` |
| `$VERIFY_CMD` | `mise run verify` |

- **format:** `cargo fmt --all`. `format-check` runs it with `--check`.
- **lint:** `cargo clippy --workspace --all-targets --locked -- -D warnings`
  with `SQLX_OFFLINE=true`.
- **build:** `cargo build --release --locked --package mastofeed-server` with
  `SQLX_OFFLINE=true`.
- **test:** starts the isolated database, then `cargo nextest run --workspace
  --locked` and `cargo test --doc --workspace --locked`.
- **verify:** format-check, lint, `sqlx:check`, `scan:secrets`, `scan:deps`,
  build, and test, in this order. It stops at the first failed step.
- **Other tasks:** `db:start` / `db:stop` (isolated PostgreSQL),
  `sqlx:prepare` (regenerate `.sqlx` after a query or migration change),
  `dev` (run with `.env` and text logs), `migrate` (migrate the database of
  `.env`), and `parity` (owner-run live check, §14).

**Test database isolation.** `scripts/test-db.sh` runs the pinned PostgreSQL
17.11 on `127.0.0.1:55432` with trust authentication, data in `.tmp/postgres`,
and no Unix socket. The `test`, `sqlx:prepare`, and `sqlx:check` tasks set
`DATABASE_URL` to that server themselves, so a `DATABASE_URL` in the shell
or in `.env` cannot reach them. `#[sqlx::test]` creates and drops one
database per test on that server. The `dev`, `migrate`, and `parity` tasks
read `.env`; `parity` replaces `DATABASE_URL` with the isolated server.
The port is fixed, so only one checkout can run its test database at a
time. Run `mise run db:stop` in the other checkout first.

**Sandboxes.** PostgreSQL needs System V shared memory, and the tests bind
`127.0.0.1` ports. A host sandbox that blocks either makes `db:start` or the
tests fail; that is a missing environment, not a pass.

---

## 4. Performance budgets

| Budget | Adopted value and conditions | Evidence |
| --- | --- | --- |
| Memory | Container limit 250 MB (existing Railway setting). No tighter budget is adopted. | Local macOS arm64 release build: 9.6 MiB RSS idle, 10.2 MiB after 200 feed requests (2026-10-14). Linux RSS on Railway: measure after release (§15). |
| Startup | Ready (first `/health` 200) within the Railway healthcheck timeout of 60 s, including migrations. | Local: 53 ms to ready, excluding migrations. |
| Feed latency | No budget. The feed reader polls a few times per hour. | Local, 6 items: p50 0.46 ms, p99 1.41 ms. A 200-item feed on Railway: measure after release. |
| Image size | No budget. | Release binary 6.9 MB (macOS arm64). |

These are measurements, not gates. Do not raise a budget to fit a result.

---

## 5. Persistence shape

- **Storage:** Railway PostgreSQL 17, one table `mastodon_statuses` (primary
  key `id` text; index on `created_at`). The schema is the schema of the two
  Drizzle migrations of the TypeScript service.
- **`raw` column:** the unwrapped status object with masto.js camelCase keys
  (docs/adr/0001). The feed reads `account.avatar`, `account.avatarStatic`,
  and `mediaAttachments` from it. Interaction counts in `raw` are a snapshot
  of the last real change and go stale on purpose.
- **Change guard:** the upsert rewrites a row only when one of the typed
  columns (except `created_at`) or `raw -> 'mediaAttachments'`, `raw ->
  'account' -> 'avatar'`, or `raw -> 'account' -> 'avatarStatic'` differs
  (`IS DISTINCT FROM`, jsonb equality). The reasons are in `store.rs`.
- **Queries:** SQLx `query!` macros with committed `.sqlx` metadata.
  `sqlx.toml` maps `TIMESTAMPTZ` to `jiff_sqlx::Timestamp`.
- **Atomicity:** one transaction per sync. Competing writers (the old and the
  new deployment during a rollover) are safe: both write the same values,
  and the primary key resolves the race.
- **Migrations:** numbered SQLx files in `migrations/`. `0001_baseline.sql`
  is idempotent and changes nothing on the production schema. SQLx records
  it in `_sqlx_migrations`; the Drizzle records in the `drizzle` schema stay.
  The migration runs as a separate step before the server starts (§15).
  New migrations MUST stay compatible with the running release (expand and
  contract) and MUST NOT break the TypeScript revert path until the owner
  ends that path (docs/adr/0002).
- **Connection:** no TLS. The service reaches PostgreSQL over the Railway
  private network, as the TypeScript service did (`sslmode=prefer` without a
  TLS backend). `sslmode=require` in `DATABASE_URL` would fail at start.
- **Limits:** pool of 4 connections, acquire timeout 5 s, `statement_timeout`
  15 s.
- **Recovery:** Railway PostgreSQL backups are the data recovery path. The
  service can rebuild recent rows from the timeline, but not older ones.

---

## 6. Approved dependencies

Pinned exact versions are in `Cargo.toml`. Each line has a reason. The lead
approved the set on 2026-10-14 in the Rust port PR.

| Dependency | Version | Why it earns its place |
| --- | --- | --- |
| axum | 0.8.9 | HTTP routing and extraction (profile baseline) |
| tokio | 1.53.2 | Async runtime, timers, signals |
| tower-http | 0.6.11 | `ServeDir`, `CatchPanicLayer`, `SetResponseHeaderLayer`. 0.6, not the profile's 0.7: reqwest already depends on 0.6.11, so the graph has one copy. |
| tower (dev) | 0.5.3 | `ServiceExt::oneshot` in router tests |
| serde / serde_json | 1.0.229 / 1.0.151 | Wire types. `preserve_order` keeps Mastodon key order, so key collisions resolve like masto.js. |
| thiserror | 2.0.21 | Error enums with fixed text |
| jiff / jiff-sqlx | 0.2.38 / 0.2.0 | Time values and their SQLx encoding |
| croner | 4.0.1 | Cron parsing with Jiff types; accepts the five-field `*/5 * * * *` and an optional seconds field |
| sqlx | 0.9.0 | PostgreSQL, checked macros, migrations, `#[sqlx::test]`. No TLS feature (§5). |
| reqwest | 0.13.5 | Mastodon request; rustls, gzip |
| futures-util | 0.3.34 | `catch_unwind` for the poller; already in the graph through Axum |
| tracing / tracing-subscriber | 0.1.44 / 0.3.23 | Structured logs; the formatter is first-party (§8) |

Removed with the TypeScript service: express, helmet, masto, drizzle-orm,
drizzle-kit, pg, node-cron, feed, zod, dotenv, tsx, typescript, prettier.

---

## 7. Stack-specific reject-list additions

The profile list applies. In addition:

- A change of the feed output, the item ids, or the item links without a
  matching golden file change and an owner decision.
- A change of the `raw` key format or the change guard columns without a
  data-compatibility review (docs/adr/0001).
- A migration that the TypeScript revert path cannot run on (docs/adr/0002).
- A log field that is not in `ALLOWED_FIELDS`, added only to see a value.
- A `DATABASE_URL` other than the isolated server in a test task.

---

## 8. Logging & privacy

- `crates/server/src/observability.rs` writes one JSON object per line to
  standard output (`LOG_FORMAT=json`, the default) or readable text
  (`LOG_FORMAT=text`). Railway keeps the log.
- Only the fields in `ALLOWED_FIELDS` keep their values: `message`,
  `request_id`, `method`, `route`, `status`, `elapsed_ms`, `code`, `stage`,
  `bind`, `received`, `records`, `rejected`, `written`, `version`, and
  `location`. Other fields are dropped, and only their names appear in
  `dropped_fields`. Debug formatting runs only for allowed fields.
- Events of this service are logged at INFO and above; dependencies at WARN.
- One line per request: method, route template (`static` for files),
  status, and time. A request path is not logged.
- The request id is a process-local counter, so its creation cannot fail.
  The poller has no request id.
- The panic hook logs only the source location.
- `Config` debug output redacts `DATABASE_URL` and `MASTODON_ACCESS_TOKEN`.
  A configuration error names the setting, never the value.
- No crash reporter, analytics, or telemetry exporter.

---

## 9. Background & lifecycle

- **Poller:** one Tokio task owned by `lifecycle::serve`. It runs a sync at
  start and then at each `CRON_SCHEDULE` time in UTC. A sync that runs past
  a schedule time skips it, so syncs never overlap. Each sync has a 120 s
  deadline. A failure or panic is logged; the next schedule time runs.
- **Shutdown:** SIGTERM or SIGINT stops accepting connections and signals the
  poller. Active requests get 10 s. The poller gets 5 s, then it is aborted;
  an interrupted sync rolls back. The pool gets 5 s to close. The runtime
  gets 5 s for blocking tasks. Railway waits 30 s before SIGKILL.
- **Signals:** if a signal handler cannot be installed, the service logs it
  and still stops on the other signal.
- **Durable work:** none. A restart loses nothing: the next sync fetches the
  timeline again.

---

## 10. Time & timezones

- Instants are `jiff::Timestamp`; PostgreSQL `timestamptz`.
- Timeline times are truncated to milliseconds, because the TypeScript
  service stored JavaScript `Date` values. The feed writes `toISOString`
  form: UTC, three fraction digits, `Z`.
- A missing `createdAt` becomes the poll time (the TypeScript rule). An
  invalid `createdAt` or `editedAt` rejects the entry; the TypeScript service
  failed the whole sync in that case.
- The cron schedule is evaluated in UTC. The time zone database is not used.
- Tests pass fixed times (`FixedClock`, explicit `now`).

---

## 11. Design guidelines & UX thresholds

The only page is `public/index.html`, unchanged from the TypeScript service.
It has native HTML semantics and one link. The feed reader is the real user
interface; its contract is §2.1. HTTP tests check the page, the favicon,
their media types, and the Helmet 8 security headers on every response.

---

## 12. Best practices source

- [Axum 0.8](https://docs.rs/axum/0.8.9/), [tower-http 0.6](https://docs.rs/tower-http/0.6.11/), [Tokio shutdown](https://tokio.rs/tokio/topics/shutdown)
- [SQLx 0.9](https://docs.rs/sqlx/0.9.0/) (including `sqlx.toml` type overrides), [jiff-sqlx](https://docs.rs/jiff-sqlx/0.2.0/), [PostgreSQL 17](https://www.postgresql.org/docs/17/)
- [Jiff](https://docs.rs/jiff/0.2.38/), [croner 4](https://docs.rs/croner/4.0.1/), [reqwest 0.13](https://docs.rs/reqwest/0.13.5/)
- [JSON Feed 1](https://www.jsonfeed.org/version/1/), [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457), [RFC 9110 §13.1.2](https://www.rfc-editor.org/rfc/rfc9110#section-13.1.2)
- [Mastodon timelines API](https://docs.joinmastodon.org/methods/timelines/#home)
- [Railpack Rust](https://railpack.com/languages/rust/), [Railpack config file](https://railpack.com/config/file/), Railway [healthchecks](https://docs.railway.com/deployments/healthchecks), [deployment teardown](https://docs.railway.com/deployments/deployment-teardown), [deployment actions](https://docs.railway.com/deployments/deployment-actions)

---

## 13. Scoped exceptions

| Rule / scope | Reason and consequences | Compensating evidence | Lead / reviewer / approval | Expiry or reassessment |
| --- | --- | --- | --- | --- |
| Profile §15 "dependency-aware readiness": `/health` is liveness only | The start command runs `migrate` first, which needs the database; a deploy with a broken `DATABASE_URL` exits before it serves, so the healthcheck fails and the old deployment stays. A database check in `/health` would make a short database outage restart a healthy service. | `lifecycle::the_migrate_command_fails_without_a_database`; §15 healthcheck setting | Lead; independent reviewer of the port PR | When the service gets a second dependency that a deploy must check |
| Profile §15 build isolation: Railpack gives every Railway service variable, including `MASTODON_ACCESS_TOKEN` and `DATABASE_URL`, to every build command and dependency build script | Railpack 0.40.1 has no per-step variable filter; `"secrets": []` limits only cache invalidation. The same applies to the owner's other Railpack services. | Committed `Cargo.lock` with `--locked`; `cargo deny` (sources, bans, advisories); a small dependency set with a reason each (§6); no new recipient of the values | Lead; independent reviewer of the port PR | When a dependency with a build script or proc macro is added or changes, or on a supply-chain advisory |
| Profile §14 format check for all files | Without Node there is no Prettier. Markdown, JSON, SQL, and HTML files have no formatting check. | Review | Owner decision 2026-10-14 (remove Node) | When a Rust-native formatter covers these files |

---

## 14. Applicability and evidence

Run the required tests and the full `mise run verify` locally on the version
to be merged against the current base. Missing or failed mandatory local
evidence blocks merge. There is no CI.

| Doctrine / applicability | Required evidence | Environment / gap |
| --- | --- | --- |
| P1, P6: each task | Criteria, failures, assumptions, check mapping in the PR | Lead records; independent review for material changes |
| P2–P4: types and dependencies | Clippy, cargo-deny, cargo-shear; dependency reasons (§6); crate boundary | Purity inside the domain crate is review-only |
| P3, P5: feed contract | `parity.rs` against the golden files; `feed.rs` unit tests | Golden files come from one synthetic timeline |
| P5: data meaning | Normalization and key conversion tests; stored `raw` and columns equal to the TypeScript rows (jsonb equality) | JavaScript writes `60.0` as `60`; jsonb treats both as equal |
| P5–P7: storage | `sqlx:check`; migration tests (empty, Drizzle schema, repeat); change guard tests (`xmin`); atomic failure test | Isolated PostgreSQL 17.11; production runs Railway's 17.x |
| P3, P7: security | Gitleaks (history, staged, unstaged tracked files); redaction tests; no-redirect test | `.env` and untracked files are not scanned |
| P7: release | Before merge: Railpack plan, target build, healthcheck setting. After release: §15 checks | A local macOS build is not the Linux artifact |
| P8–P9: material change | Independent review with the reviewer's own `mise run verify` | Separate reviewer context |

### Check configuration

| Check | Tool | Acceptance condition |
| --- | --- | --- |
| Format | rustfmt | No difference |
| Types / lint | Clippy (`-D warnings`) | No error |
| Dead dependencies | cargo-shear 1.14.0 | No finding |
| Query metadata | `cargo sqlx prepare --workspace --check` | `.sqlx` matches the queries and the migrated isolated schema |
| Secrets | Gitleaks 8.30.1, `.gitleaks.toml` (adds a Mastodon token rule) | No finding |
| Rust dependencies | cargo-deny 0.20.2, `deny.toml` | Advisories (including unmaintained and unsound), licences, bans, and sources pass |
| Behaviour | nextest 0.9.146, doc tests | All tests pass |

### Golden files

`crates/server/tests/fixtures/golden/README.md` records how the golden files
were produced from the TypeScript service. Regenerate them only from that
commit. A golden change is a feed contract change (§2.1).

### Live checks

`mise run parity` is owner-run, because it uses the owner's token from
`.env`. It polls the live timeline once with the TypeScript service of
`d17168d` and once with the Rust service, into one database on the isolated
server, and compares the two feeds item by item. It prints counts and status
ids only. Items that differ, or a difference in the feed-level fields, fail
the check. New statuses between the two polls are reported as expected.

---

## 15. Release, recovery, and maintenance

The release is one always-on Railway service (`mastofeed` in project
`calmdonut`, region `europe-west4`, 1 replica) built by Railpack from
`railpack.json`. Each merge to `main` deploys automatically.

### Build and start (in the repository)

| Item | Value |
| --- | --- |
| Builder | Railpack 0.40.1, Rust provider (`RAILPACK_VERSION=0.40.1`) |
| Toolchain | Rust 1.99.0 from `rust-toolchain.toml` (plan: `idiomatic-version-file`) |
| Build | `SQLX_OFFLINE=true cargo build --release --locked --package mastofeed-server`, then copy the binary to `bin/` |
| Image content | `bin/mastofeed` and `public/` only |
| Start | `./bin/mastofeed migrate && exec ./bin/mastofeed serve`. Railpack 0.40.1 sets the entrypoint `/bin/bash -c` (`buildkit/convert.go`), so `&&` and `exec` work; after `exec`, the service is PID 1 and receives SIGTERM. |
| Build context | Without `mise.toml` (local tools only) |

`SQLX_OFFLINE=true` is required: Railpack gives the build the service
variable `DATABASE_URL`, and the SQLx macros would otherwise try to connect
to the production database during the build. Railpack 0.40.1 cannot read
inherited package fields in the binary crate, so `crates/server/Cargo.toml`
repeats them. Check `railpack info --show-plan .` after a change to a
manifest or to `railpack.json`.

### Service settings (in Railway)

The lead applied the healthcheck, draining, and Railpack settings on
2026-10-14, before the merge, with owner approval. The TypeScript service
also serves `/health`, so the settings are safe for a revert.

| Setting | Value | Reason |
| --- | --- | --- |
| Healthcheck path | `/health` | Railway switches traffic only after a 2xx answer, so a release that fails at start leaves the old deployment active |
| Healthcheck timeout | 60 s (service setting) | Migrations and start take well under a second |
| Draining time | 30 s (service setting) | The shutdown needs at most about 25 s (§9) |
| Railpack version | `RAILPACK_VERSION=0.40.1` (service variable) | The version of the plan evidence |
| Restart policy | Railway default | |
| Memory limit | 250 MB | Existing setting |
| Custom domain | `mastodon.calmdonut.com` (port 8080) | Existing setting; Railway sets `PORT` |

### Runtime variables (names only)

- Required: `DATABASE_URL`, `MASTODON_BASE_URL`, `MASTODON_ACCESS_TOKEN`,
  `HOME_PAGE_URL`.
- Optional: `CRON_SCHEDULE` (default `*/5 * * * *`), `FEED_LIMIT` (1–1000,
  default 200), `FEED_TITLE`, `FEED_DESCRIPTION`, `LOG_FORMAT`, `PUBLIC_DIR`.
- Platform: `PORT` and `RAILWAY_GIT_COMMIT_SHA` (set by Railway) and
  `RAILPACK_VERSION`.
- An invalid value stops the start with exit code 2. The message names the
  setting, never its value.

### Release procedure

1. Before merge: record the SHA of the active deployment as the known-good
   version. Save the public `/feed.json` for the comparison in step 4.
2. Merge. Watch the build log: Railpack 0.40.1, Rust 1.99.0, the offline
   build, and a passing healthcheck.
3. Confirm that the active deployment shows the merged commit and that
   `/health` returns that `version`.
4. Run the post-release checks.

### Post-release checks

- `GET https://mastodon.calmdonut.com/health` gives 200 with the merged SHA.
- `GET /feed.json` gives 200 with `application/feed+json; charset=utf-8`
  and an `ETag`; a repeat with `If-None-Match` gives 304.
- The items of the saved feed and the new feed are equal by id, except new
  statuses.
- The log shows `migrations are up to date`, `service started`, and
  `timeline sync finished` with `written` near 0 on the first sync. It has
  no token, URL query, or status content.
- Railway metrics: memory after one hour, compared with §4.

### Rollback and recovery

- **Immediate:** in Railway Deployments, roll back to the known-good
  deployment. Railway restores its image and variables.
- **Code:** `git revert -m 1 <merge commit>` on a branch, then merge. The
  revert removes `railpack.json`, so Railpack detects Node again.
- **Data:** a revert to the TypeScript service needs no data change. The
  schema is the same, `raw` uses the same keys, and the Drizzle records are
  intact. Evidence: the PR runs the TypeScript migrator, two TypeScript
  polls, and the TypeScript feed on a database that the Rust service wrote;
  the polls rewrote 0 rows and the feed bytes were equal (docs/adr/0002).
  The extra `_sqlx_migrations` table is harmless to the TypeScript service.
- **Database loss:** restore a Railway PostgreSQL backup. The service then
  refills recent statuses on the next sync.

### Maintenance

Review toolchain and dependency upgrades with the same evidence. After a
SQLx, jiff-sqlx, or migration change, run `mise run sqlx:prepare` and commit
`.sqlx`. After a Railpack change, compare the plan again. Keep the TypeScript
revert path until the owner ends it; then docs/adr/0002 can be superseded.
