"""ISDB-S transmit block (`SymbolsTx`): 4-bit symbol codes from the TX DMA
in, root-raised-cosine shaped 12-bit I/Q at the DAC rate out.

The FPGA implementation of the transmit interpolator's fixed-point model
(`isdb_frontend::shape::fixed`, Rust; the contract in
docs/plans/isdbs-fpga-tx.md §2 and interpolator.md §4). It must reproduce
the model bit for bit.

Codes. 4 bits per symbol: 0 = the zero symbol (silence); 8 + k = the point
e^{jk*pi/4}, k = 0..7; 1..7 are reserved and treated as the zero symbol.
One DMA sample is the two 16-bit words `util_upack2` delivers per
`fifo_rd_en` (I word = codes 0..3, Q word = codes 4..7, code 0 in bits 3..0
of the I word and first in time).

Per output sample, in this order (the model's definition):
  1. q = frac[23:15]; for each tap t of the line whose code is a point k:
     v = b[q][t] (k odd) or a[q][t] (k even); I += sI[k]*v, Q += sQ[k]*v
     (sI = [1, 1, 0, -1, -1, -1, 0, 1], sQ = [0, 1, 1, 1, 0, -1, -1, -1]);
     out = clamp(round_shift(sum, SHIFT), -2048, 2047), add half then
     arithmetic shift; the DAC word is out << 4.
  2. frac += step; on carry: frac -= 2**24, the line (newest at 0) shifts
     and the next code enters.
START clears frac, the line (all zero codes), the counters and the unpack
register; there is no forced first wrap, so sample 0 is 0.

The bank. Rows q at (q + 1/2)/P, 2S taps, a for even k and b = a/sqrt2 for
odd k, and exactly symmetric: a[q][t] == a[P-1-q][2S-1-t]. So one
true-dual-port RAMB18 (2P x 18 = 1024 x 18 at P 512) per tap pair (t,
u = 2S-1-t) holds a and b of tap t for every row, and tap u reads the same
memory mirrored: port A reads {k_t & 1, q} (it is also the load port),
port B reads {k_u & 1, ~q} (~q = P-1-q). 12 RAMB18 for span 12.

Timing: one output sample per request strobe `req` (util_rfifo's
`din_valid_0`: bursts of eight consecutive strobes that average the DAC
rate; on the PlutoSky R2 the block's clock, util_ad9361_divclk, is the
sample rate, 40 MHz at 40 Msps, so the strobe is high on nearly every
clock). The whole datapath is clock-enabled by `en` = `req` while running
(and once the first DMA word is in), as txfir's filter runs on
`dac_valid`. "Dk" is k enabled clocks after D0, the clock that computes
sample n's update:

  D0  frac + step (24 bits and carry) and, on carry, the line shift and the
      next code from the unpack register; the counters. Registered for D1:
      q = frac[23:15] and a snapshot of the 2S codes (sample n's state)
  D1  BRAM addresses {k_t & 1, q} (port A) and {k_u & 1, ~q} (port B);
      read (synchronous), the data in its register at D2
  D2  terms: per tap and component v, ~v (= -v - 1) or 0 by the code
      (18 bits, one LUT per bit, no carry chain per tap); and the count
      of the negated terms, from the D1 snapshot in two stages, which adds
      their +1s back as one more leaf of the tree
  D3..D7  adder trees 2S + 1 -> 1 (25 -> 1 in five levels, each level as
          wide as its sums can be, 24 bits at the root, use_dsp = "no")
  D8  (sum << 1) >> SHIFT (arithmetic; a 25-bit barrel shift)
  D9  ((that + 1) >> 1) = round_shift(sum, SHIFT), saturated to 12 bits,
      in the output register: sample n leaves LATENCY = 9 enabled clocks
      after its D0 clock

Nothing feeds back, so the pipeline needs no loop delay; its latency is
constant and invisible to the signal. After START the output stays 0 until
the first sample computed after it arrives (`vld`), so a restart replays
nothing stale; while not running it is 0.

Codes come from an 8-code unpack register refilled from the DMA one word
per 8 symbols, with one staging word behind it: the block keeps two words
held or requested (`fifo_rd_en`, one clock pulse per word, any clock) and
takes the responses in order whenever they come back (util_upack2 answers
every request after a fixed latency, one clock at its default
PIPELINE_STAGES 0, with either `fifo_rd_valid` = a word or
`fifo_rd_underflow` = no word, its data zeroed). A word lasts at least 8
enabled clocks (at most one symbol per sample), so the refill is always
back before the unpack register empties: the symbol cadence never waits on
the DMA. A missing word (underflow) is a word of 8 zero symbols: when its
first symbol enters the line, MISSED += 8, FIRST_MISS latches CONSUMED at
the first miss and the sticky underrun flag is set; CONSUMED counts the
symbols entered from delivered words. Counting at entry makes the counters
a function of the sample count alone, independent of the prefetch timing.
Requests start ARM clocks after the block becomes active, so a response to
a request of the pass-through (the rfifo reads the upack directly while the
block is not selected) is never taken for the block's own; the datapath
starts when the first word (or miss) is in the unpack register.
"""

