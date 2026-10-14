// Generates the golden files of the Rust parity tests from the TypeScript
// service. This file is a record. Nothing compiles or runs it in this
// repository, because the TypeScript service is not in the tree any more.
//
// Procedure (see README.md in this directory):
//   1. Check out commit d17168d (the last TypeScript release) in a scratch
//      worktree and run `npm ci`.
//   2. Copy this file and ../mastodon/*.json into that worktree.
//   3. Start an empty PostgreSQL 17 database.
//   4. Run `npx tsx generate.ts <mode> <out-dir>` with the environment below.
//
// Modes:
//   sync   Migrate, run two polls against the fake Mastodon server, then
//          write feed.json, raw.json, rows.json, and requests.json.
//   feed   Write feed.json only (use another FEED_LIMIT or no description).
//   empty  Migrate an empty database, then write feed.json.
//
// Environment: DATABASE_URL, MASTODON_BASE_URL=http://127.0.0.1:48080,
// MASTODON_ACCESS_TOKEN=fixture-token, HOME_PAGE_URL=https://feed.example/,
// FEED_TITLE, FEED_LIMIT, and optionally FEED_DESCRIPTION.

import { readFileSync, writeFileSync } from "node:fs";
import http from "node:http";
import path from "node:path";

const [mode, outDir] = process.argv.slice(2);
if (!mode || !outDir) {
  throw new Error("usage: generate.ts <sync|feed|empty> <out-dir>");
}

const fixtureDir = process.env.FIXTURE_DIR ?? "fixtures/mastodon";
const recent = readFileSync(path.join(fixtureDir, "recent.json"), "utf8");
const fresh = readFileSync(path.join(fixtureDir, "fresh.json"), "utf8");
const requests: Array<{ method: string; path: string; authorization: string | null }> = [];

const server = http.createServer((req, res) => {
  requests.push({
    method: req.method ?? "",
    path: req.url ?? "",
    authorization: req.headers.authorization ?? null
  });
  const url = new URL(req.url ?? "/", "http://127.0.0.1");
  if (url.pathname !== "/api/v1/timelines/home") {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { "content-type": "application/json; charset=utf-8" });
  res.end(url.searchParams.has("since_id") ? fresh : recent);
});
await new Promise<void>((resolve) => server.listen(48080, "127.0.0.1", resolve));

const { pool } = await import("./src/db.js");
const { runMigrations } = await import("./src/migrations.js");
const { syncTimelineOnce } = await import("./src/timelinePoller.js");
const { buildJsonFeed } = await import("./src/feed.js");

if (mode === "sync") {
  await runMigrations();
  await syncTimelineOnce();
  await syncTimelineOnce();

  const raw = await pool.query("SELECT id, raw FROM mastodon_statuses ORDER BY id");
  writeFileSync(
    path.join(outDir, "raw.json"),
    JSON.stringify(Object.fromEntries(raw.rows.map((row) => [row.id, row.raw])), null, 2) + "\n"
  );

  const rows = await pool.query(
    `SELECT id,
            to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS created_at,
            to_char(edited_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') AS edited_at,
            url, content, spoiler_text, account_id, account_username,
            account_display_name, account_url
       FROM mastodon_statuses ORDER BY id`
  );
  writeFileSync(path.join(outDir, "rows.json"), JSON.stringify(rows.rows, null, 2) + "\n");
  writeFileSync(path.join(outDir, "requests.json"), JSON.stringify(requests, null, 2) + "\n");
}

if (mode === "empty") {
  await runMigrations();
}

writeFileSync(path.join(outDir, "feed.json"), await buildJsonFeed());

await pool.end();
server.close();
