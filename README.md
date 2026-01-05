# Mastofeed

Mastofeed is a small Express service that polls your Mastodon home timeline, stores statuses in Postgres, and publishes a JSON Feed at `/feed.json`. It is meant for personal use so you can subscribe to your own Mastodon timeline in a feed reader.

## What it does

- Polls your Mastodon home timeline on a cron schedule (UTC).
- Persists statuses in Postgres with Drizzle ORM.
- Serves a JSON Feed at `/feed.json` plus a simple landing page at `/`.
- Includes media attachments in the feed when available.

## Requirements

- Node.js 24.12.0 (see `.nvmrc`)
- Postgres database
- Mastodon access token with read access to your account timeline

## Setup (local)

1. Install dependencies:
   ```sh
   npm install
   ```
2. Create your env file:
   ```sh
   cp .env.example .env
   ```
3. Update `.env` with your settings (see Environment variables below).
4. Run the dev server:
   ```sh
   npm run dev
   ```
5. Visit `http://localhost:3000/feed.json`.

Migrations run automatically on startup, but you can also run them manually:

```sh
npm run db:migrate
```

## Environment variables

These map directly to `.env.example`.

- `DATABASE_URL`: Postgres connection string.
- `MASTODON_BASE_URL`: Base URL of your instance (e.g. `https://mastodon.social`).
- `MASTODON_ACCESS_TOKEN`: Access token for the account you want to pull from.
- `HOME_PAGE_URL`: Public base URL where the app is hosted (used to build feed URLs).
- `CRON_SCHEDULE`: Cron expression for polling (runs in UTC).
- `PORT`: Port to bind the HTTP server.
- `FEED_LIMIT`: Max items in the feed (up to 1000).
- `FEED_TITLE`: Title of the feed.
- `FEED_DESCRIPTION`: Optional description for the feed.

## Build and run (production)

1. Install dependencies:
   ```sh
   npm install
   ```
2. Build:
   ```sh
   npm run build
   ```
3. Start:
   ```sh
   npm start
   ```

`npm start` runs migrations on boot, starts the HTTP server, and begins polling.

## Deploy notes

- Make sure your deployment environment can reach Postgres and your Mastodon instance.
- Set `HOME_PAGE_URL` to the public URL users will hit
- Use a process manager (systemd, pm2, etc.) to keep the service running.
- If you prefer explicit migrations, run `npm run db:migrate` during deploy.

## Endpoints

- `/` simple landing page
- `/feed.json` JSON Feed output
- `/health` health check