import math

from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

# Constants of the block (TX format version 1).
TX_ID_MAGIC = 0xB5D1
TX_VERSION = 1
FRAC_BITS = 24
COEF_BITS = 18
OUT_BITS = 12
SUM_BITS = 24
SHIFT_BITS = 5
TERM_BITS = COEF_BITS  # v or ~v: ~v of an 18-bit value is an 18-bit value
CODES_PER_WORD = 8
ARM = 4  # clocks between becoming active and the first DMA request
SI = (1, 1, 0, -1, -1, -1, 0, 1)
SQ = (0, 1, 1, 1, 0, -1, -1, -1)
# Vivado synthesis attribute: map an adder to fabric, not a DSP48E1.
NO_DSP = {"use_dsp": "no"}


def latency(taps):
    """Enabled clocks from a sample's D0 clock to the output register: the
    BRAM read, the terms, the adder tree (taps + 1 leaves: the terms and
    the count of negated ones; ceil(log2(taps + 1)) levels), the shift and
    the rounding/saturation register (9 at 24 taps)."""
    return 4 + math.ceil(math.log2(taps + 1))


LATENCY = latency(24)


def id_word(phases, span):
    """The ID word on `gpio_in`: magic, format version, log2 of the
    phases, taps (0xB5D11918 at P 512, span 12)."""
    return TX_ID_MAGIC << 16 | TX_VERSION << 12 | (phases.bit_length() - 1) << 8 | (2 * span)


def check_bank(name, bank, phases, span):
    """Refuse a bank the block cannot hold: P rows of 2S signed 18-bit
    coefficients, exactly symmetric (the block stores one copy of each tap
    pair), as the model's `validate` does."""
    w = 2 * span
    if len(bank) != phases or any(len(row) != w for row in bank):
        raise ValueError(f"bank {name}: want {phases} rows of {w} taps")
    lim = 1 << (COEF_BITS - 1)
    for q, row in enumerate(bank):
        for t, c in enumerate(row):
            if not -lim <= c < lim:
                raise ValueError(f"bank {name}[{q}][{t}] = {c} is not a signed {COEF_BITS}-bit value")
            m = bank[phases - 1 - q][w - 1 - t]
            if c != m:
                raise ValueError(
                    f"bank {name} is not symmetric: [{q}][{t}] = {c}, "
                    f"[{phases - 1 - q}][{w - 1 - t}] = {m}"
                )


class TxRegisters:
    """The run-time parameters (`TxParams`: step and shift) as signals; the
    banks are written through the core's load port."""

    def __init__(self):
        self.step = Signal(FRAC_BITS)
        self.shift = Signal(SHIFT_BITS)

    def fields(self):
        return {k: v for k, v in vars(self).items() if isinstance(v, Signal)}


