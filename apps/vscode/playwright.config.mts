import { defineConfig } from "@playwright/test";

// The extension in a real VS Code, on a real core (`npm run e2e`): one VS Code at a time, each
// test with its own (tests/harness.ts). A test that fails keeps a picture of the window, a
// trace and the logs of VS Code and of the core in test-results/.
export default defineConfig({
  testDir: "tests",
  outputDir: "test-results",
  timeout: 180_000,
  // VS Code and the core are slower to answer on a loaded machine than in a browser alone
  expect: { timeout: 15_000 },
  workers: 1,
  reporter: "list",
});
