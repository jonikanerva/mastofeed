# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

- `npm run dev` — run the server in watch mode via `tsx` (no build step).
- `npm run build` — compile TypeScript to `dist/`.
- `npm start` — run compiled server from `dist/` (runs migrations on boot).
- `npm test` / `npm run format:check` — Prettier check; this is the only test (no unit-test runner exists).
- `npm run format` — write Prettier formatting.
- `npm run db:generate` — generate a Drizzle migration after editing `src/schema.ts`.
- `npm run db:migrate` — apply migrations manually (also runs automatically on startup).

Node 24.12.0 (`.nvmrc`). Requires a Postgres `DATABASE_URL` and a Mastodon access token; copy `.env.example` to `.env`. ESM throughout (`"type": "module"`), so intra-`src` imports use `.js` extensions even for `.ts` files.

## Architecture

A single Express process with two concurrent responsibilities: serving a JSON Feed and polling Mastodon in the background.

**Boot sequence** (`src/index.ts`): run migrations → fail-fast and `process.exit(1)` if they fail → start HTTP server → start the poller. SIGINT/SIGTERM stop the poller, close the server, and end the pg pool.

**Env is validated and frozen at import time** (`src/env.ts`): a Zod schema parses `process.env` and throws if invalid. Every module imports `env` from here rather than reading `process.env` directly. `src/index.ts` imports `dotenv/config` first so `.env` is loaded before any `env` access.

**Polling** (`src/timelinePoller.ts`): `node-cron` on `CRON_SCHEDULE` (UTC), plus one immediate run on startup. An `isRunning` guard prevents overlapping syncs. Each sync queries the newest stored `id`, fetches the recent home timeline and (if a baseline exists) anything newer via `sinceId`, dedupes, and upserts via `onConflictDoUpdate`. The upsert carries a change guard, so a row is rewritten only when a field the feed renders has changed: edits overwrite, interaction counts do not. Reblogs are unwrapped to the underlying status (`status.reblog ?? status`). The full Mastodon status object is persisted in the `raw` jsonb column.

**Feed generation** (`src/feed.ts`): reads the newest `FEED_LIMIT` rows ordered by `createdAt`, then **reverses** them so the `feed` library emits chronological order. Item HTML is assembled as `content + attachments + author footer`; attachments and avatar are pulled from the `raw` jsonb (not the typed columns). All interpolated values go through `escapeHtml`. Item links are rewritten to the local instance form `BASE/@account/id` rather than the canonical status URL.

**Persistence** (`src/schema.ts`, `src/db.ts`): one table `mastodon_statuses` keyed by Mastodon status id (text), indexed on `created_at`. Drizzle over a `pg` Pool. The `raw` column holds everything not promoted to a typed column, and `src/feed.ts` reads the author avatar and the media attachments out of it. It is a snapshot from the last real change, not a live copy: the change guard compares the typed columns plus `raw -> 'mediaAttachments'` and the two avatar paths, so reply, boost and favourite counts inside `raw` go stale on purpose. Nothing reads them.

**Migrations**: `src/migrations.ts` applies SQL files from `drizzle/` using the Drizzle migrator. After changing `src/schema.ts`, run `db:generate` to produce a new SQL file in `drizzle/`, then `db:migrate` (or restart) to apply.

## Endpoints

`/` (static landing page from `public/`), `/feed.json` (JSON Feed), `/health`.

## Conventions

- TypeScript with `strict` typing, 2-space indentation. camelCase for variables/functions, PascalCase for types, flat/kebab file names (e.g. `timelinePoller.ts`).
- Validate external input with Zod (see `src/env.ts`) and never log secrets — `env.ts` deliberately logs only the field names of invalid env vars.
- `/feed.json` is public by design; keep the server hardened (Helmet is enabled in `src/app.ts`).
- Commits: short, imperative, sentence-style (e.g. "Add Mastodon polling and persistence"), one focused change each. PRs should state purpose, summary, and how it was validated.

## Deployment

This service is deployed on **Railway**. The deploy runs `npm run build` then `npm start`, which applies migrations on boot and begins polling. Ensure Railway can reach Postgres and the Mastodon instance, and set `HOME_PAGE_URL` to the public Railway URL (used to build feed URLs).
