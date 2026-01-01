import { Feed, type FeedOptions } from "feed";
import { desc } from "drizzle-orm";

import { db } from "./db.js";
import { env } from "./env.js";
import { mastodonStatuses } from "./schema.js";

const buildFeedId = () =>
  env.FEED_FEED_URL ?? env.FEED_HOME_PAGE_URL ?? env.MASTODON_BASE_URL;

export const buildJsonFeed = async () => {
  const rows = await db
    .select({
      id: mastodonStatuses.id,
      createdAt: mastodonStatuses.createdAt,
      url: mastodonStatuses.url,
      content: mastodonStatuses.content,
      spoilerText: mastodonStatuses.spoilerText,
      accountUsername: mastodonStatuses.accountUsername,
      accountDisplayName: mastodonStatuses.accountDisplayName,
      accountUrl: mastodonStatuses.accountUrl
    })
    .from(mastodonStatuses)
    .orderBy(desc(mastodonStatuses.createdAt))
    .limit(env.FEED_LIMIT);

  const newest = rows[0]?.createdAt ?? new Date();
  const items = rows.slice().reverse();

  const feedOptions: FeedOptions = {
    title: env.FEED_TITLE,
    id: buildFeedId(),
    link: env.FEED_HOME_PAGE_URL ?? env.MASTODON_BASE_URL,
    updated: newest,
    generator: "mastofeed",
    copyright: env.FEED_COPYRIGHT
  };

  if (env.FEED_DESCRIPTION) {
    feedOptions.description = env.FEED_DESCRIPTION;
  }

  if (env.FEED_FEED_URL) {
    feedOptions.feedLinks = { json: env.FEED_FEED_URL };
  }

  const feed = new Feed(feedOptions);

  for (const item of items) {
    const authorName = item.accountDisplayName || item.accountUsername;
    const title = item.spoilerText || `Post by @${item.accountUsername}`;

    feed.addItem({
      id: item.id,
      link: item.url,
      date: item.createdAt,
      title,
      content: item.content,
      author: [
        {
          name: authorName,
          link: item.accountUrl
        }
      ]
    });
  }

  return feed.json1();
};
