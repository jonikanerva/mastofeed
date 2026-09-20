import cron from "node-cron";
import { desc, sql } from "drizzle-orm";

import { db } from "./db.js";
import { env } from "./env.js";
import { masto } from "./mastodon.js";
import { mastodonStatuses } from "./schema.js";

const POLL_LIMIT = 40;

type FeedableStatus = typeof mastodonStatuses.$inferInsert;

const toFeedableStatus = (status: any): FeedableStatus | null => {
  const item = status.reblog ?? status;
  const url = item.url ?? item.uri ?? "";

  if (!item.id || !url) {
    return null;
  }

  return {
    id: String(item.id),
    createdAt: new Date(item.createdAt ?? Date.now()),
    editedAt: item.editedAt ? new Date(item.editedAt) : null,
    url: String(url),
    content: String(item.content ?? ""),
    spoilerText: String(item.spoilerText ?? ""),
    accountId: String(item.account?.id ?? ""),
    accountUsername: String(item.account?.acct ?? ""),
    accountDisplayName: String(item.account?.displayName ?? ""),
    accountUrl: String(item.account?.url ?? ""),
    raw: item
  };
};

export const syncTimelineOnce = async () => {
  const [latest] = await db
    .select({ id: mastodonStatuses.id })
    .from(mastodonStatuses)
    .orderBy(desc(mastodonStatuses.createdAt))
    .limit(1);

  const recent = await masto.v1.timelines.home.list({
    limit: POLL_LIMIT
  });

  const fresh = latest?.id
    ? await masto.v1.timelines.home.list({
        limit: POLL_LIMIT,
        sinceId: latest.id
      })
    : [];

  const combined = [...recent, ...fresh];

  if (!combined.length) {
    return 0;
  }

  const seenIds = new Set<string>();
  const records = combined.reduce<FeedableStatus[]>((acc, status) => {
    const record = toFeedableStatus(status);
    if (record && !seenIds.has(record.id)) {
      seenIds.add(record.id);
      acc.push(record);
    }
    return acc;
  }, []);

  if (records.length > 0) {
    // Rewrite a row only when the status actually changed. Every run re-sends
    // the same ~40 recent statuses, so without this guard each run rewrote all
    // of them: a new row version, a new TOAST entry for the `raw` jsonb, an
    // index update and a WAL record, all for identical data. `raw` is the
    // source of truth, so comparing it covers every derived column too.
    await db
      .insert(mastodonStatuses)
      .values(records)
      .onConflictDoUpdate({
        target: mastodonStatuses.id,
        set: {
          createdAt: sql`excluded.created_at`,
          editedAt: sql`excluded.edited_at`,
          url: sql`excluded.url`,
          content: sql`excluded.content`,
          spoilerText: sql`excluded.spoiler_text`,
          accountId: sql`excluded.account_id`,
          accountUsername: sql`excluded.account_username`,
          accountDisplayName: sql`excluded.account_display_name`,
          accountUrl: sql`excluded.account_url`,
          raw: sql`excluded.raw`
        },
        setWhere: sql`${mastodonStatuses.raw} IS DISTINCT FROM excluded.raw`
      });
  }

  return records.length;
};

export const startTimelinePolling = () => {
  let isRunning = false;

  const run = async () => {
    if (isRunning) {
      return;
    }

    isRunning = true;

    try {
      await syncTimelineOnce();
    } catch (error) {
      console.error("Timeline sync failed", error);
    } finally {
      isRunning = false;
    }
  };

  void run();

  const task = cron.schedule(env.CRON_SCHEDULE, run, {
    timezone: "UTC"
  });

  return task;
};
