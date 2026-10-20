/** Plan que devuelve `sync.pruneRemote` en dry-run. */
export interface PlanDePrune {
  vivos: number;
  enDisco: number;
  huerfanos: string[];
}

/**
 * `prune-remote` reconcilia el server contra el disco: un vault local vacío o mal montado
 * convierte todos los paths del server en huérfanos, y `--yes` los borraría de verdad.
 * Abortamos salvo `--permitir-vacio` (F-03 de la auditoría 2026-10-20).
 *
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
  return null;
}
