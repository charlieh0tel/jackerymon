// Browser tests for the jackerymon daemon's page.
//
// The daemon needs a key and a Bluetooth adapter to start, so these
// tests serve the page's files from the source tree (fake.mjs) and fake
// the API; a Rust test checks the daemon serves the same files at the
// same paths.  Playwright's own Chromium is in node_modules
// (PLAYWRIGHT_BROWSERS_PATH=0), not the home directory.

import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  testMatch: "*.spec.mjs",
  fullyParallel: true,
  timeout: 30000,
  // Never contacted: every request is answered by fake.mjs.
  use: { baseURL: "http://jackerymon.test" },
});
