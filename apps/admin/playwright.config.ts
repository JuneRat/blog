import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  reporter: "list",
  use: {
    baseURL: process.env.BLOG_BROWSER_URL,
    channel: process.env.PLAYWRIGHT_CHANNEL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
});
