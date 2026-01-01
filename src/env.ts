import { z } from "zod";

const envSchema = z.object({
  NODE_ENV: z.enum(["development", "test", "production"]).optional(),
  PORT: z.coerce.number().int().positive().default(3000),
  DATABASE_URL: z.string().min(1),
  MASTODON_BASE_URL: z.string().url(),
  MASTODON_ACCESS_TOKEN: z.string().min(1),
  CRON_SCHEDULE: z.string().min(1).default("*/5 * * * *"),
  FEED_TITLE: z.string().min(1).default("Mastodon timeline"),
  FEED_DESCRIPTION: z.string().min(1).optional(),
  FEED_HOME_PAGE_URL: z.string().url().optional(),
  FEED_FEED_URL: z.string().url().optional()
});

const parsed = envSchema.safeParse(process.env);

if (!parsed.success) {
  const errors = parsed.error.flatten().fieldErrors;
  // Avoid logging secrets while still surfacing missing/invalid fields.
  console.error("Invalid environment variables", errors);
  throw new Error("Invalid environment variables");
}

export const env = parsed.data;
