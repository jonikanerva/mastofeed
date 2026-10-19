# STACK.md — Mastofeed: Rust / Axum service with PostgreSQL

> One Rust service polls the owner's Mastodon home timeline into PostgreSQL
> and serves it as a JSON Feed. Axum, Tokio, and Tower provide HTTP and
> concurrency. SQLx gives checked SQL. Jiff represents time. There is no
> browser application: the landing page is one static HTML file.

This file adapts the agent-setup profile `stacks/STACK-RUST.md` (selection of
15 October 2026) to this repository. It is used with `DOCTRINE.md` (P1–P9)
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
  server/build.rs     # rebuild the server crate when migrations/ changes
migrations/           # SQLx migrations
.sqlx/                # committed SQLx query metadata
public/               # static landing page and favicon
scripts/              # test-db.sh (isolated PostgreSQL), parity.sh (owner-run live check)
docs/adr/             # decisions
.claude/              # Claude Code agents and skills (copy of agent-setup, PR #8)
.agents/              # Codex skills (copy of agent-setup, PR #8)
.codex/               # Codex agents (copy of agent-setup, PR #8)
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

- **Language:** Rust 1.99.1, edition 2024, pinned in `rust-toolchain.toml`
  with rustfmt and Clippy. `rust-version = "1.99"` in the workspace; the
  server crate repeats it (§15, Railpack).
- **Dependency MSRVs:** SQLx 0.9.0 and jiff-sqlx 0.2.0 declare 1.94; the
  others are lower or undeclared and build with 1.99.1.
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
tools: PostgreSQL 18.6 (conda package), sqlx-cli 0.9.0 (built from source
with the `postgres` feature), cargo-deny 0.20.2, cargo-nextest 0.9.148,
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
| Persistence | PostgreSQL 18, SQLx 0.9 | Checked query macros, migrations, transactions |
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

