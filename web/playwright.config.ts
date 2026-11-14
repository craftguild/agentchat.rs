import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests/browser',
  timeout: 45_000,
  expect: { timeout: 12_000 },
  workers: 1,
  fullyParallel: false,
  use: { browserName: 'chromium', viewport: { width:1440, height:1000 },
    locale:'en-US', timezoneId:'Asia/Tokyo', trace:'retain-on-failure', screenshot:'only-on-failure' },
  reporter: [['list']],
});
