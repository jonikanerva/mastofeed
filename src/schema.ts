import { index, jsonb, pgTable, text, timestamp } from "drizzle-orm/pg-core";

export const mastodonStatuses = pgTable(
  "mastodon_statuses",
  {
    id: text("id").primaryKey(),
    createdAt: timestamp("created_at", { withTimezone: true }).notNull(),
    editedAt: timestamp("edited_at", { withTimezone: true }),
    url: text("url").notNull(),
    content: text("content").notNull(),
    spoilerText: text("spoiler_text").notNull().default(""),
    accountId: text("account_id").notNull(),
    accountUsername: text("account_username").notNull(),
    accountDisplayName: text("account_display_name").notNull(),
    accountUrl: text("account_url").notNull(),
    raw: jsonb("raw").notNull()
  },
  (table) => ({
    createdAtIdx: index("mastodon_statuses_created_at_idx").on(table.createdAt)
  })
);
