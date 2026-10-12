# SPS30 — protocol facts

Part number `sps30`. Sensirion particulate sensor, **I²C**, address `0x69` (`SPS30_I2C_ADDR_69`,
`sps30_i2c.h`). It speaks UART too, but on a bus it is I²C — and it is not the **SDS011** (Nova), which is
**UART only** with no I²C address, and so cannot be declared on an I²C bus.

Derived from Sensirion's driver `embedded-i2c-sps30` (`sps30_i2c.c`/`.h`; BSD-3, upstream `main`),
symbols cited. An **implementation, not the datasheet**.

## Commands (16-bit, MSB first)

A command is a 16-bit word, high byte first (`sensirion_i2c_fill_cmd_send_buf`); the `SPS30_CMD_ID` set:

- `0x0010` start measurement, then a format word: `0x0300` float (`768`) or `0x0500` uint16 (`1280`);
- `0x0104` stop; `0x0202` data-ready flag → 1 word (`0` none, `1` ready);
- `0x0300` read measurement values — one command for both formats, the format fixed at start;
- `0x1001` sleep, `0x1103` wake-up; `0x5607` fan cleaning; `0x8004` auto-clean interval (s);
- `0xD002` product type, `0xD033` serial, `0xD100` firmware version, `0xD206` device status, `0xD210`
  clear it, `0xD304` reset (idle-mode only; from sleep, wake up first).

## How a command is framed

One write, with a CRC byte after **each 16-bit word** (`sensirion_i2c_fill_cmd_send_buf`): command word
MSB first, then its CRC, each argument word the same. A read is a **separate** exchange after that write
(`sensirion_i2c_read_data_inplace`), not one combined write-then-read.

Delays after a write, in **ms**: start **20**, stop **20**, clear status **5**, reset **100**.

## The wake-up is a *sequence*

`sps30_wake_up_sequence` sends wake-up **twice** — a wake-up to an awake device is ignored and the first
pulse may be swallowed, so one is not enough.

## Reply shape

Each 16-bit word is followed by its CRC; `sensirion_i2c_read_data_inplace` reads words **with** their
CRCs, verifies each, and compacts in place — the caller holds plain big-endian words.

- `0x0300` **float**: **60 bytes** on the wire → ten big-endian IEEE-754 floats (40 data + 20 CRC;
  `sensirion_common_bytes_to_float` = `b0<<24|b1<<16|b2<<8|b3`), in order — mass `PM1.0`, `PM2.5`,
  `PM4.0`, `PM10` [µg/m³]; number `NC0.5`…`NC10` [#/cm³]; typical particle size [µm]. A **PM2.5** meter
  wants `float[1]`, not the first float;
- `0x0300` **uint16**: 10 words, **30 bytes** — the same ten values, scaled.

## Checksum

CRC-8 over each 2-byte word, MSB-first (`sensirion_i2c_generate_crc`): polynomial `0x31` (x⁸+x⁵+x⁴+1),
init `0xFF`, no final XOR, the CRC after its word. A disagreeing CRC rejects the reply (`CRC_ERROR`). The
same Sensirion CRC-8 the SHT family uses — only the words around it differ.

## What a bad reply looks like

- corrupt: a word's CRC disagrees → the read errors, rejecting the reply;
- absent: the write or read fails at the bus → treat the sensor as absent, not as a zero reading;
- not ready: the data-ready flag reads `0` → no new frame yet; the driver does not retry.


