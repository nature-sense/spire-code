# SHT30 — protocol facts

Part number `sht30`. Family **SHT3x** (Sensirion), I²C, addresses `0x44` (ADDR to GND) and `0x45`
(ADDR to VDD) (`sht3x.h:54,55`). The driver names only the **family** — SHT30, SHT31, SHT35 share this
protocol, and no member appears in it (`sht3x.h:34`) — so the address, not the name, is what separates
the part from an SHT2x at `0x40`.

Derived from `esp-idf-lib` `components/sht3x/sht3x.c`/`.h` (BSD-3), upstream `master`; each line number
is a checkable claim. An **implementation, not the datasheet**.

## Commands (16-bit, swapped onto the wire)

- measure, `SHT3X_MEASURE_CMD[mode][repeat]` (`sht3x.c:61–68`, sent `:142`, single-shot `:267`):
  single-shot **without clock stretching** `0x2400`/`0x240B`/`0x2416` high/medium/low (`:62`), and
  periodic 0.5/1/2/4/10 per second (`:63–67`);
- `0xE000` fetch data (`:56`, `:177`);
- `0x3041` clear status (`:54`, `:237`); `0x3093` stop periodic (`:57`, `:295`); `0x306D`/`0x3066`
  heater on/off (`:58,59`, `:244`);
- `0xF32D` read status (`:53`) and `0x30A2` soft reset (`:55`) are **declared, never used**, and no
  serial-number command exists at all — unlike SHT2x (`0x0FFA`, `0xC9FC`) this driver reads no ID.

## How a measurement is framed

The command is **two bytes**, swapped by `shuffle` (`sht3x.c:98–101`) and written with no register
address (`:122–127`):

1. write the measure command;
2. wait **15 ms** high / 6 ms medium / 4 ms low (`:75–77`), rounded **up** to a whole RTOS tick (`:73`);
3. read 6 bytes, and *not* with a bare read: `0xE000` is written and the 6 bytes read in the **same**
   transfer (`i2c_dev_read`, `:177–178`). An SHT2x driver's one command byte and bare 3-byte read are
   not a smaller version of this.

## Reply shape

Six bytes (`SHT3X_RAW_DATA_SIZE`, `sht3x.h:57`): 0–1 temperature, **big-endian** `raw[0]·256 + raw[1]`,
byte 2 its CRC; 3–4 humidity, big-endian, byte 5 its CRC (`:252`, `:255`). Temperature =
`raw·175/65535 − 45`, humidity = `raw·100/65535` — **65535**, where SHT2x divides by 65536 and uses
−46.85.

## Checksum

CRC-8 over each **two-byte** half, bit by bit, MSB first: seed `0xff`, polynomial `0x31` (x⁸+x⁵+x⁴+1)
(`sht3x.c:96`, `:103–120`). Bytes 0–1 are checked against byte 2, 3–4 against byte 5 (`:188`, `:195`).
**The seed is `0xff`, not 0** — the polynomial is an SHT2x driver's, the initial value is not, so a CRC
routine lifted from `si7021.c` rejects every frame here.

## What a bad reply looks like

- corrupt: either CRC disagrees → `ESP_ERR_INVALID_CRC` (`:190–198`) — skipping the check accepts a
  value never measured;
- early: the fetch runs before the wait elapsed → `ESP_ERR_INVALID_STATE`, "Measurement is still
  running" (`:170–174`), and the same error before any start (`:165–169`);
- short: the read fills a fixed `uint8_t[6]` (`sht3x.h:57`); anything else is the bus layer failing to
  report, so treat it as absent. No retry, no timeout.
