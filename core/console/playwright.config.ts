import { defineConfig, devices } from "@playwright/test";

// Runs against the throwaway Frood of e2e/serve.sh, which serves the built console.
export default defineConfig({
  testDir: "e2e",
  workers: 1,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: `http://127.0.0.1:${process.env.FROOD_CONSOLE_PORT ?? 18080}`,
    trace: "retain-on-failure",
    ...devices["Desktop Chrome"],
    viewport: { width: 1280, height: 860 },
    locale: "en-US",
  },
});
