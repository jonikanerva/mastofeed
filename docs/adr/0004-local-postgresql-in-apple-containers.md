# 0004. Local PostgreSQL runs in Apple containers

- Status: accepted
- Date: 2026-11-15

## Context

The tests, the SQLx checks, and the parity check used one native PostgreSQL
server per checkout. mise installed it from conda (`conda:postgresql`). The
server listened on the fixed port 55432 with trust authentication. Only one
checkout could run it at a time, and two test runs shared it. Agents work
in many worktrees at the same time, so they collided. `dev` and `migrate`
used the `DATABASE_URL` of `.env.local`; there was no local dev database.
`pg:rehearse` needed the binaries of two PostgreSQL versions on the Mac.

The owner wants one container tool on the Apple silicon Mac, without
Docker, Colima, or OrbStack, and no PostgreSQL install on the Mac.

## Decision

1. Every local PostgreSQL server runs in an Apple `container` of the
   official `postgres` image. `MASTOFEED_PG_IMAGE` in `mise.toml` pins the
   tag and the digest. It is the only local PostgreSQL version setting.
2. Each worktree has one dev server with a named volume and durable
   defaults. Its id is a hash of the checkout path.
3. Each independent test run gets a new test server on tmpfs with fsync
   off. `verify` shares one server over its steps.
4. `scripts/db.sh` owns the lifecycle: names, labels, ports, passwords,
   TLS, readiness, locks, slots, clean-up, and the reaping of abandoned
   servers. The mise tasks are the interface.
5. Servers publish on `127.0.0.1` only, require a password, and offer TLS
   with a self-signed X.509 v3 certificate made in the container.
6. `pg:rehearse` runs `pg_upgrade` in a container of the new image with
   the binaries of the old image copied in.

## Alternatives

- Keep conda PostgreSQL with a port and a data directory per run: no new
  tool, but each run needs `initdb`, and `pg:rehearse` still needs two
  versions on the Mac.
- Docker Desktop, Colima, or OrbStack: more mature, but a second container
  tool on the Mac, against the owner's choice.
- A derived image with both versions for `pg:rehearse`: needs the shared
  builder VM. Copying the binaries at run time needs no build.

## Consequences

- The Mac needs the container tool 1.0.0 or later and its system service.
  The tool talks over XPC, so the database tasks fail in a Seatbelt
  sandbox and must run outside it.
- A test run costs about 4 to 6 s for the server start. The suite ran
  slower than on the native server in the measurements (STACK.md §3).
- Runs in parallel and worktrees no longer collide. At most 4 test servers
  run at the same time.
- The old `.tmp/postgres` directories stay on disk. Nothing uses them.

## Cost of reversal

Low. Restore `scripts/test-db.sh`, the `conda:postgresql` pin, and the
former tasks from git. No production data depends on this decision.

## Reassessment

When the container tool changes its CLI or networking in a way that breaks
`scripts/db.sh`, when the suite time becomes a problem, or when the owner
leaves the Apple silicon Mac.
