import { createRestAPIClient } from "masto";

import { env } from "./env.js";

export const masto = createRestAPIClient({
  url: env.MASTODON_BASE_URL,
  accessToken: env.MASTODON_ACCESS_TOKEN
});
