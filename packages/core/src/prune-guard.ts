import type { PlanDePrune } from './prune.js';

/**
 * `prune-remote` reconcilia el server contra el disco. Dos formas de destruir un vault
 * entero por accidente, y una guarda para las dos:
 *
 *  1. El disco está vacío (vault mal montado, carpeta equivocada) y el server tiene todo.
 *  2. El disco está a medias: más de la MITAD de los paths del server quedarían huérfanos.
 *
 * En los dos casos se aborta salvo `--permitir-vacio` (F-03 de la auditoría 2026-10-20).
 * Devuelve el mensaje de error, o `null` si el plan puede seguir.
 */
export function guardaDePrune(plan: PlanDePrune, permitirVacio: boolean): string | null {
  if (permitirVacio) return null;

  if (plan.vivos > 0 && plan.enDisco === 0) {
    return (
      `el disco local no tiene archivos y el server tiene ${plan.vivos}: ` +
      'aplicar este plan borraría TODO el vault del server. Abortado.\n' +
      '  ¿Vault mal montado o carpeta equivocada? Si es intencional: --permitir-vacio'
    );
  }

  if (plan.vivos > 0 && plan.huerfanos.length > plan.vivos * 0.5) {
    const porcentaje = Math.round((plan.huerfanos.length / plan.vivos) * 100);
    return (
      `el plan borraría ${plan.huerfanos.length} de ${plan.vivos} paths (${porcentaje} %): ` +
      'más de la mitad del server. Abortado.\n' +
      '  Revisá que el vault esté completo antes de aplicar. Si es intencional: --permitir-vacio'
    );
  }

  return null;
}
