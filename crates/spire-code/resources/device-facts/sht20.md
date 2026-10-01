# SHT20 — protocol facts

Part number `sht20`. Family **SHT2x** (Sensirion), I²C, address `0x40` (`si7021.h:51`).

Derived from `esp-idf-lib` `components/si7021/si7021.c` (BSD-3), whose header covers
"…HTU2xD/SHT2x and compatible" sensors (`si7021.h:33`) and whose delay is commented "100 ms for
SHT20" (`si7021.c:48`). Line numbers are that file's upstream `master` — every one of them is a claim
you can check against it. This is an **implementation, not the datasheet**: enough to write a driver
that works, not enough to be authoritative. Confirm against the SHT2x datasheet.

## Commands (1 byte each unless noted)

- `0xF3` measure temperature, no-hold → 3-byte reply (`si7021.c:53`, used `:246`)
- `0xF5` measure humidity, no-hold → 3-byte reply (`si7021.c:51`, used `:257`)
- `0xFE` soft reset, no reply (`si7021.c:55`, used `:145`)
- `0xE7` read user register → 1 byte, **not CRC-checked by this driver** (`si7021.c:57`, used `:218`, a
  bare 1-byte read); `0xE6` writes it (`:56`, `:235`)
- `0x0FFA` read serial part 1 → 8 bytes (`si7021.c:60`, used `:273`); `0xC9FC` part 2 → 6 bytes (`:61`, `:280`)
- `0xB884` read firmware revision → 1 byte (`si7021.c:62`, used `:324`)
- `0xE3` / `0xE5` hold-master temperature / humidity ("not used, can't stretch clock") and `0xE0`
  read-temperature ("not used"): declared but **not used** by this driver — it only does no-hold
  (`si7021.c:50,52,54`)

## How a measurement is framed

Three phases, with a **fixed 100 ms** gap (`si7021.c:48`, applied `:99`):

1. write the 1-byte command — a bare write, no register address (`si7021.c:96`);
2. wait 100 ms;
3. read 3 bytes — a bare read, no command in the read phase (`si7021.c:103`).

So it is **two I²C transactions with a sleep between**, not one `bus_write_read`: the command and
the reply are separate exchanges, and the wait is the device's.

## Reply shape

- bytes 0–1 are the raw value, **big-endian**: `buf[0] << 8 | buf[1]` (`si7021.c:106`);
- byte 2 is the checksum over those two bytes (`si7021.c:108`);
- temperature = `raw · 175.72 / 65536 − 46.85` (`si7021.c:247`);
  humidity = `raw · 125.0 / 65536 − 6` (`si7021.c:258`).

## Checksum

CRC-8 over the **two data bytes**, seed `0`, polynomial x⁸+x⁵+x⁴+1 (`0x131`): `(value << 8) | crc`
through a 24-bit shift register, divisor `0x988000`, must reach zero (`si7021.c:75–90`). It covers
the value bytes, not the command, and a mismatch rejects the reading (`si7021.c:108–111`).

## What a bad reply looks like

- corrupt: 3 bytes arrive and the CRC disagrees → reject (`si7021.c:108–111`). Skipping this
  accepts a value that was never measured.
- short: exactly 3 bytes are read into a fixed `uint8_t[3]` (`si7021.c:102`) — anything else is the
  bus layer failing to report, so treat it as absent rather than as a value.
- absent: the read errors and `measure` returns the bus error (`si7021.c:103`). No retry and no
  timeout — the wait is unconditional.
