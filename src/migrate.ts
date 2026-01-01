import path from "node:path";

import { migrate } from "drizzle-orm/node-postgres/migrator";
import { Pool } from "pg";
import { drizzle } from "drizzle-orm/node-postgres";

const databaseUrl = process.env.DATABASE_URL;

if (!databaseUrl) {
  console.error("DATABASE_URL is required to run migrations");
  process.exit(1);
}

const pool = new Pool({
  connectionString: databaseUrl
});

const db = drizzle(pool);

try {
  await migrate(db, {
    migrationsFolder: path.resolve(process.cwd(), "drizzle")
  });
} finally {
  await pool.end();
}
