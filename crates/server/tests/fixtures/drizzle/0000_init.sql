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
