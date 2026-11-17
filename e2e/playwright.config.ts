import { defineConfig, devices } from "@playwright/test";

/**
 * End-to-end tests (design doc §14.3). They need a running stack: the API
 * serving the web build, the worker, an empty database, and Plaid sandbox
 * keys. See README.md in this folder.
 */
export default defineConfig({
  testDir: "tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 300_000,
  expect: { timeout: 15_000 },
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: process.env.E2E_BASE_URL ?? "http://localhost:8080",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    ...devices["Desktop Chrome"],
    // Locally, `E2E_CHANNEL=msedge` uses the installed Edge.
    ...(process.env.E2E_CHANNEL ? { channel: process.env.E2E_CHANNEL } : {}),
  },
});
