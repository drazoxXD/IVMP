#!/usr/bin/env python3
"""Builds Client/Hooks/natives.json, the native name table of the client's Lua runtime.

Source: the native lists of the GTA IV script tooling in the ScriptHookDotNet sources
(github.com/Priler/gta4_scripthookdotnet, docs/natives/, MIT licensed; the lists go back to Alexander
Blade's scocl). The hashes match GTA IV Complete Edition 1.2.0.59: every name of natives.cfg.txt
is registered in the game.

  natives.cfg.txt   NAME=$HASH
  natives.h.txt     extern <ret> NAME(<type> <name>, ...);   (types for the signature)
  missing-from-headers.txt   NAME  0xHASH  <ret> NAME(<type> <name>, ...)   (the same for what the header lacks)

Usage: gen_natives.py <folder holding natives.cfg.txt and natives.h.txt> [output.json]

Signature letters (see Client/Hooks/src/luanatives.rs): arguments i int, u unsigned, f float,
b bool, s string, a any (typed by the Lua value), I / F / V out parameter (int / float / vector of
three floats; a pointer argument in the header); returns i u f b s, none for void.
"""
import json
import re
import sys
from pathlib import Path

DECL = re.compile(r"^\s*extern\s+([A-Za-z0-9_]+(?:\s*\*)?)\s*([A-Z][A-Z0-9_]*)\s*\((.*)\)\s*;")


def parse_type(tokens):
    """(letter, is_pointer) of a parameter's type tokens (without its name)."""
    pointer = "*" in tokens
    base = [t for t in tokens if t not in ("*", "const", "struct", "unsigned")]
    base = base[0] if base else "int"
    if pointer:
        if base == "char":
            return "s"
        return {"float": "F", "Vector3": "V"}.get(base, "I")
    low = base.lower()
    if low == "float":
        return "f"
    if low in ("boolean", "bool"):
        return "b"
    if low == "uint":
        return "u"
    if low == "scriptany":
        return "a"
    return "i"  # int, handles (Ped, Vehicle, ...) and enums


def parse_param(text):
    tokens = text.replace("*", " * ").split()
    # The last identifier is the parameter's name unless it is the only word of the type.
    words = [t for t in tokens if t not in ("*", "const", "struct", "unsigned")]
    if len(words) >= 2 and tokens[-1] != "*":
        tokens = tokens[:-1]
    return parse_type(tokens)


def parse_return(text):
    tokens = text.replace("*", " * ").split()
    if tokens == ["void"]:
        return ""
    letter = parse_type(tokens)
    if "*" in tokens:
        return "s" if letter == "s" else "i"
    return {"a": "i"}.get(letter, letter)


def main():
    folder = Path(sys.argv[1])
    out = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(__file__).resolve().parent.parent / "Hooks" / "natives.json"
    hashes = {}
    for line in (folder / "natives.cfg.txt").read_text(encoding="utf-8", errors="replace").splitlines():
        m = re.match(r"^([A-Za-z0-9_]+)=\$([0-9A-Fa-f]+)\s*$", line.strip())
        if m:
            hashes[m.group(1).upper()] = int(m.group(2), 16)
    signatures = {}
    for line in (folder / "natives.h.txt").read_text(encoding="utf-8", errors="replace").splitlines():
        if not line.lstrip().startswith("extern"):
            continue
        line = re.sub(r"/\*.*?\*/", "", line.split("//")[0])
        m = DECL.match(line)
        if not m:
            continue
        ret, name, params = m.group(1), m.group(2), m.group(3).strip()
        args = "" if params in ("", "void") else "".join(parse_param(p) for p in params.split(",") if p.strip())
        signatures[name.upper()] = (args, parse_return(ret))
    # Natives the header lacks: NAME 0xHASH <declaration or "-">. Their hash counts too.
    missing = folder / "missing-from-headers.txt"
    if missing.exists():
        for line in missing.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.split(None, 2)
            if len(parts) < 3 or not parts[1].lower().startswith("0x"):
                continue
            name, rest = parts[0].upper(), parts[2].strip()
            try:
                hashes.setdefault(name, int(parts[1], 16))
            except ValueError:
                continue
            if name in signatures or rest.startswith("-"):
                continue
            m = DECL.match("extern " + re.sub(r"/\*.*?\*/", "", rest.split("//")[0]) + ";")
            if m and m.group(2).upper() == name:
                params = m.group(3).strip()
                args = "" if params in ("", "void") else "".join(parse_param(p) for p in params.split(",") if p.strip())
                signatures[name] = (args, parse_return(m.group(1)))
    table = {}
    for name, value in sorted(hashes.items()):
        entry = {"hash": f"0x{value:08X}"}
        if name in signatures:
            entry["args"], entry["ret"] = signatures[name]
        table[name] = entry
    out.write_text(json.dumps(table, indent=2) + "\n", encoding="utf-8")
    typed = sum(1 for e in table.values() if "args" in e)
    print(f"{len(table)} natives, {typed} with a signature -> {out}")


main()
