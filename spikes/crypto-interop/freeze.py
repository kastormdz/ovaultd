#!/usr/bin/env python3
"""Congela (o emite candidatos de) los vectores del spike en un JSON versionado.

Uso:  freeze.py --from .out_rust.txt --json <destino.json>

Gate: se NIEGA a escribir si los casos negativos no dan lo esperado. Un fixture que no pasa su propio
gate es peor que no tener fixture.
"""
import argparse, json, sys, time
from pathlib import Path

from compare import KEYS, load

ESPERADO = {
    "UNWRAP": "OK", "PATH_ENC_RT": "OK", "BLOB_RT": "OK", "SIG_VERIFY": "OK",
    "NEG_WRONG_PW": "REJECTED", "NEG_AAD": "REJECTED", "NEG_SWAP_PATH": "REJECTED",
    "NEG_SIG": "REJECTED", "NEG_TRUNC": "REJECTED",
    "DEDUPE_SAME": "SAME", "DEDUPE_DIFF": "DIFFERENT", "RENAME": "OK",
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="src", required=True)
    ap.add_argument("--json", dest="dst", required=True)
    ap.add_argument("--source-label", default="spike crypto-interop (rust)")
    a = ap.parse_args()

    data = load(a.src)
    faltan = [k for k in KEYS if k not in data]
    if faltan:
        print(f"FALTAN VECTORES: {', '.join(faltan)}", file=sys.stderr)
        return 1
    malos = {k: data.get(k) for k, v in ESPERADO.items() if data.get(k) != v}
    if malos:
        print(f"GATE FALLA (valores inesperados): {malos}", file=sys.stderr)
        return 1

    out = {
        "_meta": {
            "spec": "sync2",
            "layout_contract": "docs/decisions/0006-g1-g2-vaultid-y-firma-encadenada.md",
            "source": a.source_label,
            "generated": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "note": "NO editar a mano: se regenera con spikes/crypto-interop/freeze.py a partir del ejecutable. "
                    "Un lado (Rust) es la fuente; el otro (JS) solo puede coincidir o fallar la interop.",
        }
    }
    out.update({k: data[k] for k in KEYS})
    dest = Path(a.dst)
    dest.parent.mkdir(parents=True, exist_ok=True)
    dest.write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"CONGELADO -> {dest} ({len(KEYS)} vectores, gate OK)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
