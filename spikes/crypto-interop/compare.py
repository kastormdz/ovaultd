#!/usr/bin/env python3
"""Diffea los vectores del spike Rust vs JS. Exit 1 si difieren.

Uso:  compare.py <out_rust.txt> <out_js.txt>
      compare.py --check <out_rust.txt> <out_js.txt> <fixture.json>   # compara contra el fixture congelado
"""
import json
import sys

KEYS = [
    "PW_NFKC", "KEK", "VAULT_KEY", "WRAPPED_PW", "WRAPPED_RC", "UNWRAP",
    "K_CONTENT", "K_NONCE", "K_PATH", "K_META", "K_BIND", "K_SIGN", "PUBKEY",
    "PATH_ID", "PATH_ENC", "PATH_ENC_RT", "NONCE_BODY",
    "BLOB", "BLOB_LEN", "BLOB_ID", "BLOB_RT", "T_MAC", "SIG", "SIG_VERIFY",
    "NEG_WRONG_PW", "NEG_AAD", "NEG_SWAP_PATH", "NEG_SIG", "NEG_TRUNC",
    "DEDUPE_SAME", "DEDUPE_DIFF", "RENAME",
]


def load(path):
    out = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip():
                continue
            parts = line.split(None, 1)
            if len(parts) == 2:
                out[parts[0].strip()] = parts[1].strip()
    return out


def main():
    args = sys.argv[1:]
    check = False
    if args and args[0] == "--check":
        check = True
        args = args[1:]
    if len(args) < 2:
        print(__doc__)
        return 2
    rust, js = (load(p) for p in args[:2])
    fixture = None
    if check and len(args) > 2:
        with open(args[2], encoding="utf-8") as fh:
            fixture = json.load(fh)

    print(f"{'CAMPO':<15} {'RUST (server)':<18} {'JS (plugin)':<18} VEREDICTO")
    print("-" * 74)
    ok = True
    for key in KEYS:
        a, b = rust.get(key, "(falta)"), js.get(key, "(falta)")
        same = a == b
        ok &= same
        extra = ""
        if fixture is not None:
            f = fixture.get(key)
            if f is None:
                extra = " [falta en fixture]"
                ok = False
            elif f != a:
                extra = " [fixture DIFIERE]"
                ok = False
        print(f"{key:<15} {a[:16]:<18} {b[:16]:<18} {'IDENTICO' if same else 'DIFIERE'}{extra}")

    faltan = [k for k in (set(rust) | set(js)) if k not in KEYS]
    if faltan:
        print(f"\nCLAVES INESPERADAS: {', '.join(sorted(faltan))}")
        ok = False

    print()
    if ok:
        print("==> INTEROP sync2 OK: Rust y JS derivan, cifran y firman byte a byte identico"
              + (" y coinciden con el fixture congelado" if fixture is not None else ""))
        return 0
    print("==> INTEROP FALLA: revisar los layouts de docs/crypto.md antes de tocar los vectores")
    return 1


if __name__ == "__main__":
    sys.exit(main())
