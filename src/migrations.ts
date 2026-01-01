import path from "node:path";

import { migrate } from "drizzle-orm/node-postgres/migrator";

import { db, pool } from "./db.js";

export const runMigrations = async (options?: { closePool?: boolean }) => {
  const migrationsFolder = path.resolve(process.cwd(), "drizzle");

  console.log(`Running migrations from ${migrationsFolder}`);
  await migrate(db, { migrationsFolder });
  console.log("Migrations complete");

  if (options?.closePool) {
    await pool.end();
  }
};
