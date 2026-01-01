import { runMigrations } from "./migrations.js";

try {
  await runMigrations({ closePool: true });
} catch (error) {
  console.error("Migrations failed", error);
  process.exit(1);
}