class SymbolTx(Elaboratable):
    """Symbol codes from the DMA unpacker in, 12-bit I/Q out.

    Ports:
      start                  one-cycle pulse: reset frac, the line, the
                             unpack register and the counters;
      run, selected          from the control (the datapath and the DMA
                             requests are active while both are set);
      req                    one output sample per strobe;
      rd_en                  DMA word request (util_upack2 fifo_rd_en);
      rd_data, rd_valid, rd_underflow
                             the response: Cat(I word, Q word), a word or
                             no word (in order, any fixed latency);
      o_i, o_q               the output sample (registered, LATENCY enabled
                             clocks after its D0 clock);
      c_we, c_bank, c_q, c_t, c_data
                             bank load (one coefficient per cycle: bank 0 =
                             a, 1 = b; any tap t < 2S, a far tap t >= S is
                             stored as its mirror [P-1-q][2S-1-t]);
      consumed, missed, first_miss, samples, underrun
                             the counters and the sticky flag;
      en, frac, line, wrap, code, s_sum_i, s_sum_q
                             verification: the clock enable, sample n's
                             state in D0 (`wrap`, `code`: the carry of its
                             update and the code that enters), and its
                             pre-round sums aligned with o_i, o_q.
    """

    def __init__(self, phases, span, a=None, b=None):
        assert phases & (phases - 1) == 0 and 2 <= phases <= 512, "P: a power of two up to 512"
        assert 1 <= span and 2 * span <= 32, "2S taps: at most 32 (the address has 5 tap bits)"
        self.phases = phases
        self.span = span
        self.taps = 2 * span
        self.pbits = phases.bit_length() - 1
        self.latency = latency(self.taps)
        if (a is None) != (b is None):
            raise ValueError("give both banks or neither")
        if a is not None:
            check_bank("a", a, phases, span)
            check_bank("b", b, phases, span)
        self.a = a
        self.b = b
        self.regs = TxRegisters()
        self.start = Signal()
        self.run = Signal()
        self.selected = Signal()
        self.req = Signal()
        self.rd_en = Signal()
        self.rd_data = Signal(32)
        self.rd_valid = Signal()
        self.rd_underflow = Signal()
        self.o_i = Signal(signed(OUT_BITS))
        self.o_q = Signal(signed(OUT_BITS))
        self.c_we = Signal()
        self.c_bank = Signal()
        self.c_q = Signal(self.pbits)
        self.c_t = Signal(5)
        self.c_data = Signal(signed(COEF_BITS))
        self.consumed = Signal(32)
        self.missed = Signal(32)
        self.first_miss = Signal(32)
        self.samples = Signal(32)
        self.underrun = Signal()
        self.en = Signal()
        self.frac = Signal(FRAC_BITS)
        self.line = [Signal(4, name=f"line{t}") for t in range(self.taps)]
        self.wrap = Signal()
        self.code = Signal(4)
        self.s_sum_i = Signal(signed(SUM_BITS))
        self.s_sum_q = Signal(signed(SUM_BITS))

    def elaborate(self, platform):
        m = Module()
        P, S, W, pb = self.phases, self.span, self.taps, self.pbits
        regs = self.regs
        en = self.en
        active = Signal()
        m.d.comb += active.eq(self.run & self.selected)

        # --- DMA words: the unpack register and one staging word ----------
        ureg = Signal(32)  # codes still to enter, next one in bits 3..0
        ucnt = Signal(range(CODES_PER_WORD + 1))
        umiss = Signal()  # ureg is a missing word (8 zero symbols)
        stg = Signal(32)
        svalid = Signal()
        smiss = Signal()
        pend = Signal(2)  # requests sent, response not yet seen
        arm = Signal(range(ARM + 1))
        primed = Signal()
        held = Signal(3)
        m.d.comb += held.eq((ucnt != 0) + svalid + pend)
        armed = arm == ARM
        m.d.sync += arm.eq(Mux(active, Mux(armed, arm, arm + 1), 0))
        m.d.comb += self.rd_en.eq(active & armed & (held < 2) & ~self.start)
        resp = Signal()
        m.d.comb += resp.eq((self.rd_valid | self.rd_underflow) & (pend != 0))
        rword = Mux(self.rd_valid, self.rd_data, 0)
        rmiss = ~self.rd_valid
        m.d.sync += pend.eq(pend + self.rd_en - resp)

        ready = Signal()
        m.d.comb += [
            ready.eq(primed | (ucnt != 0)),
            en.eq(self.req & active & ready),
        ]
        m.d.sync += primed.eq(ready)

        # --- D0: phase accumulator, line, counters ------------------------
        nxt = Signal(FRAC_BITS + 1, attrs=NO_DSP)
        m.d.comb += [
            nxt.eq(self.frac + regs.step),
            self.wrap.eq(nxt[FRAC_BITS]),
            self.code.eq(ureg[:4]),
        ]
        consume = Signal()
        m.d.comb += consume.eq(en & self.wrap)
        take = Signal()  # the unpack register is empty after this clock
        m.d.comb += take.eq((ucnt == 0) | (consume & (ucnt == 1)))

        consumed_n = Signal(32, attrs=NO_DSP)
        missed_n = Signal(32, attrs=NO_DSP)
        samples_n = Signal(32, attrs=NO_DSP)
        m.d.comb += [
            consumed_n.eq(self.consumed + 1),
            missed_n.eq(self.missed + Mux(ucnt == 0, 1, CODES_PER_WORD)),
            samples_n.eq(self.samples + 1),
        ]
        missed_any = Signal()

        with m.If(en):
            m.d.sync += [self.frac.eq(nxt[:FRAC_BITS]), self.samples.eq(samples_n)]
        # a missing word counts when its first symbol enters (and an empty
        # unpack register, which the prefetch never lets happen, per symbol)
        miss_now = Signal()
        m.d.comb += miss_now.eq((ucnt == 0) | (umiss & (ucnt == CODES_PER_WORD)))
        with m.If(consume):
            m.d.sync += [self.line[t].eq(self.line[t - 1]) for t in range(1, W)]
            m.d.sync += self.line[0].eq(self.code)
            with m.If(miss_now):
                m.d.sync += [self.missed.eq(missed_n), self.underrun.eq(1), missed_any.eq(1)]
                with m.If(~missed_any):
                    m.d.sync += self.first_miss.eq(self.consumed)
            with m.If((ucnt != 0) & ~umiss):
                m.d.sync += self.consumed.eq(consumed_n)
            with m.If(ucnt != 0):
                m.d.sync += [ureg.eq(ureg >> 4), ucnt.eq(ucnt - 1)]
        with m.If(take):
            with m.If(svalid):
                m.d.sync += [ureg.eq(stg), ucnt.eq(CODES_PER_WORD), umiss.eq(smiss), svalid.eq(0)]
            with m.Elif(resp):
                m.d.sync += [ureg.eq(rword), ucnt.eq(CODES_PER_WORD), umiss.eq(rmiss)]
        with m.If(resp & ~(take & ~svalid)):
            m.d.sync += [stg.eq(rword), smiss.eq(rmiss), svalid.eq(1)]

        # D0 -> D1: sample n's phase row and codes
        q1 = Signal(pb)
        snap = [Signal(4, name=f"snap{t}") for t in range(W)]
        with m.If(en):
            m.d.sync += q1.eq(self.frac[FRAC_BITS - pb:])
            m.d.sync += [snap[t].eq(self.line[t]) for t in range(W)]

        # --- bank load (registered; port A's address while writing) -------
        far = Signal()
        m.d.comb += far.eq(self.c_t >= S)
        lw_we = Signal()
        lw_pair = Signal(range(S))
        lw_addr = Signal(pb + 1)
        lw_data = Signal(signed(COEF_BITS))
        m.d.sync += [
            lw_we.eq(self.c_we & (self.c_t < W)),
            lw_pair.eq(Mux(far, W - 1 - self.c_t, self.c_t)),
            lw_addr.eq(Cat(Mux(far, ~self.c_q, self.c_q), self.c_bank)),
            lw_data.eq(self.c_data),
        ]

        # --- D1, D2: one memory per tap pair, then the terms --------------
        cd2 = [Signal(4, name=f"cd{t}") for t in range(W)]
        with m.If(en):
            m.d.sync += [cd2[t].eq(snap[t]) for t in range(W)]
        data = [None] * W
        for p in range(S):
            u = W - 1 - p
            if self.a is not None:
                init = [self.a[q][p] for q in range(P)] + [self.b[q][p] for q in range(P)]
            else:
                init = []
            mem = Memory(shape=signed(COEF_BITS), depth=2 * P, init=init)
            m.submodules[f"bank{p}"] = mem
            wp = mem.write_port()
            rpa = mem.read_port()
            rpb = mem.read_port()
            pa = Signal(pb + 1, name=f"pa{p}")
            m.d.comb += [
                pa.eq(Mux(lw_we, lw_addr, Cat(q1, snap[p][0]))),
                wp.addr.eq(pa),
                wp.data.eq(lw_data),
                wp.en.eq(lw_we & (lw_pair == p)),
                rpa.addr.eq(pa),
                rpa.en.eq(en),
                rpb.addr.eq(Cat(~q1, snap[u][0])),  # a[P-1-q][p] = a[q][u]
                rpb.en.eq(en),
            ]
            data[p] = rpa.data
            data[u] = rpb.data

        # Terms: +v, -v or 0 by the code. -v = ~v + 1: the term register
        # takes ~v (a LUT per bit, no carry chain per tap) and the +1s go
        # into the tree as one more leaf, the count of negated terms
        # (counted from the D1 snapshot in two stages, ready with the
        # terms), so the sum is exact.
        def sign_sets(table):
            return ({k for k in range(8) if table[k]}, {k for k in range(8) if table[k] < 0})

        leaves = {"i": [], "q": []}
        for nm, table in (("i", SI), ("q", SQ)):
            nonzero, negative = sign_sets(table)

            def has(code, ks):
                return code[3] & Cat(*[code[:3] == k for k in sorted(ks)]).any()

            for t in range(W):
                v = Signal(signed(TERM_BITS), name=f"v{nm}{t}")
                m.d.comb += v.eq(data[t])
                c = Signal(signed(TERM_BITS), name=f"c{nm}{t}")
                m.d.comb += c.eq(Mux(has(cd2[t], nonzero), Mux(has(cd2[t], negative), ~v, v), 0))
                term = Signal(signed(TERM_BITS), name=f"t{nm}{t}")
                with m.If(en):
                    m.d.sync += term.eq(c)
                leaves[nm].append(term)
            flags = [has(snap[t], negative) for t in range(W)]
            groups = []
            for j in range(0, W, 6):
                g = Signal(range(7), name=f"neg{nm}_g{j // 6}")
                with m.If(en):
                    m.d.sync += g.eq(sum(flags[j:j + 6]))
                groups.append(g)
            count = Signal(range(W + 1), name=f"neg{nm}")
            with m.If(en):
                m.d.sync += count.eq(sum(groups))
            leaves[nm].append(count)
        leaves_i, leaves_q = leaves["i"], leaves["q"]

        # --- D3..D7: adder trees ------------------------------------------
        def tree(leaves, name):
            # level l sums at most 2**(l + 1) terms of TERM_BITS (and the
            # count, at most 2S): TERM_BITS + l + 2 bits hold every sum
            # exactly, SUM_BITS at the root
            cur = list(leaves)
            lvl = 0
            while len(cur) > 1:
                nx = []
                width = min(TERM_BITS + lvl + 2, SUM_BITS) if len(cur) > 2 else SUM_BITS
                for i in range(0, len(cur), 2):
                    s = Signal(signed(width), name=f"{name}_l{lvl}_{i // 2}", attrs=NO_DSP)
                    with m.If(en):
                        m.d.sync += s.eq(cur[i] + (cur[i + 1] if i + 1 < len(cur) else 0))
                    nx.append(s)
                cur = nx
                lvl += 1
            return cur[0], lvl

        si, levels = tree(leaves_i, "si")
        sq, _ = tree(leaves_q, "sq")
        assert self.latency == 4 + levels

        # --- D8: (sum << 1) >> SHIFT; D9: (+1) >> 1, saturate ---------------
        # (sum << 1) >> s = sum >> (s - 1) exactly for s >= 1, so
        # ((sum << 1 >> s) + 1) >> 1 = (sum + 2**(s - 1)) >> s, and = sum for
        # s = 0: the model's round_shift for every s.
        vld = Signal(self.latency)  # vld[i]: i + 1 samples since START
        with m.If(en):
            m.d.sync += vld.eq(Cat(Const(1, 1), vld[:-1]))
        outs = []
        for s, o, ss, nm in ((si, self.o_i, self.s_sum_i, "i"), (sq, self.o_q, self.s_sum_q, "q")):
            w = Signal(signed(SUM_BITS + 1), name=f"w{nm}")
            sd = Signal(signed(SUM_BITS), name=f"sd{nm}")
            r = Signal(signed(SUM_BITS + 1), name=f"r{nm}", attrs=NO_DSP)
            lo, hi = -(1 << (OUT_BITS - 1)), (1 << (OUT_BITS - 1)) - 1
            m.d.comb += r.eq((w + 1) >> 1)
            with m.If(en):
                m.d.sync += [
                    w.eq((s << 1) >> regs.shift),
                    sd.eq(s),
                    o.eq(Mux(vld[-1], Mux(r > hi, hi, Mux(r < lo, lo, r)), 0)),
                    ss.eq(sd),
                ]
            outs.append(o)
        with m.If(~active):
            m.d.sync += [o.eq(0) for o in outs]

        # START resets the state (last: it overrides the updates above).
        with m.If(self.start):
            m.d.sync += [
                self.frac.eq(0),
                ureg.eq(0), ucnt.eq(0), umiss.eq(0),
                stg.eq(0), svalid.eq(0), smiss.eq(0),
                pend.eq(0), arm.eq(0), primed.eq(0),
                self.consumed.eq(0), self.missed.eq(0), self.first_miss.eq(0),
                self.samples.eq(0), self.underrun.eq(0), missed_any.eq(0),
                vld.eq(0),
            ]
            m.d.sync += [ln.eq(0) for ln in self.line]
        return m


