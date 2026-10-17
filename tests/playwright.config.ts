/**
 * Playwright Test — specs de browser (fase 2).
 *
 * Los casos críticos corren contra Chromium REAL y procesos reales: matar una promesa
 * no simula un proceso muerto (trampa declarada del plan).
 */

import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './browser',
  timeout: 120_000,
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    headless: true,
  },
});
