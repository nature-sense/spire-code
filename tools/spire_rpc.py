#!/usr/bin/env python3
"""Drive a JSON RPC through the spire_code dylib from the shell.

The first call triggers init_actor_system() (full actor boot + global LLM config
from ~/.spire/llm-config.json) — same as the app. Subsequent calls route fast.

Usage: spire_rpc.py <method> <params-json> [dylib-path]
Example:
  tools/spire_rpc.py createProject/GenerateSpec \
      '{"projectName":"spire-notes","goal":"quick capture and search of notes"}'

Prints the JSON reply. Exit code 0 on a reply without an "error" key.
"""
import ctypes
import json
import sys
import time

RTLD_NOW = 2
RTLD_LOCAL = 4


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    method = sys.argv[1]
    try:
        params = json.loads(sys.argv[2])
    except json.JSONDecodeError as e:
        print(f"params not JSON: {e}")
        return 2
    dylib = sys.argv[3] if len(sys.argv) > 3 else (
        "/Users/steve/naturesense/spire/spire-code/target/release/libspire_code.dylib"
    )

    lib = ctypes.CDLL(dylib, mode=RTLD_NOW | RTLD_LOCAL)
    send = lib.spire_send_json
    send.restype = ctypes.c_void_p
    send.argtypes = [ctypes.c_char_p]
    free = lib.spire_free_string
    free.restype = None
    free.argtypes = [ctypes.c_void_p]

    body = json.dumps({"method": method, "params": params})
    print(f"[spire_rpc] {method} (len={len(body)}) — {time.strftime('%H:%M:%S')}")
    started = time.time()
    raw = send(body.encode("utf-8"))
    reply = ctypes.string_at(raw).decode("utf-8")
    free(raw)
    elapsed = time.time() - started
    print(f"[spire_rpc] reply ({elapsed:.1f}s):")
    print(reply)

    try:
        obj = json.loads(reply)
    except json.JSONDecodeError:
        return 1
    return 1 if isinstance(obj, dict) and "error" in obj else 0


if __name__ == "__main__":
    sys.exit(main())
