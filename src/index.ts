import "dotenv/config";

import { app } from "./app.js";
import { pool } from "./db.js";
import { env } from "./env.js";
import { startTimelinePolling } from "./timelinePoller.js";

const server = app.listen(env.PORT, () => {
  console.log(`Server listening on port ${env.PORT}`);
});

const poller = startTimelinePolling();

const shutdown = () => {
  poller.stop();
  server.close(async () => {
    await pool.end();
    process.exit(0);
  });
};

process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
