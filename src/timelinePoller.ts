import cron from "node-cron";
import { desc } from "drizzle-orm";

import { db } from "./db.js";
import { env } from "./env.js";
import { masto } from "./mastodon.js";
import { mastodonStatuses } from "./schema.js";

const POLL_LIMIT = 40;

const toFeedableStatus = (status: any) => {
  const item = status.reblog ?? status;
  const url = item.url ?? item.uri ?? "";

  if (!item.id || !url) {
    return null;
  }

  return {
    id: String(item.id),
    createdAt: new Date(item.createdAt ?? Date.now()),
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

  const timeline = await masto.v1.timelines.home.list({
    limit: POLL_LIMIT,
    ...(latest?.id ? { sinceId: latest.id } : {})
  });

  if (!timeline.length) {
    return 0;
  }

  const records = timeline.map(toFeedableStatus).filter(Boolean);

  if (records.length > 0) {
    await db
      .insert(mastodonStatuses)
      .values(records)
      .onConflictDoNothing();
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
