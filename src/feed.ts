import { Feed, type FeedOptions } from "feed";
import { desc } from "drizzle-orm";

import { db } from "./db.js";
import { env } from "./env.js";
import { mastodonStatuses } from "./schema.js";

type RawStatus = {
  mediaAttachments?: Array<{
    type?: string;
    url?: string;
    previewUrl?: string;
    description?: string | null;
  }>;
  account?: {
    avatar?: string;
    avatarStatic?: string;
  };
};

const normalizeHomeUrl = () => env.HOME_PAGE_URL.replace(/\/+$/, "");
const buildFeedUrl = () => `${normalizeHomeUrl()}/feed.json`;
const buildFeedId = () => buildFeedUrl();

const escapeHtml = (value: string) =>
  value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

const getRawRecord = (raw: unknown): RawStatus | null => {
  if (!raw || typeof raw !== "object") {
    return null;
  }

  return raw as RawStatus;
};

const getAuthorAvatar = (raw: unknown) => {
  const record = getRawRecord(raw);
  const avatar = record?.account?.avatar;
  const avatarStatic = record?.account?.avatarStatic;

  if (avatar) {
    return avatar;
  }

  if (avatarStatic) {
    return avatarStatic;
  }

  return undefined;
};

const getAttachments = (raw: unknown) => {
  const record = getRawRecord(raw);
  return Array.isArray(record?.mediaAttachments) ? (record?.mediaAttachments ?? []) : [];
};

const renderAttachmentsHtml = (raw: unknown) => {
  const attachments = getAttachments(raw);

  if (attachments.length === 0) {
    return "";
  }

  const html = attachments
    .map((attachment) => {
      const url = attachment.url ?? attachment.previewUrl;
      if (!url) {
        return "";
      }

      const description =
        typeof attachment.description === "string" ? escapeHtml(attachment.description) : "";
      const escapedUrl = escapeHtml(url);

      switch (attachment.type) {
        case "image":
          return `<figure class="attachment attachment-image"><img src="${escapedUrl}" alt="${description}" loading="lazy" />${
            description ? `<figcaption>${description}</figcaption>` : ""
          }</figure>`;
        case "video":
        case "gifv":
          return `<figure class="attachment attachment-video"><video controls src="${escapedUrl}" preload="metadata"></video>${
            description ? `<figcaption>${description}</figcaption>` : ""
          }</figure>`;
        case "audio":
          return `<figure class="attachment attachment-audio"><audio controls src="${escapedUrl}"></audio>${
            description ? `<figcaption>${description}</figcaption>` : ""
          }</figure>`;
        default:
          return `<p class="attachment attachment-link"><a href="${escapedUrl}">Attachment</a></p>`;
      }
    })
    .filter(Boolean)
    .join("");

  return html ? `<div class="attachments">${html}</div>` : "";
};

type JsonAttachment = {
  url: string;
  mime_type: string;
  title?: string;
};

const buildJsonAttachments = (raw: unknown): JsonAttachment[] => {
  const attachments = getAttachments(raw);

  return attachments
    .map((attachment) => {
      const url = attachment.url ?? attachment.previewUrl;
      if (!url) {
        return null;
      }

      const type = attachment.type ?? "unknown";
      const mimeType =
        type === "image"
          ? "image/*"
          : type === "video" || type === "gifv"
            ? "video/*"
            : type === "audio"
              ? "audio/*"
              : "application/octet-stream";

      return {
        url,
        mime_type: mimeType,
        title: attachment.description ?? undefined
      };
    })
    .filter((attachment): attachment is JsonAttachment => attachment !== null);
};

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
      accountUrl: mastodonStatuses.accountUrl,
      raw: mastodonStatuses.raw
    })
    .from(mastodonStatuses)
    .orderBy(desc(mastodonStatuses.createdAt))
    .limit(env.FEED_LIMIT);

  const newest = rows[0]?.createdAt ?? new Date();
  const items = rows.slice().reverse();

  const feedOptions: FeedOptions = {
    title: env.FEED_TITLE,
    id: buildFeedId(),
    link: env.HOME_PAGE_URL,
    updated: newest,
    generator: "mastofeed",
    copyright: `© ${new Date().getFullYear()} ${normalizeHomeUrl()}`
  };

  if (env.FEED_DESCRIPTION) {
    feedOptions.description = env.FEED_DESCRIPTION;
  }

  feedOptions.feedLinks = { json: buildFeedUrl() };

  const feed = new Feed(feedOptions);

  for (const item of items) {
    const authorName = item.accountDisplayName || item.accountUsername;
    const avatarUrl = getAuthorAvatar(item.raw);
    const attachmentsHtml = renderAttachmentsHtml(item.raw);
    const content = `${item.content}${attachmentsHtml}`;
    const jsonAttachments = buildJsonAttachments(item.raw);

    const author = {
      name: authorName,
      link: item.accountUrl,
      ...(avatarUrl ? { avatar: avatarUrl } : {})
    };

    const feedItem = {
      id: item.id,
      link: item.url,
      date: item.createdAt,
      title: authorName,
      content,
      ...(avatarUrl ? { image: avatarUrl } : {}),
      author: [author],
      ...(jsonAttachments.length
        ? { extensions: [{ name: "attachments", objects: jsonAttachments }] }
        : {})
    };

    feed.addItem(feedItem);
  }

  return feed.json1();
};
