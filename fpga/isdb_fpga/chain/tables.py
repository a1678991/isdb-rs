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
