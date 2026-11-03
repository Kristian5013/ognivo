import { defineConfig } from "@playwright/test";

// The GUI in its real window: the perf budgets of docs/architecture.md §10 (`npm run perf`),
// and a session on a real core (`npm run e2e`).
export default defineConfig({
  testDir: "tests",
  timeout: 120_000,
  workers: 1,
  reporter: "list",
});
