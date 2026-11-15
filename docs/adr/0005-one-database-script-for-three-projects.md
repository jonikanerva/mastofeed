# 0005. One database script for three projects, with automatic clean-up

- Status: accepted
- Date: 2026-11-15
- Extends: 0004

## Context

ADR 0004 moved the local PostgreSQL servers into Apple containers. caltodo
and spot-price made the same move at the same time, each with its own
script. The three scripts differed in names, task names, passwords, version
pins, signal handling, and clean-up. The owner wants one behaviour in the
three repositories. The owner also wants the database to be invisible:
`dev` and the test tasks start what they need, and old servers go away
without a manual step.

## Decision

1. `scripts/db.sh` is byte-identical in caltodo, mastofeed, and spot-price.
   `scripts/db-project.sh` holds the differences of each project: names,
   database names, the port range, TLS, and the project commands.
2. The commands that need the dev server start it; the last one stops it at
   exit. `db:start` keeps it running until `db:stop`.
3. Every command that starts a server first removes the test and rehearsal
   servers of ended runs, and the dev server and dev volume of removed
   worktrees. A worktree counts as removed only when git no longer lists it,
   its directory is gone, and its parent directory exists.
4. The dev volume name holds the major version
   (`mastofeed-dev-<id>-pg<major>`). A volume without the suffix holds major
   version 18 and stays in use.
5. `db:dump`, `db:restore`, `db:clone`, and `db:reset` manage dev data.
   The last three first write a non-empty database to `.tmp/db/backups/`.
6. The readiness check sends a PostgreSQL SSLRequest. A running dev server
   whose port does not answer is restarted.
7. `.claude/settings.json` lists the database tasks in
   `sandbox.excludedCommands`, because the `container` tool needs XPC.

## Alternatives

- Keep three scripts and align only their interfaces: less work now, but
  the scripts drift again with each fix.
- A shared script in the agent-setup repository: one source, but the owner
  keeps agent-setup free of project test tooling.
- Keep the dev data of removed worktrees until a manual clean-up: no data
  loss after a moved worktree, but the volumes pile up. Agent worktrees hold
  disposable data.
- A limit of test servers over all repositories of the Mac: the owner
  decided against it (2026-11-15). Each repository keeps its own limit of 4.

## Consequences

- A developer or an agent runs only `dev`, `migrate`, `test`, `sqlx:*`,
  `verify`, or `parity`. The `db:` tasks are optional tools.
- A moved worktree loses its dev data. Use `db:dump` before the move.
- A fix to `scripts/db.sh` goes to all three repositories. Compare the files
  with `diff` or `shasum`.
- `db:gc -- --remove-orphaned-dev` goes away: the clean-up removes that data
  itself.

## Cost of reversal

Low. Restore the earlier `scripts/db.sh` from git. No production data
depends on this decision.

## Reassessment

When the container tool changes its CLI or networking in a way that breaks
`scripts/db.sh`, or when the owner leaves the Apple silicon Mac.
