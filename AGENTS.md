# Repository Guidelines

## Project Structure & Module Organization

- `src/` holds the TypeScript app code: Express server (`src/app.ts`), entrypoint (`src/index.ts`), env validation (`src/env.ts`), DB wiring (`src/db.ts`), Mastodon polling (`src/timelinePoller.ts`), and JSON Feed generation (`src/feed.ts`).
- `drizzle/` contains SQL migrations and the migration journal. `drizzle.config.ts` configures Drizzle Kit.
- Root configs include `package.json`, `tsconfig.json`, and `.nvmrc` (Node 24.12.0).
- There is no tests folder yet.

## Build, Test, and Development Commands

- `npm install` installs dependencies.
- `npm run dev` runs the server in watch mode via `tsx`.
- `npm run build` compiles TypeScript to `dist/`.
- `npm run format` formats supported files using Prettier.
- `npm run format:check` verifies formatting without writing changes.
- `npm start` runs the compiled server.
- `npm run db:generate` generates Drizzle migrations from `src/schema.ts`.
- `npm run db:migrate` applies migrations to the configured Postgres database.

## Coding Style & Naming Conventions

- TypeScript with ESM (`"type": "module"`). Use 2-space indentation and `strict` typing.
- Use camelCase for variables/functions, PascalCase for types/classes, and kebab/flat file names (e.g., `timelinePoller.ts`).
- Format code with Prettier (`npm run format`).
- Validation is done with Zod (`src/env.ts`); avoid logging secrets.

## Testing Guidelines

- `npm test` runs `prettier --check .` to enforce formatting. Add a runner and update this section if you introduce unit tests.

## Commit & Pull Request Guidelines

- Commits follow short, imperative, sentence-style messages (e.g., “Add Mastodon polling and persistence”). Keep them focused on a single change.
- PRs should include: purpose, summary of changes, and how you validated (commands run). Link issues if applicable.

## Security & Configuration Tips

- Configure secrets via environment variables; never commit `.env`. See `.env.example` for required keys.
- The `/feed.json` endpoint is public by design; keep server hardened (Helmet enabled).
