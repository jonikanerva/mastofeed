# 0001. The stored raw status keeps the masto.js camelCase keys

- Status: accepted
- Date: 2026-10-14

## Context

The TypeScript service read the timeline with masto.js. masto.js converts
every object key of a response to camelCase with `change-case` 5
(`media_attachments` becomes `mediaAttachments`). It does not convert keys
that contain `:` or start with `_`. The service stored the converted status
object in the jsonb column `raw`.

Two parts read `raw` by these keys: the feed (`account.avatar`,
`account.avatarStatic`, `mediaAttachments`) and the change guard of the
upsert (the same three paths). The production table holds rows in this form.

The Rust service receives the snake_case keys of the Mastodon API.

## Decision

The Rust service converts the keys with the rules of `change-case` 5
before it stores a status (`crates/server/src/mastodon/keys.rs`). The stored
form stays the same as before.

## Alternatives

- **Store snake_case and read both forms.** Rejected: rows would have two
  forms. A revert to the TypeScript service would render the new rows
  without attachments and avatars, and its change guard would see a change
  in every new row.
- **Migrate all rows to snake_case.** Rejected: an irreversible data change
  for no user-visible gain, and it breaks the revert path.

## Consequences

- The feed, the change guard, and both service versions read the same rows.
- The key conversion must match `change-case` exactly for ASCII keys. The
  golden files and `keys.rs` tests check this, including digits, repeated
  underscores, case boundaries, and the masto.js exceptions.
- For non-ASCII keys, the conversion uses the Unicode properties of the Rust
  standard library. These approximate the JavaScript general categories.
  Mastodon keys are ASCII.
- Numbers keep their JSON form (`60.0` stays `60.0`). JavaScript wrote `60`.
  jsonb compares both as equal, so the guard sees no change.

## Cost of reversal

A later change to snake_case needs a data migration of every row and the end
of the TypeScript revert path (0002).

## Reassessment

When the owner ends the TypeScript revert path, or when the service needs
to read a key that the camelCase form makes ambiguous.
