/**
 * Obtención de la password, sin dejarla nunca en `argv`, logs ni disco.
 *
 * Orden: `OVAULTD_PASSWORD` (env) → `--password-stdin` → prompt interactivo oculto.
 */

import { createInterface } from 'node:readline';

export const readStdin = (): Promise<string> =>
  new Promise((resolve, reject) => {
    let data = '';
    process.stdin.setEncoding('utf8');
    process.stdin.on('data', (chunk) => {
      data += chunk;
    });
    process.stdin.on('end', () => resolve(data));
    process.stdin.on('error', reject);
  });

/** Lee la primera línea de stdin sin la newline final. */
export const readStdinLine = async (): Promise<string> => {
  const texto = await readStdin();
  const linea = texto.split(/\r?\n/, 1)[0] ?? '';
  return linea.trim();
};

const promptOculto = (etiqueta: string): Promise<string> =>
  new Promise((resolve, reject) => {
    if (!process.stdin.isTTY) {
      reject(new Error('no hay TTY para el prompt; usá OVAULTD_PASSWORD o --password-stdin'));
      return;
    }

    const rl = createInterface({ input: process.stdin, output: process.stderr, terminal: true });
    // El prompt va por `setPrompt`/`prompt` y el override deja pasar el PRIMER write:
    // readline con `terminal: true` reescribe la línea al inicializarse y borraba el
    // `write` previo, así que el comando parecía colgado esperando una password invisible.
    const escribirOriginal = (rl as unknown as { _writeToOutput?: (s: string) => void })._writeToOutput;
    let promptMostrado = false;
    (rl as unknown as { _writeToOutput: (texto: string) => void })._writeToOutput = function (texto: string) {
      if (!promptMostrado) {
        promptMostrado = true;
        escribirOriginal?.call(rl, texto);
        return;
      }
      if (texto.includes('\n') || texto.includes('\r')) {
        escribirOriginal?.call(rl, '\n');
      }
    };
    rl.setPrompt(etiqueta);
    rl.prompt();

    rl.question('', (respuesta) => {
      rl.close();
      resolve(respuesta);
    });
    rl.on('error', reject);
  });

/** Resuelve la password según las opciones del comando. */
export const resolvePassword = async (opciones: {
  passwordStdin?: boolean;
  prompt?: string;
}): Promise<string> => {
  const desdeEnv = process.env.OVAULTD_PASSWORD;
  if (desdeEnv && desdeEnv.length > 0) return desdeEnv;

  if (opciones.passwordStdin) {
    const linea = await readStdinLine();
    if (linea.length === 0) throw new Error('stdin vacío: no llegó password');
    return linea;
  }

  return promptOculto(opciones.prompt ?? 'Password: ');
};
