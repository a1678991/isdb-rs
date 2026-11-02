"""Tables the chain's HDL elaborates from (the model's sources, rebuilt in
Python; the tests check each against the vectors where they carry it).

- The B44 PRBS generators (`isdb_fec::prbs::B44Prbs`, last-stage output).
"""

PILOT_INIT = "100000000101100"  # X^15 + X^14 + 1, pilot (B44 3.6 (3))
TMCC_INIT = "100000000001110"  # X^15 + X^14 + 1, TMCC dispersal (3.6 (2))
SLOT_INIT = "1010000000000000000011010"  # X^25 + X^22 + 1, slot dispersal (3.6 (1))


def prbs(init, a, n, length):
    """`length` output bits of the B44 generator (`Lfsr::next_bit_last_stage`):
    the register with stage i + 1 in bit i, out = stage n, feedback = stage a
    xor stage n into stage 1."""
    reg = lfsr_state(init)
    out = []
    for _ in range(length):
        out.append((reg >> (n - 1)) & 1)
        fb = ((reg >> (a - 1)) ^ (reg >> (n - 1))) & 1
        reg = ((reg << 1) | fb) & ((1 << n) - 1)
    return out


def pilot_signs():
    """`isdbs3_core::dispersal::pilot_signs`: +1 / -1 per pilot symbol of a
    frame (32 per slot, 120 slots)."""
    return [-1 if b else 1 for b in prbs(PILOT_INIT, 14, 15, 120 * 32)]


def lfsr_state(init):
    """The generator's register as an integer, stage 1 in bit 0 (the form
    the HDL's shift registers start from)."""
    return sum(int(c) << i for i, c in enumerate(init))


# --- the constellations (isdbs3_core::constellation, params) ---------------

RATES = [41, 49, 61, 73, 81, 89, 93, 97, 101, 105, 109]  # TMCC rate code = index + 1
MOD_BITS = {1: 1, 2: 2, 3: 3, 4: 4, 5: 5}  # TMCC modulation code -> bits per symbol
GAMMA16 = {41: 3.09, 49: 2.97, 61: 3.93, 73: 2.87, 81: 2.92, 89: 2.97, 93: 2.87, 97: 2.73,
           101: 2.67, 105: 2.76, 109: 2.69}
GAMMA32 = {41: 6.53, 49: 7.17, 61: 8.03, 73: 5.61, 81: 5.68, 89: 5.57, 93: 5.33, 97: 5.05,
           101: 4.80, 105: 4.82, 109: 4.66}
QPSK = [(0b00, 45.0), (0b10, 135.0), (0b11, 225.0), (0b01, 315.0)]
PSK8 = [(0b001, 0.0), (0b000, 45.0), (0b100, 90.0), (0b110, 135.0), (0b010, 180.0),
        (0b011, 225.0), (0b111, 270.0), (0b101, 315.0)]
APSK16_R1 = [(0b1100, 45.0), (0b1110, 135.0), (0b1111, 225.0), (0b1101, 315.0)]
APSK16_R2 = [(0b0100, 15.0), (0b0000, 45.0), (0b1000, 75.0), (0b1010, 105.0), (0b0010, 135.0),
             (0b0110, 165.0), (0b0111, 195.0), (0b0011, 225.0), (0b1011, 255.0),
             (0b1001, 285.0), (0b0001, 315.0), (0b0101, 345.0)]
APSK32_R1 = [(0b10001, 45.0), (0b10101, 135.0), (0b10111, 225.0), (0b10011, 315.0)]
APSK32_R2 = [(0b10000, 15.0), (0b00000, 45.0), (0b00001, 75.0), (0b00101, 105.0),
             (0b00100, 135.0), (0b10100, 165.0), (0b10110, 195.0), (0b00110, 225.0),
             (0b00111, 255.0), (0b00011, 285.0), (0b00010, 315.0), (0b10010, 345.0)]
APSK32_R3 = [(0b11000, 0.0), (0b01000, 22.5), (0b11001, 45.0), (0b01001, 67.5), (0b01101, 90.0),
             (0b11101, 112.5), (0b01100, 135.0), (0b11100, 157.5), (0b11110, 180.0),
             (0b01110, 202.5), (0b11111, 225.0), (0b01111, 247.5), (0b01011, 270.0),
             (0b11011, 292.5), (0b01010, 315.0), (0b11010, 337.5)]


def nominal(mod, rate_code):
    """`chain::demap::nominal`: the points of modulation code `mod` at rate
    code `rate_code` in Q.10, label order (round(1 024 p), Rust's rounding)."""
    import math

    from .common import round_half_away

    r = RATES[rate_code - 1]

    def deg(rad, d):
        a = d * math.pi / 180.0
        return (rad * math.cos(a), rad * math.sin(a))

    p = [(0.0, 0.0)] * (1 << MOD_BITS[mod])
    if mod == 1:
        p = [deg(1.0, 45.0), deg(1.0, 225.0)]
    elif mod == 2:
        for lab, d in QPSK:
            p[lab] = deg(1.0, d)
    elif mod == 3:
        for lab, d in PSK8:
            p[lab] = deg(1.0, d)
    elif mod == 4:
        for lab, d in APSK16_R1:
            p[lab] = deg(1.0, d)
        for lab, d in APSK16_R2:
            p[lab] = deg(GAMMA16[r], d)
    else:
        for lab, d in APSK32_R1:
            p[lab] = deg(1.0, d)
        for lab, d in APSK32_R2:
            p[lab] = deg(GAMMA16[r], d)
        for lab, d in APSK32_R3:
            p[lab] = deg(GAMMA32[r], d)
    pw = sum(i * i + q * q for i, q in p) / len(p)
    s = math.sqrt(pw)
    return [(round_half_away(i / s * 1024), round_half_away(q / s * 1024)) for i, q in p]


def nominal_rom():
    """Every mode's nominal points in one table: entry ((mod - 1) * 16 +
    (rate - 1)) * 32 + label (mod 1..5, rate code 1..11), (re, im); unused
    entries 0."""
    rom = []
    for mod in range(1, 6):
        for rc in range(1, 17):
            pts = nominal(mod, rc) if rc <= 11 else []
            rom += pts + [(0, 0)] * (32 - len(pts))
    return rom
