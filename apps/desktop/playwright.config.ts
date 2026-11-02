import { defineConfig } from "@playwright/test";

// The perf budgets of the GUI (docs/architecture.md §10), measured in the real window.
export default defineConfig({
  testDir: "tests",
  timeout: 120_000,
  workers: 1,
  reporter: "list",
});
