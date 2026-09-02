#!/usr/bin/env python3
"""Runtime FFI round-trip through a generated SpireApp core dylib.

Calls `spire_send_json` with JSON envelopes and prints the replies, proving
the bridge-derived dispatch routes known methods to their actor and reports
unknown/bad input without panicking.

Usage: spire_ffi_roundtrip.py <libspire_*.dylib>
"""
import ctypes
import json
import sys

RTLD_NOW = 2
RTLD_LOCAL = 4

p = sys.argv[1] if len(sys.argv) > 1 else "/Users/steve/naturesense/spire/spire-gis/target/debug/libspire_gis.dylib"
lib = ctypes.CDLL(p, mode=RTLD_NOW | RTLD_LOCAL)

send = lib.spire_send_json
send.restype = ctypes.c_void_p
send.argtypes = [ctypes.c_char_p]
free = lib.spire_free_string
free.restype = None
free.argtypes = [ctypes.c_void_p]


def call(method, params=None):
    body = json.dumps({"method": method, "params": params or {}})
    raw = send(body.encode("utf-8"))
    out = ctypes.string_at(raw).decode("utf-8")
    free(raw)
    return out


ok = True
for label, expected in [
    ("known method routes to MapActor", {"ok": True, "result": None}),
    ("unknown method reports error", {"ok": False, "error": "unknown method"}),
]:
    reply = json.loads(call("map/listLayers" if "routes" in label else "map/doesNotExist"))
    print(f"{'PASS' if reply == expected else 'FAIL'}  {label}: {reply}")
    ok = ok and reply == expected

# Malformed JSON must produce a clean error, not a crash.
raw = lib.spire_send_json(b"{not json")
reply = ctypes.string_at(raw).decode("utf-8")
lib.spire_free_string(raw)
print(f"{'PASS' if 'bad request' in reply else 'FAIL'}  malformed JSON handled: {reply}")
ok = ok and "bad request" in reply

sys.exit(0 if ok else 1)