Intentional difference (issue #9, owner decision 2026-10-19): the poller
pages the statuses that are newer than the newest stored status (§2.3). The
TypeScript service requested one page of 40 and lost the older statuses of
a larger gap. When at most 40 statuses are newer, the stored rows and the
feed stay equal to the TypeScript service. The requests differ only by one
last request with `max_id`, which gets an empty page; `parity.rs` checks
this. With more than 40, the Rust service stores more statuses, so its feed
has items that the TypeScript feed does not have. The existing items stay
equal.

### 2.2 Errors and failure containment

- Public failures use RFC 9457 `application/problem+json` with `type`
  (`about:blank`), `title`, `status`, and a stable `code`: `not_found` (404,
  also for an unsupported method), `unavailable` (503, the feed query failed
  or passed its 20 s deadline), `internal_error` (500, a panic or a render
  failure). Bodies contain no SQL, URL, or error text.
- `CatchPanicLayer` turns a handler panic into the 500 problem. The poller
  catches a panic of one sync and continues with the next schedule time.
- A failed sync writes nothing: the upsert runs in one transaction. A failed
  request on any page fails the whole sync, so the next sync requests the
  same gap again.
- A body above the 2 MiB limit fails the sync with `body_too_large`, also on
  a later page and in the check request after the page cap. Every sync then
  fails in the same way until newer statuses push the large status out of
  the requested range: about 240 statuses (the newest page, 5 pages, and the
  check request). Until then, the feed gets no new statuses.
- Adapters map every failure to a fixed kind (`MastodonError`, `StoreError`)
  with a fixed `code()` for logs. Transport error text never leaves the
  adapter, because it can contain the request URL.
- An invalid timeline entry (no id, no url, invalid time) is left out and
  counted as `rejected`; the other entries are stored. This is the partial
  success of the TypeScript service.

### 2.3 Outbound source: Mastodon

`MastodonClient` (`crates/server/src/mastodon.rs`):

- `GET {MASTODON_BASE_URL}/api/v1/timelines/home?limit=40[&since_id=…][&max_id=…]`
  with `Authorization: Bearer` (a sensitive header value), `Accept:
  application/json`, and `User-Agent: mastofeed/<version>`.
- One sync requests the newest page (no `since_id`). When a stored status
  exists, it then requests the newer statuses with `since_id` set to the
  newest stored id. After a page with entries, the next request adds
  `max_id` set to the id of the oldest entry of that page (for a boost, the
  id of the boost), like the `next` link of Mastodon. Only an empty page
  ends the requests. A short page does not, because Mastodon applies the
  limit before it removes deleted statuses.
- The page cap is 5 pages of newer statuses (`NEWER_PAGE_CAP`, 200
  statuses, the default feed length). After 5 pages, one more request of
  40 tells whether more newer statuses exist; its entries are not stored.
  If they exist, the sync stores what it received and logs a warning with
  `code` `page_cap`. The older statuses of the gap stay missing, because
  the next sync starts from the new newest stored status.
- Body budget: the response bodies of one sync stay at or below two
  maximal bodies (`BODY_BUDGET_PAGES`, 2 × 2 MiB = 4 MiB). A request
  starts only if a maximal body still fits, so all 7 requests run while
  the pages average at most about 0.3 MiB. A page of 40 statuses is
  usually below 200 KiB. With larger pages, the sync stores what it
  received and logs a warning with `code` `body_budget`. The warning can
  also appear when no status is missing.
- Memory: the body limit is 2 MiB, 10 times a usual page. The memory bound
  covers Mastodon status entities, not every JSON shape (owner decision
  2026-10-19). The measurements show it for status-like bodies only. The service reads only the owner's instance, and
  its serializers produce status objects with fixed fields. A byte limit
  cannot bound the parsed size of every JSON shape.
- The ignored tests in `tests/integration/memory.rs` measure one sync at
  the body budget: two bodies just below the limit (macOS arm64, release
  build, 2026-10-19, two runs each). The container limit is 250 MB, about
  238 MiB (§15). The Linux numbers are not measured.

  | Body shape | 2 MiB limit | 4 MiB limit |
  | --- | --- | --- |
  | Hashtag objects (status-like) | +45 MiB | +87 MiB |
  | Array of small numbers (synthetic) | +149 MiB | +299 MiB |
  | One-element arrays (synthetic) | +395 MiB | not measured |
  | One-key objects (synthetic) | +212 MiB | not measured |

- A page whose oldest entry has no string id stops the requests, with
  `code` `missing_cursor`.
- Request bounds of one sync: at most 7 requests (`MAX_SYNC_REQUESTS`).
  A sync has a 240 s deadline: 7 requests at the 30 s timeout plus 30 s
  for the database work, which usually takes well below 1 s. A
  compile-time assertion in `poller.rs` keeps the deadline above that sum.
- Redirects are off, so the token never goes to another host.
- Connect timeout 10 s. The request timeout of 30 s covers the body. The
  body limit is 2 MiB after decompression. One sync has a 240 s deadline.
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
  `dev` (run with `.env.local` and text logs), `migrate` (migrate the
  database of `.env.local`), `parity` (owner-run live check, §14), and
  `pg:rehearse <old> <new>` (PostgreSQL major upgrade rehearsal, §15).

**Dependency freshness check** (`DOCTRINE.md` P7). Run it on the current
`main` before each delivery task. Every step is read-only. The steps need
network access to crates.io, GitHub, the mise registries, and Railway. An
unavailable step is a recorded gap, not a pass.

1. `cargo update --dry-run --verbose`. It lists newer SemVer-compatible
   versions in `Cargo.lock`. The direct dependencies have exact `=` pins, so
   this step shows them as `Unchanged` with the available version.
2. Compare each entry of `[workspace.dependencies]` in `Cargo.toml` with the
   latest stable release on crates.io (`max_stable_version` of
   `https://crates.io/api/v1/crates/<name>`). This step also finds new
   SemVer-incompatible versions.
3. `mise outdated --bump --local`. It compares the pins in `mise.toml` with
   the latest releases that are older than `minimum_release_age` in
   `mise.toml`. For `conda:postgresql` it shows only the newest major
   version. That pin follows the production major version (§5), so a newer
   major version is an infrastructure decision, not a tool update. Also run
   `mise latest conda:postgresql@<production major>` and compare the result
   with the pin, so a new minor release of the production major version is
   visible. mise does not apply the release age rule to `conda:postgresql`
   (§15 Dependencies).
4. `mise latest rust`. It applies the release age rule. Compare the result
   with `rust-toolchain.toml`. `rustup check` shows the newest stable Rust
   without the age rule.
5. Compare each infrastructure version in §15 Infrastructure with its latest
   stable release: the production PostgreSQL image tag (Railway service
   `Postgres mastofeed`) with the latest PostgreSQL major version, and the
   Railpack version of the active `mastofeed` deployment with the latest
   release of `railwayapp/railpack`. The running production PostgreSQL
   minor version is not checked (§15 Infrastructure).

Record the result, the deps PR links, and each retained pin with its reason
and reassessment condition in the task PR.

**Test database isolation.** `scripts/test-db.sh` runs the pinned PostgreSQL
18.6 on `127.0.0.1:55432` with trust authentication, data in `.tmp/postgres`,
and no Unix socket. The server offers TLS with a self-signed X.509 v3
certificate, like production (§5), and also accepts connections without TLS.
When an older version of the script started the server in `.tmp/postgres`
without TLS, `start` restarts that server with TLS. When the data directory
has another major version than the pinned binaries, `start` stops that
server and creates a new cluster; the test data is disposable. The `test`,
`sqlx:prepare`, and `sqlx:check` tasks set `DATABASE_URL` to that server themselves, so a `DATABASE_URL` in the shell
or in `.env.local` cannot reach them. `#[sqlx::test]` creates and drops one
database per test on that server. The `dev`, `migrate`, and `parity` tasks
read `.env.local`; `parity` replaces `DATABASE_URL` with the isolated server.

**No `.env` file.** The local settings of the service are in `.env.local`.
The SQLx query macros parse every `.env` file in the crate directory and its
parent directories at compile time, also with `SQLX_OFFLINE=true`. An
unquoted value with spaces, such as `CRON_SCHEDULE=*/5 * * * *`, stops the
build. sqlx-cli runs with `--no-dotenv`.
The port is fixed, so only one checkout can run its test database at a
time. Run `mise run db:stop` in the other checkout first.

**Sandboxes.** PostgreSQL needs System V shared memory, and the tests bind
`127.0.0.1` ports. A host sandbox that blocks either makes `db:start` or the
tests fail; that is a missing environment, not a pass.

---

## 4. Performance budgets

| Budget | Adopted value and conditions | Evidence |
| --- | --- | --- |
| Memory | Container limit 250 MB (existing Railway setting). No tighter budget is adopted. | Local macOS arm64 release build: 9.6 MiB RSS idle, 10.2 MiB after 200 feed requests (2026-10-14). |
| Startup | Ready (first `/health` 200) within the Railway healthcheck timeout of 60 s, including migrations. | Local: 53 ms to ready, excluding migrations. |
| Feed latency | No budget. The feed reader polls a few times per hour. | Local, 6 items: p50 0.46 ms, p99 1.41 ms. |
| Image size | No budget. | Release binary 6.9 MB (macOS arm64). |

These are measurements, not gates. Do not raise a budget to fit a result.

---

## 5. Persistence shape

- **Storage:** Railway PostgreSQL 18, one table `mastodon_statuses` (primary
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
- **Server version:** production runs the Railway image
  `ghcr.io/railwayapp-templates/postgres-ssl:18` (major version 18 since the
  upgrade of 2026-10-15). The isolated test server runs 18.6. Read the image
  tag again after a Railway database upgrade and keep the test server on the
  same major version. A newer stable major version follows the `DOCTRINE.md` P7
  infrastructure rule (§15 Infrastructure).
- **Connection TLS:** SQLx has the feature `tls-rustls-aws-lc-rs`. It uses
  the aws-lc-rs provider of reqwest, so the binary has one rustls crypto
  provider (`cargo tree -i ring` prints nothing). The default
  `sslmode=prefer` uses TLS when the server offers it and does not check the
  certificate chain. A failed TLS handshake fails the connection; there is
  no fallback to plain text. `PGSSLMODE` is not set on the service. The
  production certificate is X.509 v3 (owner check through the public TCP
  proxy, 2026-10-15); rustls rejects older versions. The `migrate` command
  logs whether the connection uses TLS. The service reaches PostgreSQL over
  the Railway private network. The TypeScript service connected without TLS.
- **Limits:** pool of 4 connections, acquire timeout 5 s, `statement_timeout`
  15 s.
- **Recovery:** Railway PostgreSQL backups are the data recovery path. The
  service can rebuild recent rows from the timeline, but not older ones.
  A Railway major upgrade adds a backup before and after the upgrade (§15
  Infrastructure).

---

## 6. Approved dependencies

Pinned exact versions are in `Cargo.toml`. Each line has a reason. The lead
approved the set on 2026-10-14 in the Rust port PR.

| Dependency | Version | Why it earns its place |
| --- | --- | --- |
| axum | 0.8.9 | HTTP routing and extraction (profile baseline) |
| tokio | 1.53.3 | Async runtime, timers, signals |
| tower-http | 0.6.11 | `ServeDir`, `CatchPanicLayer`, `SetResponseHeaderLayer`. 0.6, not the profile's 0.7: reqwest already depends on 0.6.11, so the graph has one copy. |
| tower (dev) | 0.5.3 | `ServiceExt::oneshot` in router tests |
| serde / serde_json | 1.0.229 / 1.0.154 | Wire types. `preserve_order` keeps Mastodon key order, so key collisions resolve like masto.js. |
| thiserror | 2.0.21 | Error enums with fixed text |
| jiff / jiff-sqlx | 0.2.38 / 0.2.0 | Time values and their SQLx encoding |
| croner | 4.0.1 | Cron parsing with Jiff types; accepts the five-field `*/5 * * * *` and an optional seconds field |
| sqlx | 0.9.0 | PostgreSQL, checked macros, migrations, `#[sqlx::test]`. TLS feature `tls-rustls-aws-lc-rs`, which adds webpki-roots (no build script) (§5). |
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
  a schedule time skips it, so syncs never overlap. Each sync has a 240 s
  deadline for all its pages and the database work (§2.3). A failure or panic is logged; the next
  schedule time runs.
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
- [SQLx 0.9](https://docs.rs/sqlx/0.9.0/) (including `sqlx.toml` type overrides), [jiff-sqlx](https://docs.rs/jiff-sqlx/0.2.0/), [PostgreSQL 18](https://www.postgresql.org/docs/18/), [Railway major upgrade](https://docs.railway.com/databases/postgresql-major-upgrade)
- [Jiff](https://docs.rs/jiff/0.2.38/), [croner 4](https://docs.rs/croner/4.0.1/), [reqwest 0.13](https://docs.rs/reqwest/0.13.5/)
- [JSON Feed 1](https://www.jsonfeed.org/version/1/), [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457), [RFC 9110 §13.1.2](https://www.rfc-editor.org/rfc/rfc9110#section-13.1.2)
- [Mastodon timelines API](https://docs.joinmastodon.org/methods/timelines/#home)
- [Railpack Rust](https://railpack.com/languages/rust/), [Railpack config file](https://railpack.com/config/file/), Railway [healthchecks](https://docs.railway.com/deployments/healthchecks), [deployment teardown](https://docs.railway.com/deployments/deployment-teardown), [deployment actions](https://docs.railway.com/deployments/deployment-actions)

---

## 13. Scoped exceptions

| Rule / scope | Reason and consequences | Compensating evidence | Lead / reviewer / approval | Expiry or reassessment |
| --- | --- | --- | --- | --- |
| Profile §15 "dependency-aware readiness": `/health` is liveness only | The start command runs `migrate` first, which needs the database; a deploy with a broken `DATABASE_URL` exits before it serves, so the healthcheck fails and the old deployment stays. A database check in `/health` would make a short database outage restart a healthy service. | `lifecycle::the_migrate_command_fails_without_a_database`; §15 healthcheck setting | Lead; independent reviewer of the port PR | When the service gets a second dependency that a deploy must check |
| Profile §15 build isolation: Railpack gives every Railway service variable, including `MASTODON_ACCESS_TOKEN` and `DATABASE_URL`, to every build command and dependency build script | Railpack 0.40.1 and 0.41.0 have no per-step variable filter (0.41.0 is the Railway default of the builds of 2026-10-18 and 2026-10-19; 0.42.0 is not checked); `"secrets": []` limits only cache invalidation. The same applies to the owner's other Railpack services. | Committed `Cargo.lock` with `--locked`; `cargo deny` (sources, bans, advisories); a small dependency set with a reason each (§6); no new recipient of the values | Lead; independent reviewer of the port PR | When a dependency with a build script or proc macro is added or changes, or on a supply-chain advisory |
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
| P3, P5: feed contract | `parity.rs` against the golden files; `feed.rs` unit tests; `paging.rs` for a gap of more than 40 statuses (the intentional difference of §2.1) | Golden files come from one synthetic timeline with one newer status |
| P5: data meaning | Normalization and key conversion tests; stored `raw` and columns equal to the TypeScript rows (jsonb equality) | JavaScript writes `60.0` as `60`; jsonb treats both as equal |
| P5–P7: storage | `sqlx:check`; migration tests (empty, Drizzle schema, repeat); change guard tests (`xmin`); atomic failure test; TLS tests of the `migrate` command (`prefer`, `require`, `disable`) | Isolated PostgreSQL 18.6 with TLS; production runs the Railway image `postgres-ssl:18` (§5) |
| P3, P7: security | Gitleaks (history, staged, unstaged tracked files); redaction tests; no-redirect test | `.env.local` and untracked files are not scanned |
| P7: dependency freshness | The §3 freshness check result, deps PR links, and retained pins with reasons, in each task PR | mise applies the minimum release age to tools with release dates, including Rust. Crates, PostgreSQL, and Railpack need a manual date check (§15 Dependencies) |
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
| Behaviour | nextest 0.9.148, doc tests | All tests pass |

### Golden files

`crates/server/tests/fixtures/golden/README.md` records how the golden files
were produced from the TypeScript service. Regenerate them only from that
commit. A golden change is a feed contract change (§2.1).

### Live checks

`mise run parity` is owner-run, because it uses the owner's token from
`.env.local`. It polls the live timeline once with the TypeScript service of
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
| Builder | Railpack in the Railway default version, Rust provider. The version is not pinned (owner decision 2026-10-19). The builds of 2026-10-18 and 2026-10-19 used 0.41.0 (§15 Infrastructure). |
| Toolchain | Rust 1.99.1 from `rust-toolchain.toml` (plan: `idiomatic-version-file`) |
| Build | `SQLX_OFFLINE=true cargo build --release --locked --package mastofeed-server`, then copy the binary to `bin/` |
| Image content | `bin/mastofeed` and `public/` only |
| Start | `./bin/mastofeed migrate && exec ./bin/mastofeed serve`. Railpack 0.40.1 and 0.41.0 set the entrypoint `/bin/bash -c` (`buildkit/convert.go`), so `&&` and `exec` work; after `exec`, the service is PID 1 and receives SIGTERM. |
| Build context | Without `mise.toml` (local tools only) |

`SQLX_OFFLINE=true` is required: Railpack gives the build the service
variable `DATABASE_URL`, and the SQLx macros would otherwise try to connect
to the production database during the build. Railpack 0.40.1 and 0.41.0
cannot read inherited package fields in the binary crate, so
`crates/server/Cargo.toml` repeats them. Check `railpack info --show-plan .` after a change to a
manifest or to `railpack.json`.

`sqlx::migrate!` embeds the migrations at compile time. On stable Rust, a
new migration file alone does not rebuild the crate, so a cached build can
miss the migration. `crates/server/build.rs` prints
`cargo:rerun-if-changed=../../migrations`, as the documentation of
`sqlx::migrate!` recommends (issue #12). The build script reads no
environment variable and no file. Manual check after a change to the build
script:

1. `SQLX_OFFLINE=true cargo build --locked -p mastofeed-server`.
2. Add an empty file to `migrations/`, then run the same command with `-v`.
   The output shows `Dirty mastofeed-server ... the file
   crates/server/../../migrations has changed`.
3. Remove the file. The next build is `Dirty` again; a build after that is
   `Fresh`.

### Service settings (in Railway)

The lead applied the healthcheck and draining settings on 2026-10-14,
before the merge, with owner approval. The TypeScript service
also serves `/health`, so the settings are safe for a revert.

| Setting | Value | Reason |
| --- | --- | --- |
| Healthcheck path | `/health` | Railway switches traffic only after a 2xx answer, so a release that fails at start leaves the old deployment active |
| Healthcheck timeout | 60 s (service setting) | Migrations and start take well under a second |
| Draining time | 30 s (service setting) | The shutdown needs at most about 25 s (§9) |
| Railpack version | Railway default, not pinned | Owner decision 2026-10-19 (§15 Infrastructure) |
| Restart policy | Railway default | |
| Memory limit | 250 MB | Existing setting |
| Custom domain | `mastodon.calmdonut.com` (port 8080) | Existing setting; Railway sets `PORT` |

### Runtime variables (names only)

- Required: `DATABASE_URL`, `MASTODON_BASE_URL`, `MASTODON_ACCESS_TOKEN`,
  `HOME_PAGE_URL`.
- Optional: `CRON_SCHEDULE` (default `*/5 * * * *`), `FEED_LIMIT` (1–1000,
  default 200), `FEED_TITLE`, `FEED_DESCRIPTION`, `LOG_FORMAT`, `PUBLIC_DIR`.
- Platform: `PORT` and `RAILWAY_GIT_COMMIT_SHA` (set by Railway).
- An invalid value stops the start with exit code 2. The message names the
  setting, never its value.

### Release procedure

1. Before merge: record the SHA of the active deployment as the known-good
   version. Save the public `/feed.json` for the comparison in step 4.
2. Merge. Watch the build log: the Railpack version of the last plan
   evidence (§15 Infrastructure), Rust 1.99.1, the offline build, and a
   passing healthcheck. If the Railpack version is new, compare the plans
   of the old and the new version.
3. Confirm that the active deployment shows the merged commit and that
   `/health` returns that `version`.
4. Run the post-release checks.

### Post-release checks

- `GET https://mastodon.calmdonut.com/health` gives 200 with the merged SHA.
- `GET /feed.json` gives 200 with `application/feed+json; charset=utf-8`
  and an `ETag`; a repeat with `If-None-Match` gives 304.
- The items of the saved feed and the new feed are equal by id, except new
  statuses.
- The log shows `migrations are up to date`, `database connection uses
  TLS`, `service started`, and
  `timeline sync finished` with `written` near 0 on the first sync. It has
  no token, URL query, or status content.

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
  On PostgreSQL 18, `mise run pg:rehearse 17.11 18.6` repeats this check
  after `pg_upgrade`: the TypeScript migrator, poll, and feed work, and the
  feed bytes are equal to the Rust feed.
- **Database loss:** restore a Railway PostgreSQL backup. The service then
  refills recent statuses on the next sync.

### Infrastructure

| Component | Production | Test and local | How to read the version |
| --- | --- | --- | --- |
| PostgreSQL | Railway image `ghcr.io/railwayapp-templates/postgres-ssl:18` (18.6 at the upgrade of 2026-10-15, from the deploy log; the tag does not fix the minor version) | `conda:postgresql` 18.6 in `mise.toml` (`scripts/test-db.sh`) | Railway: the source image of service `Postgres mastofeed` and the start message `starting PostgreSQL <version>` in its deploy log. Local: `mise ls --local`. |
| Build platform | Railpack 0.41.0, the Railway default (deployment `8409bd40`, 2026-10-19, the merge of PR #10; build log: `railpack-v0.41.0`) | The same version through `mise x github:railwayapp/railpack@<version> -- railpack info --show-plan .` | Railway: the `railpackVersion` of the active deployment (`railway status --json`) or the line `Railpack <version>` in its build log |
| Runtime image | `ghcr.io/railwayapp/railpack-runtime:mise-2026.10.8` (Railpack 0.41.0) | None. A local macOS build is not the Linux artifact (§1). | The Railway build log |
| Rust toolchain | 1.99.1 (`rust-toolchain.toml`, through the Railpack plan) | 1.99.1 (`rust-toolchain.toml`) | `rustc --version`; the Railpack plan |
| Deployment platform | Railway (managed service, no version) | None | Not applicable |

Keep PostgreSQL on the same major version in test and production. The
owner decides a major upgrade (`DOCTRINE.md` P7: downtime). Procedure:

1. Rehearse: `mise run pg:rehearse <old> <new>`
   (`scripts/pg-upgrade-rehearsal.sh`). It fills a cluster of the old
   version with the TypeScript service of the revert path and the Rust
   service, runs `pg_upgrade`, and checks that the data and all feeds stay
   equal and that no poll rewrites a row (Rust `written=0`, unchanged
   `xmin`). It needs no credential. It does
   not exercise the Railway upgrade job.
2. Change `conda:postgresql` in `mise.toml` and the versions in this file in
   one PR. Run `mise run verify` on the new version.
3. Before the upgrade: save the public `/feed.json`.
4. The owner starts the upgrade in Railway: service `Postgres mastofeed`,
   Database, Config, Major Version Upgrade. The preflight report must show
   no blocker. A warning, for example a role with an `md5` password, does
   not block; record it in the PR. Railway takes a backup, runs `pg_upgrade` on the volume, and
   redeploys the service on the new image tag. The database is unavailable
   during the upgrade: `/feed.json` answers 503 and a sync can fail.
5. After the upgrade: the image tag, the start message
   `starting PostgreSQL <version>` in the deploy log of `Postgres mastofeed`,
   and the post-release checks of the feed (§15 Post-release checks).
   Railway offers "Revert and discard changes" for a limited time; the
   revert restores the backup taken before the upgrade. Writes after the
   upgrade are lost; the next sync fetches the recent statuses again. On
   2026-10-15 the upgrade job kept the old data directory for about 24
   hours (`post-upgrade: keeping ... for rollback` in the deploy log).
6. Merge the PR. The deployment runs `migrate` on the new server and logs
   the TLS state.

The data restoration of an upgrade relies on the Railway backup and revert.
The project has no independent restore test of a Railway backup. By owner
decision 2026-10-19, no work tracks such a test.

The production minor version is read from the start message in the deploy
log of `Postgres mastofeed`. `SHOW server_version` needs the database
credential.

The Railpack version is the Railway default and is never pinned (owner
decision 2026-10-19). The repository has no `build.railpackVersion` and no
mise pin for Railpack. The service must not have a `RAILPACK_VERSION`
variable.
The default can be older than the latest Railpack release: the builds of
2026-10-18 (deployment `8e29e2af`) and 2026-10-19 (deployment `8409bd40`)
used `railpack-frontend` v0.41.0, although Railpack 0.42.0 was released on
2026-10-15. The lead set a
`RAILPACK_VERSION` service variable on 2026-10-14. That variable had no
observed effect: the deployments of 2026-10-14, 2026-10-15, 2026-10-18,
and 2026-10-19 used 0.41.0. The lead did not read the variable list, because it contains
secrets. If the variable still exists, the owner removes it in Railway.
Treat the Railpack version as a platform update:

- The freshness check reads the version of the active deployment.
- When a release uses a new Railpack version, compare
  `railpack info --show-plan .` of the old and the new version and record
  the difference in the PR. A plan difference other than the image tags
  is a material change for review.
- Railpack 0.40.1 and 0.41.0 differ only in the builder and runtime image
  tags (`mise-2026.9.15` and `mise-2026.10.8`).

### Dependencies

- **Policy:** `DOCTRINE.md` P7. Run the dependency freshness check (§3).
- **Minimum release age:** 24 hours after publication, for crates, tools,
  Rust, Railpack, and PostgreSQL releases. A security fix can skip the wait.
  - `minimum_release_age = "24h"` in `mise.toml` applies the rule to
    `mise outdated`, `mise latest`, and `mise upgrade` for tools whose
    backend reports release dates, including `rust`. The `conda:postgresql`
    backend reports no dates, so mise does not filter it. Exact pins
    install as written. For a security fix, pass
    `--minimum-release-age 0s`.
  - The lead checks the publication date by hand for crates (crates.io;
    stable Cargo has only the unstable `-Z min-publish-age`), for
    PostgreSQL releases (postgresql.org release notes), and for Railpack
    releases (GitHub release date). Railpack follows the Railway default,
    so the rule cannot hold Railpack back.
- **Upgrade together:**
  - `sqlx`, `jiff-sqlx`, and `cargo:sqlx-cli` in `mise.toml`. Keep the
    sqlx-cli options equal to the pin of the owner's other projects
    (`mise.toml` comment).
  - `tower-http` follows the version that `reqwest` uses, so the graph has
    one copy (§6).
  - `conda:postgresql` follows the production major version (§5).
  - A new Rust version changes `rust-toolchain.toml`, `rust-version` in
    `Cargo.toml` and in `crates/server/Cargo.toml`, and §1. Check the
    Railpack plan.
- **Evidence:** the full `mise run verify`, including the parity tests. A
  change of a crate with a build script or proc macro reassesses the build
  isolation exception (§13).

### Maintenance

Review toolchain and dependency upgrades with the same evidence. After a
SQLx, jiff-sqlx, or migration change, run `mise run sqlx:prepare` and commit
`.sqlx`. After a Railpack change, compare the plan again. Keep the TypeScript
revert path (docs/adr/0002). By owner decision 2026-10-19, no work tracks
the end of the revert path.
