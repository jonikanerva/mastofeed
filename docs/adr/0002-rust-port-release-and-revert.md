# 0002. The Rust port ships in one release with a data-compatible revert path

- Status: accepted
- Date: 2026-10-14

## Context

The owner asked for the whole Rust port in one PR: easy to test locally and
in production, and easy to revert. The production database has the schema
and the Drizzle migration records of the TypeScript service. Railway builds
with Railpack and deploys each merge to `main`.

The profile (STACK-RUST §5) runs migrations as a separate release step, not
as a side effect of the server start.

## Decision

1. The port replaces the TypeScript service in one PR. The schema, the
   environment variable names, the endpoints, and the feed output stay the
   same.
2. `migrations/0001_baseline.sql` is idempotent. On the production schema it
   changes nothing; on an empty database it creates the same schema.
3. The start command in `railpack.json` runs `mastofeed migrate` and then
   `exec mastofeed serve`. The migration is a separate command, and the
   start command is versioned with the code.
4. A revert to the TypeScript service needs no data change. New migrations
   must keep this true until the owner ends the revert path.

## Alternatives

- **Railway pre-deploy command.** Rejected: it is a service setting outside
  the repository. A code revert would leave it in place, and the TypeScript
  image has no `mastofeed` binary, so the revert deploy would fail.
- **Migrate inside `serve`.** Rejected: an implicit start-up effect, against
  the profile, with no benefit over the start command.
- **Railway Infrastructure as Code.** Rejected: it needs Node.js in the
  repository. Config as Code (`railway.json`) is deprecated and new services
  cannot opt in.

## Consequences

- `git revert -m 1` of the merge removes `railpack.json`, so Railpack
  detects Node again. A Railway rollback restores the old image at once.
- The `_sqlx_migrations` table stays after a revert. The TypeScript service
  does not read it.
- During a deploy, the old and the new service poll for a short time. Both
  write the same values; the change guard and the primary key make this safe.
- Evidence (2026-10-14, local): a database with the Drizzle history was
  migrated by the Rust service and filled by three Rust syncs. Then the
  TypeScript migrator ran, and two TypeScript polls rewrote 0 rows. The
  TypeScript feed and the Rust feed were byte-equal.

## Cost of reversal

Low while the revert path holds: a rollback or a code revert.

## Reassessment

When a product change needs a schema change that the TypeScript service
cannot run on. Then the owner decides to end the revert path, and this ADR
is superseded.
