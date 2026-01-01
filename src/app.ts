import express from "express";
import helmet from "helmet";

import { buildJsonFeed } from "./feed.js";

export const app = express();

app.disable("x-powered-by");
app.use(helmet());
app.use(express.json({ limit: "1mb" }));

app.get("/health", (_req, res) => {
  res.json({ status: "ok" });
});

app.get("/feed.json", async (_req, res, next) => {
  try {
    const feed = await buildJsonFeed();
    res.type("application/feed+json; charset=utf-8").send(feed);
  } catch (error) {
    next(error);
  }
});

app.use(
  (err: unknown, _req: express.Request, res: express.Response, _next: express.NextFunction) => {
    console.error("Unhandled error", err);
    res.status(500).json({ error: "Internal server error" });
  }
);
