import { defineConfig } from 'vitest/config';

// Los E2E del motor corren contra `LocalRuntime` (SQLite + filesystem real, sin navegador).
export default defineConfig({
  test: {
    include: ['local/**/*.test.ts'],
    testTimeout: 30_000,
  },
});
