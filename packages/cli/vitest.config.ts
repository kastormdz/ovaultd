import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vitest/config';

/**
 * Los tests corren contra el SRC de los paquetes hermanos (sin build previo):
 * el runtime real usa `dist` vía los exports del workspace.
 */
export default defineConfig({
  resolve: {
    alias: {
      '@ovaultd/protocol': fileURLToPath(new URL('../protocol/src/index.ts', import.meta.url)),
      '@ovaultd/crypto': fileURLToPath(new URL('../crypto/src/index.ts', import.meta.url)),
    },
  },
});
