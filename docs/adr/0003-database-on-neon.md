# 0003. The database moves from Railway PostgreSQL to the Neon Free plan

- Status: accepted (pilot)
- Date: 2026-11-15

## Context

The owner wants lower monthly costs for the projects on the Railway Hobby
plan. The Hobby plan costs at least $5 a month and includes $5 of usage.
Railway charges $10 per GB-month of memory. In the week before 2026-11-15,
the service `Postgres mastofeed` used 121 MB of memory on average, about
$1.3 a month. The six application services and the three databases of the
project together used about $8.7 a month.

Neon offers serverless PostgreSQL. On the Free plan, the compute suspends
after 5 minutes without queries. The plan gives 100 CU-hours a month at a
minimum compute size of 0.25 CU, 1 GB of storage, and a history window of
6 hours. The owner created the Neon project `mastofeed` (PostgreSQL 18,
`aws-eu-central-1`) for a pilot. The data is regenerable: the next sync
stores the recent timeline again.

## Decision

1. The service uses the Neon project `mastofeed` as its database. The
   service stays on Railway.
2. The service connects to the direct Neon endpoint with
   `sslmode=verify-full`. It does not use the Neon pooler.
3. Production sets `CRON_SCHEDULE` to `*/30 * * * *`, so the compute can
   suspend between syncs and feed requests.
4. The Railway service `Postgres mastofeed` stays unchanged as the rollback
   database. The owner decides when to remove it.

## Alternatives

- **Stay on Railway PostgreSQL.** No new provider and no extra latency. It
  keeps the cost of about $1.3 a month.
- **Neon Launch plan.** No CU-hour limit, but an always-active 0.25 CU
  compute costs about $19 a month, more than Railway.
- **Neon pooler endpoint.** Rejected: the SQLx migrator needs a session
  advisory lock, and `migrate` through the pooler failed.
- **Keep the five-minute poll.** Rejected: the compute would never suspend
  and would use about 180 CU-hours a month, above the Free plan limit.
- **Neon Import Data Assistant.** Not used: its documented source versions
  are PostgreSQL 14 to 17, and production runs 18. `pg_dump` and
  `pg_restore` copied the data instead (STACK.md §15 Infrastructure).

## Consequences

- Neon becomes a new recipient of the stored statuses (VISION.md, Least
  data).
- Database traffic goes over the public internet from `europe-west4` to
  Frankfurt, with TLS and a certificate check. Each query has more latency
  than on the Railway private network. The first query after a suspension
  waits for the compute to start.
- New statuses reach the feed within 30 minutes plus one feed reader poll,
  not 5 minutes.
- When the project uses all CU-hours of a month, Neon suspends the compute
  until the next month, and `/feed.json` answers 503. Above 1 GB of storage,
  Neon blocks writes.
- The Neon history window of 6 hours adds a short restore path that Railway
  Hobby did not have.
- The `migrate` log line about TLS reports `does not use TLS` on Neon,
  because the Neon proxy ends the TLS connection (STACK.md §5).
- Railway removes the cost of `Postgres mastofeed` only after the owner
  removes that service.

## Cost of reversal

Low while the Railway database exists: copy the data from Neon back to
Railway, set `DATABASE_URL` back to the Railway reference, and remove
`CRON_SCHEDULE` (STACK.md §15 Rollback). No code change is necessary.
Without the copy, for example when Neon has suspended the compute for the
rest of the month, the first sync stores at most 200 statuses of the gap.
After the owner removes the Railway database, a reversal is a data move to
a new database.

## Reassessment condition

Reassess when the Neon project uses more than 85 CU-hours in a month, when
storage passes 800 MB, when the Neon Free plan terms change, or when the
feed latency or the cold starts cause failed feed requests.
