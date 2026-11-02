"""The interface between the chain and the LDPC decoder (stage 7, written
on another branch as `isdb_fpga/ldpc.py`; plan piece 2). The chain drives
the input side and consumes the result side; both are in the chain's
clock domain (the symbol clock, `util_ad9361_divclk`, 40 MHz). The decoder
runs in its own faster domain (100 or 200 MHz) behind asynchronous FIFOs
on both sides, which belong to the decoder's wrapper: every crossing is a
FIFO, nothing else crosses.

**Input (chain -> decoder)**, one beat per clock when `in_valid & in_ready`:

- `in_op`: OP_BEGIN opens the codeword buffer of context `in_ctx` (0: the
  slot codewords, 1: the frame's TMCC codeword); OP_WRITE writes `in_n`
  (1 .. 5) LLRs, lane l at codeword position `in_pos[l]` (< 44 880) with
  value `in_llr[l]` (i8, -127 .. 127) into the open buffer of `in_ctx`;
  OP_DONE closes it (a write beat may also carry `in_first`, which opens
  the buffer first, and `in_last`, which closes it after: the demapper's
  form, one beat per main symbol with no beat to spare at a codeword's
  ends; the TMCC burst uses OP_BEGIN and OP_DONE): decode as rate `in_rate` (numerator r of r/120; 61
  for the TMCC), at most `in_max_iter` iterations, offset `in_offset`,
  `in_shortened` (the TMCC's shortened 61/120 word: positions 0 .. 1 869
  and 11 484 .. 22 813 are known zeros the decoder fills with +127 itself;
  they are never written), with the 32-bit `in_tag` returned with the
  result (bits 0-6 the slot index, bit 7 TMCC, bits 8-15 the frame
  counter's low byte, 16-19 the rate code, 20-23 the modulation code,
  24-26 the transmission mode: what the finish stage needs of the slot).
- A slot codeword is written by the demapper, one beat per main symbol
  (its 1 .. 5 label bits at their de-interleaved positions); the TMCC
  codeword is burst from the TMCC stage's staging buffer (5 positions per
  beat) on the cycles the demapper leaves free. The two contexts share the
  port cycle by cycle; the decoder keeps one open buffer per context.
  Every position of a slot codeword is written exactly once; the TMCC's
  31 680 sent positions likewise.

**Result (decoder -> chain)**, in the order the codewords were closed
(the decoder's queue; plan stage 7: a result depends only on the
codeword, the code, the schedule and the limit):

- a header beat (`out_head` high) with `out_tag`, `out_iterations` (the
  limit when not converged) and `out_converged`;
- then the hard decisions of the first k positions (k = 374 r; for the
  TMCC, 22 814) in position order, 16 per beat (`out_bits`, bit b =
  position 16 q + b; the last beat zero-padded), `out_last` on the last;
- `out_ready` from the chain (the finish stage may stall the stream).

**The stub** (`LdpcStub` in the simulations, `stages.py`): the chain's
vectors carry the decoder's results (each slot's iterations and the
decoded slot bits in the `Slots` words, the TMCC's iterations and
information bits), not its output bits, so the stub rebuilds them: for a
converged codeword with no BCH correction (all of the synthetic sets) the
decoder's first k hard decisions are the transmitted ones, i.e. the
header and data bits the vectors give, BCH-encoded (gf.encode), with the
six stuffing ones, then dispersed (the slot PRBS across the frame's valid
slots; for the TMCC the TMCC PRBS over its 9 614 data bits, the known
zeros 0). It checks the input side against the vectors where they carry
it (chain-4: every TMCC position and value; chain-5: every LLR of two
slots and their positions) and answers each OP_DONE with that header and
those bits, in order. It is a stand-in for the decoder's output, not a
model of the decoder: a codeword the stub cannot rebuild (not converged,
or BCH-corrected) is reported as a gap in the comparison.
"""

from amaranth.hdl import Signal, signed

OP_BEGIN, OP_WRITE, OP_DONE = 0, 1, 2
LANES = 5
TAG_TMCC = 1 << 7
TMCC_RATE_CODE = 3  # 61/120


class LdpcPort:
    """The signals of both sides (the chain's view)."""

    def __init__(self, name="ldpc"):
        self.in_valid = Signal(name=f"{name}_in_valid")
        self.in_ready = Signal(name=f"{name}_in_ready", init=1)
        self.in_op = Signal(2, name=f"{name}_in_op")
        self.in_ctx = Signal(name=f"{name}_in_ctx")
        self.in_n = Signal(range(LANES + 1), name=f"{name}_in_n")
        self.in_pos = [Signal(16, name=f"{name}_in_pos{k}") for k in range(LANES)]
        self.in_llr = [Signal(signed(8), name=f"{name}_in_llr{k}") for k in range(LANES)]
        self.in_rate = Signal(7, name=f"{name}_in_rate")
        self.in_max_iter = Signal(8, name=f"{name}_in_max_iter")
        self.in_offset = Signal(3, name=f"{name}_in_offset")
        self.in_shortened = Signal(name=f"{name}_in_shortened")
        self.in_first = Signal(name=f"{name}_in_first")
        self.in_last = Signal(name=f"{name}_in_last")
        self.in_tag = Signal(32, name=f"{name}_in_tag")
        self.out_valid = Signal(name=f"{name}_out_valid")
        self.out_ready = Signal(name=f"{name}_out_ready")
        self.out_head = Signal(name=f"{name}_out_head")
        self.out_tag = Signal(32, name=f"{name}_out_tag")
        self.out_iterations = Signal(8, name=f"{name}_out_iterations")
        self.out_converged = Signal(name=f"{name}_out_converged")
        self.out_bits = Signal(16, name=f"{name}_out_bits")
        self.out_last = Signal(name=f"{name}_out_last")


def tmcc_position(b):
    """`chain::tmcc::word_position`: TMCC signal bit b's position in the
    shortened word."""
    return b + 1870 if b < 9614 else b + 13200
