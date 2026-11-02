"""The `Slots` format, version 1 (`isdb_frontend::slots`), in Python: a
reader of the stream into blocks (for the comparisons and the LDPC stub)
and the payload layouts the packer (`packer.py`) produces.
"""

SYNC = (0x1ACF, 0xFC1D)
HEADER_WORDS = 9
SLOT, TMCC, STATUS = 1, 2, 3
RATES = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109]


def data_bits(rate_code):
    return 374 * RATES[rate_code - 1] - 176 - 192 - 6


def crc32c(words, seq):
    crc = 0xFFFFFFFF

    def byte(c, b):
        c ^= b
        for _ in range(8):
            c = (c >> 1) ^ (0x82F63B78 if c & 1 else 0)
        return c

    for b in seq.to_bytes(4, "little"):
        crc = byte(crc, b)
    for w in words:
        crc = byte(crc, w & 0xFF)
        crc = byte(crc, w >> 8)
    return crc ^ 0xFFFFFFFF


def header(kind, seq, payload, frame):
    w = [SYNC[0], SYNC[1], 0xF | kind << 4 | 1 << 8, seq & 0xFFFF, seq >> 16, payload,
         frame & 0xFFFF, frame >> 16 & 0xFFFF, 0]
    w[8] = (0xFFFF - sum(w[2:8])) & 0xFFFF
    return w


def blocks(words):
    """[(kind, seq, frame, payload words)] of a clean stream (asserts)."""
    out = []
    i = 0
    while i < len(words):
        w = words[i:i + HEADER_WORDS]
        assert tuple(w[:2]) == SYNC, i
        kind = (w[2] >> 4) & 0xF
        seq = w[3] | w[4] << 16
        p = w[5]
        frame = w[6] | w[7] << 16
        assert w == header(kind, seq, p, frame), i
        pay = words[i + HEADER_WORDS:i + HEADER_WORDS + p]
        crc = crc32c(pay, seq)
        tr = words[i + HEADER_WORDS + p:i + HEADER_WORDS + p + 2]
        assert tr == [crc & 0xFFFF, crc >> 16], i
        out.append((kind, seq, frame, pay))
        i += HEADER_WORDS + p + 2
    return out


def unpack_bits(ws, n):
    return [(ws[i // 16] >> (i % 16)) & 1 for i in range(n)]


def slot_fields(pay):
    """A slot block's fields and its bits."""
    w0, w1, w2 = pay[:3]
    f = {"slot": w0 & 0xFF, "mode": w0 >> 8 & 7, "dummy": w0 >> 11 & 1, "converged": w0 >> 12 & 1,
         "bch_ok": w0 >> 13 & 1, "mod": w1 & 0xF, "rate": w1 >> 4 & 0xF, "iterations": w1 >> 8,
         "bch": w2}
    n = 0 if f["dummy"] else 176 + data_bits(f["rate"])
    f["bits"] = unpack_bits(pay[3:], n)
    return f
