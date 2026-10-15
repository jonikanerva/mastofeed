# Mastofeed

Mastofeed polls your Mastodon home timeline, stores the statuses in
PostgreSQL, and publishes them as a JSON Feed at `/feed.json`. It is for
personal use: subscribe to your own home timeline in a feed reader.

## What it does

- Polls the home timeline at start and on a cron schedule (UTC).
- Stores each status once and rewrites it only when a rendered value
  changes, for example after an edit.
- Serves a JSON Feed (version 1) at `/feed.json`, with media attachments and
  an author footer in each item, and a small landing page at `/`.
- Answers conditional requests (`If-None-Match`) with `304 Not Modified`.

## Requirements

- [mise](https://mise.jdx.dev) and [rustup](https://rustup.rs). mise
  installs the other tools, including PostgreSQL 17 for the tests.
- A Mastodon access token with read access to your account.
- A PostgreSQL database for the service.

## Setup (local)

1. Install the tools and the dependencies:
   ```bash
   mise trust && mise run setup
   ```
2. Create your environment file and set your values:
   ```bash
   cp .env.example .env.local
   ```
   Use `.env.local`, not `.env`. The SQLx query macros read every `.env`
   file at compile time, and a value such as `*/5 * * * *` stops the build.
3. Apply the migrations to the database of `.env.local`, then run the service:
   ```bash
   mise run migrate
   ```
   ```bash
   mise run dev
   ```
4. Open `http://localhost:3000/feed.json`.

## Environment variables

- `DATABASE_URL`: PostgreSQL connection URL. The default `sslmode=prefer`
  uses TLS when the server offers it. `sslmode=require` also works. STACK.md
  §5 has the details.
- `MASTODON_BASE_URL`: base URL of your instance, for example
  `https://mastodon.social`. Item links point to this instance.
- `MASTODON_ACCESS_TOKEN`: access token of the account to read.
- `HOME_PAGE_URL`: public base URL of the service. The feed URL and the icon
  URL are built from it.
- `CRON_SCHEDULE`: poll schedule in UTC, five fields or six with seconds.
  Default `*/5 * * * *`.
- `PORT`: HTTP port. Default 3000.
- `FEED_LIMIT`: number of items in the feed, 1 to 1000. Default 200.
- `FEED_TITLE`: feed title. Default `Mastodon timeline`.
- `FEED_DESCRIPTION`: optional feed description.
- `LOG_FORMAT`: `json` (default) or `text`.
- `PUBLIC_DIR`: directory of the static files. Default `public`.

An invalid value stops the service at start. The message names the
variable but not its value.

## Commands

| Command | What it does |
| --- | --- |
| `mise run verify` | All checks: format, lint, query metadata, secrets, dependencies, build, tests |
| `mise run test` | Unit and integration tests against an isolated PostgreSQL |
| `mise run build` | Release build |
| `mise run dev` | Run the service with `.env.local` and readable logs |
| `mise run migrate` | Apply the migrations to the database of `.env.local` |
| `mise run parity` | Compare the feed of the previous TypeScript service and of this service on your live timeline |
| `mise run db:start` / `db:stop` | Start or stop the isolated test database |

The binary has two commands: `mastofeed migrate` applies the migrations and
exits; `mastofeed serve` (the default) serves HTTP and polls.

## Endpoints

- `/`: landing page
- `/feed.json`: JSON Feed
- `/health`: liveness, with the deployed version when Railway provides it

## Deployment

The service runs on Railway. Railpack builds it from `railpack.json`, and
the start command runs the migrations before the server starts. `STACK.md`
§15 describes the settings, the release checks, and the rollback.
