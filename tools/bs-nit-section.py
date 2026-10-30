#!/usr/bin/env python3
"""A BS-shaped NIT actual section (ARIB STD-B10 Part 2 §5.2.4 Table 5-3) as
hex, for `[nit] replace_sections` of an isdbt-mux configuration: the interim
for docs/plans/isdbs-next/bs-si.md until the multiplexer generates it.

Network loop: network_name 0x40 (§6.2.11), system_management 0xFE
(§6.2.21: broadcasting_flag '00', broadcasting_identifier '000010' Table 6-36
"wide band digital satellite broadcasting ... 11.7 to 12.2 GHz",
additional_broadcasting_identification 0x01). TS loop: service_list 0x41
(§6.2.14) then satellite_delivery_system 0x43 (§6.2.6 Tables 6-8..6-11):
frequency BCD of BS-n (11 727.48 + 19.18 (n - 1) MHz, SPECS.md §1.2),
orbital position 110.0 E, polarization from the channel's parity (odd
right-hand '11', even left-hand '10'), modulation '01000' and FEC_inner
'1000' ("refer to TMCC signal"), symbol rate 028.8600 Msym/s (ordinance
Art. 52(3)). The loop order and these values match a 2023 BS recording.

Usage: tools/bs-nit-section.py NAME_HEX [--channel 1] [--tsid 0x4017]
           [--nid 0x0004] [--service 0x03E7:0x01 ...] [--version 0]
NAME_HEX is the ARIB 8-unit coded network name (e.g. from `isdb ts extract`
of a NIT the multiplexer made). Placeholder identities only (CONTRIBUTING).
"""
import argparse


def crc32_mpeg(data: bytes) -> int:
    c = 0xFFFFFFFF
    for b in data:
        c ^= b << 24
        for _ in range(8):
            c = ((c << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if c & 0x80000000 else (c << 1) & 0xFFFFFFFF
    return c


def bcd(value: int, digits: int) -> int:
    s = f"{value:0{digits}d}"
    if len(s) != digits:
        raise ValueError(f"{value} does not fit {digits} BCD digits")
    return int(s, 16)


def desc(tag: int, body: bytes) -> bytes:
    return bytes([tag, len(body)]) + body


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("name_hex")
    p.add_argument("--channel", type=int, default=1, help="BS channel n (1..24)")
    p.add_argument("--tsid", type=lambda s: int(s, 0), default=0x4017)
    p.add_argument("--nid", type=lambda s: int(s, 0), default=0x0004)
    p.add_argument("--service", action="append", default=[], help="SID:TYPE, e.g. 0x03E7:0x01")
    p.add_argument("--version", type=int, default=0)
    a = p.parse_args()
    if not 1 <= a.channel <= 24:
        raise SystemExit("--channel: 1..24")
    services = a.service or ["0x03E7:0x01"]
    freq_10khz = 1_172_748 + 1_918 * (a.channel - 1)  # 10 kHz units
    pol = 0b11 if a.channel % 2 else 0b10
    sat = (
        bcd(freq_10khz, 8).to_bytes(4, "big")
        + bcd(1100, 4).to_bytes(2, "big")  # 110.0 degrees
        + bytes([(1 << 7) | (pol << 5) | 0b01000])  # east, polarization, modulation
        + ((bcd(288_600, 7) << 4) | 0b1000).to_bytes(4, "big")  # 028.8600 Msym/s, FEC
    )
    svc = b"".join(
        int(s, 0).to_bytes(2, "big") + bytes([int(t, 0)])
        for s, t in (x.split(":") for x in services)
    )
    net = desc(0x40, bytes.fromhex(a.name_hex)) + desc(0xFE, bytes([0x02, 0x01]))
    ts = desc(0x41, svc) + desc(0x43, sat)
    tsloop = a.tsid.to_bytes(2, "big") + a.nid.to_bytes(2, "big") + (0xF000 | len(ts)).to_bytes(2, "big") + ts
    body = (
        a.nid.to_bytes(2, "big")
        + bytes([0xC0 | (a.version & 31) << 1 | 1, 0, 0])
        + (0xF000 | len(net)).to_bytes(2, "big")
        + net
        + (0xF000 | len(tsloop)).to_bytes(2, "big")
        + tsloop
    )
    n = len(body) + 4
    sec = bytes([0x40, 0xF0 | (n >> 8), n & 0xFF]) + body
    print((sec + crc32_mpeg(sec).to_bytes(4, "big")).hex())


if __name__ == "__main__":
    main()