# --- control: the RX block's protocol on the DAC core's GPIO pair ---------

# Register addresses (isdb_frontend::control::tx, Rust).
STEP = 0x00
SHIFT = 0x01
STATUS = 0x02
CONSUMED = 0x03
MISSED = 0x04
FIRST_MISS = 0x05
SAMPLES = 0x06
A_BASE = 0x4000
B_BASE = 0x8000

OP_NOP, OP_ADDR, OP_HI, OP_WRITE, OP_READ, OP_CTRL, OP_WRITE_S = range(7)
CTRL_RUN, CTRL_START, CTRL_SELECT = 1, 2, 4


class TxControl(Elaboratable):
    """Command decoder and register file for one `SymbolTx`, on
    `up_dac_gpio_out` / `up_dac_gpio_in` of the DAC core (`cf-ad9361-dds-
    core-lpc` debug addresses 0x800000BC / 0x800000B8).

    The protocol of the RX block's `Control`: `gpio_out` passes a
    two-register synchroniser (it arrives from the AXI clock and changes
    only when the host writes it), and a command executes two cycles after
    its toggle bit (31) changes there, when every bit has settled. Bits
    30..28 are the operation, 27..1 the argument; bit 0 is not used (it is
    the TX FIR's enable in Tezuka's own image, also written by the kernel's
    interpolation attribute). Banks: a at 0x4000 + q*32 + t, b at
    0x8000 + q*32 + t (t < 2S, by WRITE_S or HI/WRITE); a far tap
    (t >= S) writes its mirror, so writing either half, or both, loads the
    bank. STEP keeps 24 bits, SHIFT 5. STATUS: bit 0 run, 1 selected, 2
    underrun (sticky until START). CONSUMED, MISSED, FIRST_MISS, SAMPLES:
    the core's counters (read only). CTRL: bit 0 RUN, bit 1 START (one
    pulse), bit 2 SELECT (the block drives the rfifo and owns the upack's
    read).

    A start without lead-in misses: STEP, SHIFT (and banks other than the
    default), CTRL START|SELECT (the block owns the DMA, requests nothing
    and sends zeros), then the TX DMA started (its first buffers pushed),
    then CTRL RUN|SELECT: the first request finds a word. With RUN at once
    the block requests from the start and counts the words the DMA does
    not have yet as missed (FIRST_MISS 0). Stop: CTRL 0 (transparent
    again).
    """

    def __init__(self, core):
        self.core = core
        self.gpio_out = Signal(32)
        self.gpio_in = Signal(32)
        self.run = Signal()
        self.selected = Signal()

    def elaborate(self, platform):
        m = Module()
        core = self.core
        regs = core.regs.fields()
        pb = core.pbits

        s1 = Signal(32)
        s2 = Signal(32)
        m.d.sync += [s1.eq(self.gpio_out), s2.eq(s1)]
        prev = Signal()
        m.d.sync += prev.eq(s2[31])
        edge1 = Signal()
        go = Signal()
        m.d.sync += [edge1.eq(s2[31] != prev), go.eq(edge1)]

        op = s2[28:31]
        arg = s2[1:28]
        addr = Signal(16)
        staged = Signal(16)
        show_read = Signal()
        readback = Signal(32)

        value = Signal(32)
        m.d.comb += value.eq(Mux(op == OP_WRITE_S, arg.as_signed(), Cat(arg[:16], staged)))
        bank_addr = addr[14] ^ addr[15]
        if 5 + pb < 14:  # rows beyond P (only with P < 512)
            bank_addr = bank_addr & (addr[5 + pb:14] == 0)
        m.d.comb += [
            core.run.eq(self.run),
            core.selected.eq(self.selected),
            core.c_bank.eq(addr[15]),
            core.c_q.eq(addr[5:5 + pb]),
            core.c_t.eq(addr[:5]),
            core.c_data.eq(value[:COEF_BITS]),
        ]
        named = {STEP: "step", SHIFT: "shift"}
        m.d.sync += core.start.eq(0)
        with m.If(go):
            with m.Switch(op):
                with m.Case(OP_NOP):
                    m.d.sync += show_read.eq(0)
                with m.Case(OP_ADDR):
                    m.d.sync += [addr.eq(arg[:16]), show_read.eq(0)]
                with m.Case(OP_HI):
                    m.d.sync += [staged.eq(arg[:16]), show_read.eq(0)]
                with m.Case(OP_WRITE, OP_WRITE_S):
                    m.d.sync += [addr.eq(addr + 1), show_read.eq(0)]
                    for a, name in named.items():
                        with m.If(addr == a):
                            sig = regs[name]
                            m.d.sync += sig.eq(value[: len(sig)])
                    m.d.comb += core.c_we.eq(bank_addr)
                with m.Case(OP_READ):
                    m.d.sync += show_read.eq(1)
                    with m.Switch(addr):
                        for a, name in named.items():
                            with m.Case(a):
                                m.d.sync += readback.eq(regs[name])
                        with m.Case(STATUS):
                            m.d.sync += readback.eq(Cat(self.run, self.selected, core.underrun))
                        with m.Case(CONSUMED):
                            m.d.sync += readback.eq(core.consumed)
                        with m.Case(MISSED):
                            m.d.sync += readback.eq(core.missed)
                        with m.Case(FIRST_MISS):
                            m.d.sync += readback.eq(core.first_miss)
                        with m.Case(SAMPLES):
                            m.d.sync += readback.eq(core.samples)
                        with m.Default():
                            m.d.sync += readback.eq(0)
                with m.Case(OP_CTRL):
                    m.d.sync += [
                        show_read.eq(0),
                        self.run.eq(arg[0]),
                        core.start.eq(arg[1]),
                        self.selected.eq(arg[2]),
                    ]
        ident = Const(id_word(core.phases, core.span), 32)
        m.d.comb += self.gpio_in.eq(Mux(show_read, readback, ident))
        return m

