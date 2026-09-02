#!/usr/bin/env python3
"""Verify a spire_code dylib dlopens cleanly (catches mis-aligned LINKEDIT)."""
import ctypes, sys
RTLD_NOW = 2; RTLD_LOCAL = 4
p = sys.argv[1] if len(sys.argv) > 1 else '/Users/steve/naturesense/spire/spire-code/target/release/libspire_code.dylib'
try:
    lib = ctypes.CDLL(p, mode=RTLD_NOW | RTLD_LOCAL)
    fn = getattr(lib, 'spire_send_json', None)
    print(f'OK  dylib loads, spire_send_json resolved: {fn is not None}')
    sys.exit(0)
except OSError as e:
    print(f'FAIL {p}: {e}')
    sys.exit(1)
