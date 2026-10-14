-- Baseline of the mastodon_statuses table.
--
-- The production database already has this schema from the two Drizzle
-- migrations of the TypeScript service (0000_init and 0001_add_edited_at).
-- Every statement is idempotent, so this migration creates the schema on an
-- empty database and changes nothing on an existing one. The Drizzle
-- bookkeeping in the "drizzle" schema stays in place, so a revert to the
-- TypeScript service also needs no data change (docs/adr/0001).

CREATE TABLE IF NOT EXISTS "mastodon_statuses" (
  "id" text PRIMARY KEY NOT NULL,
  "created_at" timestamptz NOT NULL,
  "url" text NOT NULL,
  "content" text NOT NULL,
  "spoiler_text" text DEFAULT '' NOT NULL,
  "account_id" text NOT NULL,
  "account_username" text NOT NULL,
  "account_display_name" text NOT NULL,
  "account_url" text NOT NULL,
  "raw" jsonb NOT NULL
);

CREATE INDEX IF NOT EXISTS "mastodon_statuses_created_at_idx" ON "mastodon_statuses" ("created_at");

ALTER TABLE "mastodon_statuses" ADD COLUMN IF NOT EXISTS "edited_at" timestamptz;
