# 0006. The feed is served from memory

- Status: accepted
- Date: 2026-11-17
- Extends: 0003

## Context

ADR 0003 moved the database to the Neon Free plan. The plan gives 100
CU-hours a month, and each start of the compute keeps it active for at
least 5 minutes.

The handler of `/feed.json` read the database on each request, also for a
`304` response. Feedbin requests the feed about every 16 minutes (about 90
requests in the 24 hours before 2026-11-17 06:45 UTC). The syncs start the
compute 48 times a day. The requests and the syncs seldom fall in the same
5 minutes, so the compute started 3 to 6 times an hour and was active 43 %
of the time. At 0.274 CU on average, the projection was 85 to 95 CU-hours a
month. That is above the reassessment condition of ADR 0003 (85 CU-hours).
At 100 CU-hours, Neon suspends the compute until the next month.

The poller is the only writer of the status table, and the service runs one
replica.

## Decision

1. The poller renders the feed after each sync, also after a failed sync,
   and keeps the document and its `ETag` in memory (`FeedCache`).
2. `/feed.json` serves the document in memory. It reads the database only
   while the cache is empty: from the start of the process to the end of
   the first refresh. That read does not fill the cache.
3. A failed refresh keeps the previous document.
4. Owner decision (2026-11-17): when the database is unavailable,
   `/feed.json` shows the last document, not a 503 response. `VISION.md`
   records the changed failure behaviour.

## Alternatives

- **Sync once an hour, no cache.** No code change. About 70 CU-hours a
  month, too near the limit, and new statuses reach the feed later.
- **Cache and sync once an hour.** About 18 CU-hours a month. Rejected: the
  cache alone gives enough margin, and the 30-minute schedule stays.
- **Lower the maximum compute size to 0.25 CU.** It saves about 9 %. Not
  enough alone.
- **Back to Railway PostgreSQL, or the Neon Launch plan.** About $1.3 or
  about $9 to $10 a month. Rejected while the Free plan is sufficient.
- **A cache that the handler fills.** Rejected: a read that starts before a
  sync could replace the newer document of that sync, and the feed would
  stay old until the next sync.

## Consequences

- A feed request starts no compute. The estimate is about 35 CU-hours in 30
  days: 48 starts a day, about 322 s each, 0.274 CU on average.
- The feed bytes do not change. The golden parity test also reads the
  document through the cache and the router.
- The feed shows the table as it was after the last sync of this instance.
  During a deployment, the old and the new instance can both sync, and the
  feed of an instance can lag the table until its next sync.
- With the database unavailable, the feed stays available and does not
  change. After a restart of the service, `/feed.json` answers 503 until
  the database is available.
- The process keeps one feed document in memory (350 KB for 200 items on
  2026-11-17).

## Cost of reversal

Low. Revert the PR. The schema and the data do not change.

## Reassessment condition

Reassess when the service gets a second replica or a second writer of the
table, when the Neon project uses more than 85 CU-hours in a month, or when
the service leaves the Neon Free plan.
