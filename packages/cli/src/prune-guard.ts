/** Plan que devuelve `sync.pruneRemote` en dry-run. */
export interface PlanDePrune {
  vivos: number;
  enDisco: number;
  huerfanos: string[];
}

/**
 * `prune-remote` reconcilia el **server contra el disco local**: los paths vivos del server
 * que no están en disco son "huérfanos" y `--yes` los borra. Si el disco está vacío (vault
 * mal montado, carpeta equivocada, un `mount` que no llegó), TODOS los paths del server
 * caen como huérfanos y el plan borra el vault entero — en el server, firmado, sin vuelta.
 *
 * F-03 de la auditoría 2026-10-20: abortar cuando el origen está vacío y el destino no,
 * salvo que el operador lo pida explícitamente con `--permitir-vacio`.
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
