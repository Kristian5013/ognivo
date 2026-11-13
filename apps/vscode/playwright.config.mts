import { defineConfig } from "@playwright/test";

// The extension in a real VS Code, on a real core (`npm run e2e`).
export default defineConfig({
  testDir: "tests",
  timeout: 180_000,
  workers: 1,
  reporter: "list",
});
