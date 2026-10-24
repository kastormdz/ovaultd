import { appendFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline/promises';

import { configDir } from './state.js';

/**
 * Confirmación para una acción DESTRUCTIVA. En un TTY pregunta; en un script (sin TTY) exige
 * `--yes`: nunca se borra algo que el usuario no vio. Devuelve `false` si dijo que no.
 */
export const confirmarAccion = async (pregunta: string, opciones: { yes?: boolean }): Promise<boolean> => {
  if (opciones.yes) return true;
  if (!process.stdin.isTTY) {
    throw new Error(`${pregunta}\nNo hay terminal para confirmar: repetí con --yes si es lo que querés.`);
  }
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  try {
    const respuesta = await rl.question(`${pregunta} [s/N] `);
    return /^s(i|í)?$/i.test(respuesta.trim());
  } finally {
    rl.close();
  }
};

/** Bitácora local de acciones destructivas (`~/.config/ovaultd/actividad.log`, 0600). */
export const registrarAccion = (texto: string): void => {
  try {
    mkdirSync(configDir(), { recursive: true, mode: 0o700 });
    appendFileSync(join(configDir(), 'actividad.log'), `${new Date().toISOString()} ${texto}\n`, {
      mode: 0o600,
    });
  } catch {
    // Que falle el log no puede tumbar la acción que el usuario pidió.
  }
};
