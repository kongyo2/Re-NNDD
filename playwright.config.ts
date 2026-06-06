// Playwright E2E config for Re:NNDD.
//
// Re:NNDD ships as a Tauri 2 desktop app, but its UI is a SvelteKit SPA. The
// real WebKitGTK window is driven separately by the `run-re-nndd` skill's
// WebDriver `driver.mjs`; Playwright can't attach to that. So these E2E tests
// run the *frontend* in a real browser against the Vite dev server, with the
// Tauri IPC boundary faked by tests/e2e/helpers/tauri-mock.ts.
//
// Shape follows mizchi's playwright-test skill, adapted to this repo:
//   - baseURL / webServer use Vite's fixed port 1420 (vite.config.ts sets
//     strictPort: true), NOT the skill's example :3000.
//   - chromium only: keeps CI fast and dependency-light (the skill's basic CI
//     installs just chromium). Add firefox/webkit projects here if desired.
//   - no fixed waits anywhere; web-first assertions auto-retry (see specs).

import { defineConfig, devices } from '@playwright/test';

const CI = !!process.env.CI;

export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: CI, // no stray .only() reaches CI
  retries: CI ? 2 : 0, // retry only on CI
  workers: CI ? 1 : undefined,
  reporter: CI ? [['html'], ['github']] : [['list'], ['html']],
  // Generous ceilings so the Vite dev server's first cold compile of a route
  // chunk doesn't trip assertions — these are auto-retry caps, not fixed waits.
  timeout: 60_000,
  expect: { timeout: 10_000 },
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry', // trace only when a retry happens (cheap + enough)
    screenshot: 'only-on-failure',
    video: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run dev',
    url: 'http://localhost:1420',
    reuseExistingServer: !CI,
    timeout: 120_000,
  },
});
