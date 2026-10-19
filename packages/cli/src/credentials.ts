/**
 * Obtención de datos interactivos, sin dejar la password nunca en `argv`, logs ni disco.
 *
 * Orden para la password: `OVAULTD_PASSWORD` (env) → `--password-stdin` → prompt oculto.
 * El prompt oculto NO usa readline: se escribe una vez y se leen teclas en modo raw
 * (`*` por carácter). readline con `terminal: true` reescribe la línea y en varias
 * terminales borraba el prompt, así que el comando parecía colgado esperando a ciegas.
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

/** Pregunta con eco visible (email, carpeta). `readline.question` gestiona el prompt. */
export const preguntarVisible = (etiqueta: string, defecto?: string): Promise<string> =>
  new Promise((resolve, reject) => {
    if (!process.stdin.isTTY) {
      reject(new Error(`no hay TTY para preguntar «${etiqueta}»; pasalo como opción`));
      return;
    }
    const rl = createInterface({ input: process.stdin, output: process.stdout });
    rl.question(`${etiqueta}${defecto !== undefined ? ` [${defecto}]` : ''}: `, (respuesta) => {
      rl.close();
      const limpio = respuesta.trim();
      resolve(limpio.length > 0 ? limpio : (defecto ?? ''));
    });
    rl.on('error', reject);
  });

/** Prompt oculto en modo raw: el prompt se ve SIEMPRE y cada tecla pinta un `*`. */
const promptOculto = (etiqueta: string): Promise<string> =>
  new Promise((resolve, reject) => {
    const stdin = process.stdin;
    if (!stdin.isTTY) {
      reject(new Error('no hay TTY para el prompt; usá OVAULTD_PASSWORD o --password-stdin'));
      return;
    }

    process.stdout.write(etiqueta);
    stdin.setRawMode(true);
    stdin.resume();
    stdin.setEncoding('utf8');

    let valor = '';
    const terminar = (fn: () => void): void => {
      stdin.setRawMode(false);
      stdin.pause();
      stdin.removeListener('data', alTeclear);
      process.stdout.write('\n');
      fn();
    };

    const alTeclear = (chunk: string): void => {
      for (const caracter of chunk) {
        if (caracter === '\r' || caracter === '\n') {
          const listo = valor;
          terminar(() => resolve(listo));
          return;
        }
        if (caracter === '\u0003') {
          // Ctrl+C
          terminar(() => reject(new Error('cancelado')));
          return;
        }
        if (caracter === '\u007f' || caracter === '\b') {
          if (valor.length > 0) {
            valor = valor.slice(0, -1);
            process.stdout.write('\b \b');
          }
          continue;
        }
        if (caracter >= ' ') {
          valor += caracter;
          process.stdout.write('*');
        }
      }
    };

    stdin.on('data', alTeclear);
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
