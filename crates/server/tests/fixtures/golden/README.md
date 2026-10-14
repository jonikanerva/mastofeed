# Golden files of the TypeScript service

These files are the output of the TypeScript service at commit `d17168d`
(the last TypeScript release) for the synthetic timeline in `../mastodon`.
The Rust parity tests (`tests/integration/parity.rs`) compare against them.
The timeline is synthetic. It contains no real account or status.

| Directory | Mode | Settings |
| --- | --- | --- |
| `sync/` | `sync` | `FEED_LIMIT=200`, `FEED_DESCRIPTION="Fixture description"` |
| `limit3/` | `feed` on the `sync` database | `FEED_LIMIT=3`, no description |
| `empty/` | `empty` | `FEED_LIMIT=200`, no description |

All runs use `MASTODON_BASE_URL=http://127.0.0.1:48080`,
`MASTODON_ACCESS_TOKEN=fixture-token`, `HOME_PAGE_URL=https://feed.example/`,
and `FEED_TITLE="Fixture feed"`.

- `feed.json`: the `/feed.json` body (`buildJsonFeed()`).
- `raw.json`: the stored `raw` column by status id.
- `rows.json`: the stored typed columns, times in microseconds.
- `requests.json`: the requests that the fake Mastodon server received.

## Reproduce

1. Extract commit `d17168d` into an empty directory and run `npm ci`:
   `git archive d17168d | tar -x -C <dir>`.
2. Copy `generate.ts` to the root of that directory and `../mastodon` to
   `fixtures/mastodon` in it.
3. Create empty databases on the isolated server (`mise run db:start`).
4. In that directory, for each row of the table, run
   `npx tsx generate.ts <mode> <out-dir>` with `DATABASE_URL` and the
   settings above. Run `feed` for `limit3/` on the database of `sync/`.

Port 48080 must be free: the generator starts the fake Mastodon server.
