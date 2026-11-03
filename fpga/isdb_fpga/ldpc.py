"""ISDB-S3 LDPC decoder (`LdpcDecoder`): layered offset min-sum in i8 with
the schedule contract of docs/plans/isdbs3-fpga-rx.md ("The LDPC schedule
contract", the delta write-back of 2026-11-01), S check-node units, early
stop, a fixed iteration limit. It must reproduce the Rust model
(`isdb_fec::ldpc::fixed`, `Schedule::Layered { step: S }`) bit for bit;
`ldpcmodel.py` states the contract in Python and `ldpcsched.lane_decode`
is this design's own scheme in Python (both checked against the vectors).

Codes: every rate of a rate set (default the eleven B44 rates, n = 44 880,
NG = 120 groups of 374 bits), the address tables as ROM built at
elaboration (from fpga/build/ldpc-tables.json, else the .rs source).
S (the step, the check-node units) must divide 374: 11, 17, 22, 34 or
187 (S = 374 / 2 and 374 are refused: ldpcsched).

Storage and address generation (ldpcsched's docstring has the derivation
and `ldpcsched.check_layout` its verification against the code's checks):
bit j of group g is lane j mod S, word j div S; the information groups are
the codeword's bits in order, the parity groups G + l hold p_{l + i q} at
bit i. Each lane is a memory of NG * Wp words (Wp = W rounded up to a
power of two, W = 374 / S), address {g, w}, 9 bits: L (i8) and H (the hard
decision of the previous iteration, below). K such banks (the codeword
queue, below): codewords load, wait, decode and stream their results in
different banks. A step of layer l reads, per table entry (g, s = a S +
b), every lane once: lane lam at word (st - a - [lam >= S - b]) mod W,
for unit (lam + b) mod S (a rotation by b); the two parity entries are
generated (p_{c-1}: group G + l - 1, or G + q - 1 at shift 1 in layer 0,
whose check 0 has no such edge; p_c: group G + l).

Messages (compressed): per check (unit, step) the record m1, m2 (after
the offset, 7 bits each) and the index of min1 (5 bits), one memory S x
19 bits wide, one word per step; per edge the sign of r, one memory S bits
wide, one word per (step, entry). r_old = +-(k == imin ? m2 : m1).

The delta write-back (contract): every lane keeps the sums it wrote for
the last two entries of the step (11 bits, unsaturated); an edge whose bit
an earlier check of the step shares writes D = r_new - r_old, else A = t +
r_new, and adds onto the sum of the entry one or two back when that
entry wrote the same bit in this step; the memory gets sat8 of the sum.
The entries of one group in a layer (a chain: the contract's conflicting
pairs and triples) are consecutive in the ROM, so the last write of a
shared bit in the step carries t_1 + r_1,new + sum (r_i,new - r_i,old):
the contract's value, independent of the order of the adds. The units'
modes are thresholds on u per entry (`Layout.contribution`,
`Layout.acc`), from a small chain ROM.

Syndrome: pass p (iteration p) reads, for every edge, the hard decision
of iteration p - 1 of its bit: the sign of L at the bit's first read in
the pass (`Layout.first_read`), else H, which every write stores (so H
holds iteration p - 1's decisions once the bit was first read). The XOR
per check, counted over the pass, is the syndrome weight of iteration
p - 1; pass p decides: 0 -> converged after p - 1 iterations; p - 1 =
max_iter -> not converged; else pass p + 1. The codeword's result is H
after that pass (its writes finish first). So a codeword converging after
i iterations takes i + 1 passes (the last one's L updates are not used).

Schedule, cycle by cycle (entry k of a step; one entry per cycle, a
step's entries on consecutive cycles, steps back to back unless the
hazard gap says otherwise). Every block RAM port is a register (the lane
banks' read address at A, their write address, data and enable at the
port stage; one per bank and lane, the engine's, the output stream's and
the loader's requests muxed into it a cycle ahead) and every block RAM's
output goes into a register first (its output register, DO_REG):

  I0  sequencer: step (layer, st), entry k; the info ROM's address
  I1  the info ROM's output (a block RAM)
  I1b its output register: g, s, first, chain (the parity entries
      generated); the chain ROM's address (a running pointer), registered
  I2a (a, b) = divmod(s, S) by a LUT ROM
  I2b w0 = (st - a) mod W, w1 = (st - a - 1) mod W, the lane threshold
      S - b; the chain ROM's data
  I3  per lane the address {g, lam >= S - b ? w1 : w0}, into the bank's
      address register; the record and sign memories' addresses
  A   the addresses at the memories (they sample them at A's end)
  A1  the memories' outputs into their output registers
  A2  the decoding bank's word per lane (a K:1 mux); r_old's magnitude
      (record word: k == imin ? m2 : m1) and sign
  A3  the lanes rotated to the units by b: the first half of the levels
  A4  the second half; the per-unit flags (first read, valid, delta mode,
      accumulate mode) from the chain thresholds
  A5  t + r_old (unsaturated); the old hard decision h
  A6  t = sat(L - r_old)
  A7  |t| (saturating); t, r_old, h, flags into the 64-deep FIFO
  A8  min1, min2, imin, sign parity, syndrome parity (reset at k = 0)
  L   (the step's last entry's A8 + 1) min - offset; the syndrome
      popcount starts
  L1  max(min - offset, 0) latched (r_new's record) and written to the
      record memory; the write phase starts: the FIFO's entry 0
  Wa  (entry k at L1 + 1 + k) the FIFO's entry: |r_new| (k == imin), its
      sign, -r_old
  Wb  v = t + r_new, or r_new - r_old for an edge whose bit an earlier
      check of the step shares; the sign memory write; the rotation amount
  Wc  units rotated back to the lanes: the first half; the lane addresses
  Wd  the second half
  We  per lane: sum = v + (the pending sum of the entry 1 or 2 back)
  Wf  {h, sat8(sum)} into the bank's port registers (the write is at the
      port stage, Wf + 1)

So entry k's write reaches its memory WRITE_LATENCY = 16 cycles after
d + k cycles past its read (d the layer degree), measured by
synth/ldpc_latency.py. A step may read a word only after the last write of
it: `ldpcsched.gaps` computes, at elaboration, the idle cycles before each
step (a steady state over passes; most steps need none); the gap ROM holds
the nonzero ones but the first step's, which is a per-rate constant. The
sequencer loads a step's wait at the previous step's last entry (the gap
ROM's entry, looked up and compared in the two cycles before) and counts
it down; its issue is a function of three registers. A pass's first step
also waits for the previous pass's decision: the last step's popcount,
the decision registered, DECIDE = 19 idle cycles after the pass's last
issue (also measured), which the first step's gap covers.

Clock target 200 MHz (`clk_fpga`-derived MMCM output in the image):
Vivado 2026.1, out of context (synth/ldpc_vivado.py, synth_design
-mode out_of_context, place, route), S = 34, K = 3, all eleven rates:
WNS +0.391 ns at 5.0 ns, 11 790 LUT, 15 232 FF, 79.5 BRAM36, 0 DSP. Every
stage is at most one carry chain plus a few LUT levels; the rotators
split in two stages of three 2:1 levels at S = 34; the registers with many
loads carry `max_fanout` (FANOUT) for Vivado to replicate; the
read-to-write FIFO stays in LUTRAM (`ram_style`). No DSP: the input
address mapping divides by 374 and by q with pipelined restoring
dividers.

The codeword queue. K banks (a build parameter, default 3) of L/H; a
codeword holds a bank from its `begin` (the first of its LLRs) until its
hard decisions have left on the output stream. Two load contexts
(`in_ctx`: 0 the slot codewords, 1 the TMCC codeword, whose LLRs arrive
interleaved with the slots') may be open at once; each LLR write carries
its codeword's bank. A bank is allocated at `begin` (the lowest free one)
and freed when its output stream ends; `load_ready` = a bank is free.
`start` commits the context's codeword (after its last LLR) to the
result queue, in commit order; the engine decodes the queue in order, and
the results leave in the same order. The input never stalls (the
demapper runs at one symbol per clock), so the back-pressure is
`load_ready` and the overrun rule: a codeword whose `begin` comes while
every bank is held is **dropped**: its LLRs are discarded, `overruns`
counts it, and at its `start` it still enters the result queue, so in
its place in the sequence the decoder emits a result record with
res_overrun = 1, res_converged = 0, res_iterations = 0, res_unsat = 0
and no data words: a slot not decoded, like a failed LDPC (the
downstream dispersal runs across the valid slots in order and the host
counts every slot). Why K = 3: a codeword is complete only at its slot's
end (the interleaver spreads it over the slot, T = 33.05 ms / 121 ≈ 54.6k
cycles at 200 MHz), so with K banks a codeword may take up to about
(K - 1) T less the output (2.8k cycles) and any queue wait: K = 2 gives
9 passes at 93/120, S = 34 (8 iterations, the worst-case figure), K = 3
about 19 (a limit of 16 for an isolated long codeword); the frame-average
budget is about 10 passes per codeword against a measured mean of 3.1
(`ldpcqueue.py` models it). The result queue holds QUEUE_DEPTH entries;
an entry committed while it is full (beyond any rate the slots can
cause) is lost and sets the sticky `queue_overflow`.

Abandoned codewords (issue #28). A context's codeword holds its bank from
`begin` until it is committed; the chain may give one up before its last
LLR: a frame dropped or the chain restarted (a lost sync, the host's
START) while a slot codeword loads, the host stopping the input mid-way.
Two rules return such a bank, so none stays allocated without a codeword
that will ever be committed (before them each abandoned codeword kept its
bank until the bitstream was reloaded: 3, 2, 1, then no free bank, every
codeword dropped, the TMCC's too):

- `begin` on a context that still holds an uncommitted bank (its codeword
  begun, not committed, not dropped) takes that bank again (`reused`
  counts it): the new codeword writes every position it uses after the
  old one's writes (the load pipeline is in order), so nothing of the old
  survives into its decode;
- `abort` (one cycle, `in_ctx`) gives up the context's open codeword: its
  bank is freed at once (`aborted` counts it) and the context is closed
  (a commit of it without a new `begin` would enter the queue as a
  dropped codeword: an overrun record, as for a `begin` with no free
  bank). Committed codewords are untouched: they decode and their results
  leave in order as always. The chain sends `abort` on context 0 after
  every frame drop and restart (`back.py`), through the bridge's input
  FIFO in order with the beats.

`held` is the number of banks allocated (registered).

Ports (all in the sync domain):
  begin, in_ctx
      a codeword of context in_ctx begins (one cycle, before or with its
      first LLR): a bank is allocated, or the codeword is dropped;
  load_ready
      a bank is free (a `begin` now is not dropped);
  in_valid, in_pos (16), in_llr (i8), in_rate (rate index), in_ctx
      one LLR per cycle into context in_ctx's codeword, any order, never
      stalled; the codeword's rate must be on in_rate (the parity
      positions map through q);
  start, in_ctx, st_rate, st_max_iter (8), st_offset (7), st_short
      commit context in_ctx's codeword (one cycle, after its last LLR;
      `start` and `begin` of the next codeword may share a cycle).
      st_max_iter: the iteration limit (the chain's register; its reset
      value is DEFAULT_MAX_ITER = 14). st_short: the shortened TMCC code:
      the decoder writes +127 itself into the known-zero positions [0,
      info_start) and [info_start + info_len, k) (`shortened`, by default
      (1870, 9614)) before the first pass, so only the 31 680 sent
      positions need loading;
  o_valid, o_data (16), o_last, o_ready
      the hard decisions, 16 per word, bit 0 first: the NG groups in
      order, 374 bits each (so the information bits in codeword order,
      then the parity in layer order: p_l, p_{l+q}, ... per l; the bits
      of the last word past n are 0); none for a dropped codeword;
  res_valid (one cycle, before the codeword's first word), res_iterations,
  res_converged, res_unsat, res_overrun
      each committed codeword's result, in commit order;
  abort, in_ctx
      give up context in_ctx's open (begun, uncommitted) codeword: its
      bank freed (one cycle, not with `begin` or `start`);
  overruns (16), queue_overflow
      dropped codewords since reset (wraps), and the sticky queue flag;
  held, aborted (16), reused (16)
      the banks allocated now; banks freed by `abort` and banks a `begin`
      took back from its context's abandoned codeword, since reset (wrap);
  busy, passes, cycles (verification: the engine's passes and cycles of
      the last codeword).
"""

import os

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from . import ldpcmodel, ldpcsched
from .ldpcsched import GROUP

WRITE_LATENCY = 16
DECIDE = 19
FIFO_DEPTH = 64
OFFSET_BITS = 7
ITER_BITS = 8
DEFAULT_MAX_ITER = 14
QUEUE_DEPTH = 8
NO_DSP = {"use_dsp": "no"}
# Vivado synthesis attribute: replicate a register that drives many loads
FANOUT = {"max_fanout": 48}
# Vivado synthesis attribute: a memory in distributed RAM (LUTRAM)
LUTRAM = {"ram_style": "distributed"}


def _bits(x):
    return max(1, int(x).bit_length())


def _pow2(n):
    """The power of two at least n (at least 2): the ROMs and lane memories
    are that deep, so every address their ports can see is inside them (a
    don't-care read on an idle cycle stays in range: the compiled
    simulation checks it, and a block RAM is that deep anyway)."""
    return max(2, 1 << (n - 1).bit_length())


CAT_FANIN = 32


def _cat(m, parts, name):
    """Cat(*parts) for a long list, as a tree of wires of at most CAT_FANIN
    parts each: the same bits, but a compiled simulation gets nested
    expressions of bounded depth instead of one S-deep concatenation (the
    C++ compiler refuses that at S = 187: template depth). Each node is
    `chunk ^ 0` into a wire marked keep: a plain connection would only be
    an alias of the concatenation in RTLIL, and write_cxxrtl -O6 inlines
    every public wire not marked keep. Synthesis removes the XOR."""
    if len(parts) <= CAT_FANIN:
        return Cat(*parts)
    nodes = []
    for j in range(0, len(parts), CAT_FANIN):
        chunk = Cat(*parts[j:j + CAT_FANIN])
        w = Signal(len(chunk), name=f"{name}_{j // CAT_FANIN}", attrs={"keep": 1})
        m.d.comb += w.eq(chunk ^ Const(0, len(chunk)))
        nodes.append(w)
    return _cat(m, nodes, name + "_")


def _rotate(m, vec, width, n, amount, name, cat, lo=0, hi=None):
    """vec (n fields of `width` bits) rotated so that field i of the result
    is field (i - amount) mod n of vec (amount < n): a barrel rotator, one
    2:1 mux level per bit of `amount`; only the levels lo .. hi - 1 (the
    bits of `amount` they use), so a rotation can be split across stages
    (rotations compose: the result of the two halves is the whole)."""
    cur = vec
    levels = _bits(n - 1) if hi is None else hi
    for i in range(lo, levels):
        sh = 1 << i
        if sh >= n:
            break
        rot = cat([cur[width * ((f - sh) % n):width * ((f - sh) % n) + width] for f in range(n)], f"{name}{i}r")
        # a rotation by sh < n; amounts are < n, so the levels' sum stays < 2n:
        # the wrap is exact because every level is a true rotation mod n
        nxt = Signal(width * n, name=f"{name}{i}")
        m.d.comb += nxt.eq(Mux(amount[i], rot, cur))
        cur = nxt
    return cur


def default_codes():
    """{rate: rows} of the eleven B44 rates."""
    return ldpcmodel.load_tables()


class _Rate:
    """Elaboration-time constants of one rate for S units."""

    def __init__(self, rows, ngroups, s_units):
        self.lay = lay = ldpcsched.Layout(rows, ngroups, s_units)
        self.G, self.q, self.d, self.W = lay.G, lay.q, lay.d, lay.W
        self.k = GROUP * self.G
        self.steps = self.q * self.W
        self.edges = self.steps * self.d
        self.gaps, self.cycles = ldpcsched.gaps(lay, WRITE_LATENCY, DECIDE)
        self.info = []  # per layer, the d - 2 information entries
        self.chain = []  # the chain entries in pass order
        for layer in range(self.q):
            for e in lay.layers[layer][:-2]:
                self.info.append(e)
                if e["nf"]:
                    self.chain.append(e)
        assert len(self.info) == self.q * (self.d - 2)


class LdpcDecoder(Elaboratable):
    """The decoder; `codes` = {rate: rows} (default the eleven B44 rates,
    `ngroups` 120), the rate index of the ports is the position in
    sorted(codes)."""

    def __init__(self, units, codes=None, ngroups=120, shortened=ldpcmodel.TMCC_SHORT, banks=3,
                 sim=False):
        self.S = S = units
        assert banks >= 2, "at least two banks (one loading while one decodes)"
        self.K = banks
        self.sim = sim  # the concatenation tree of `_cat` (compiled simulation only)
        assert GROUP % S == 0 and S <= GROUP // 2, f"S = {S} must divide 374 and be at most 187"
        self.codes = codes if codes is not None else default_codes()
        self.rate_ids = sorted(self.codes)
        self.NG = ngroups
        self.n = GROUP * ngroups
        self.W = GROUP // S
        self.Wp = 1 << (self.W - 1).bit_length()
        self.shortened = shortened
        self.rates = [_Rate(self.codes[r], ngroups, S) for r in self.rate_ids]
        self.max_d = max(r.d for r in self.rates)
        # the read-to-write FIFO holds at most d + 2 entries (README)
        assert self.max_d <= 32 and self.max_d + 4 < FIFO_DEPTH
        nr = len(self.rates)
        self.begin = Signal()
        self.in_ctx = Signal()
        self.load_ready = Signal()
        self.in_valid = Signal()
        self.in_pos = Signal(16)
        self.in_llr = Signal(signed(8))
        self.in_rate = Signal(range(max(nr, 2)))
        self.start = Signal()
        self.st_rate = Signal(range(max(nr, 2)))
        self.st_max_iter = Signal(ITER_BITS)
        self.st_offset = Signal(OFFSET_BITS)
        self.st_short = Signal()
        self.o_valid = Signal()
        self.o_data = Signal(16)
        self.o_last = Signal()
        self.o_ready = Signal()
        self.res_valid = Signal()
        self.res_iterations = Signal(ITER_BITS)
        self.res_converged = Signal()
        self.res_unsat = Signal(16)
        self.res_overrun = Signal()
        self.overruns = Signal(16)
        self.queue_overflow = Signal()
        self.abort = Signal()
        self.held = Signal(range(banks + 1))
        self.aborted = Signal(16)
        self.reused = Signal(16)
        self.busy = Signal()
        self.passes = Signal(ITER_BITS + 1)
        self.cycles = Signal(24)

    def ports(self):
        return [self.begin, self.in_ctx, self.load_ready, self.in_valid, self.in_pos, self.in_llr,
                self.in_rate, self.start, self.st_rate, self.st_max_iter, self.st_offset,
                self.st_short, self.o_valid, self.o_data, self.o_last, self.o_ready,
                self.res_valid, self.res_iterations, self.res_converged, self.res_unsat,
                self.res_overrun, self.overruns, self.queue_overflow, self.busy, self.abort,
                self.held, self.aborted, self.reused]

    def elaborate(self, platform):
        m = Module()

        def cat(parts, name):
            """Cat(*parts); for the compiled simulation a bounded-depth tree
            (`_cat`), which a synthesis build does without."""
            return _cat(m, parts, name) if self.sim else Cat(*parts)

        def deep(n):
            """A memory's depth: for the compiled simulation a power of two,
            so a don't-care address on an idle cycle stays inside it (CXXRTL
            checks every port access); a synthesis build uses n."""
            return _pow2(n) if self.sim else n

        def idle0(valid, addr):
            """A memory address, 0 on an idle cycle for the compiled
            simulation (the counters run one past the end between passes)."""
            return Mux(valid, addr, 0) if self.sim else addr
        S, W, Wp, NG = self.S, self.W, self.Wp, self.NG
        rates = self.rates
        gbits = _bits(NG - 1)
        wbits = _bits(Wp - 1)
        abits = gbits + wbits
        sbits = _bits(S)  # values 0 .. S
        ubits = _bits(S - 1)
        max_steps = max(r.steps for r in rates)
        max_edges = max(r.edges for r in rates)
        stepbits = _bits(max_steps)
        edgebits = _bits(max_edges)
        lbits = _bits(max(r.q for r in rates) - 1)
        stbits = _bits(W - 1)
        kbits = 5

        def R(name, values, width=None):
            """A per-rate constant, indexed by the engine's rate register."""
            w = width or max(_bits(v) for v in values)
            return Array([Const(v, w) for v in list(values) + [values[-1]]])

        # --- the ROMs ------------------------------------------------------
        info_init, info_base, chain_rows, chain_base, gap_rows, gap_base = [], [], [], [], [], []
        ch_fields = []
        for r in rates:
            info_base.append(len(info_init))
            for e in r.info:
                info_init.append(e["g"] | e["s"] << gbits | e["first"] << (gbits + 9)
                                 | bool(e["nf"]) << (gbits + 10))
            chain_base.append(len(chain_rows))
            for e in r.chain:
                chain_rows.append(e)
            gap_base.append(len(gap_rows))
            # the steps after the first with a gap; the first step's gap (it
            # follows the previous pass's last step) is a per-rate constant
            for i, g in enumerate(r.gaps):
                if g and i:
                    gap_rows.append((i, g))
            gap_rows.append(((1 << stepbits) - 1, 0))  # sentinel
        gap0 = [r.gaps[0] for r in rates]
        gapbits = _bits(max([g for _, g in gap_rows] + gap0 + [1]))

        # chain ROM: per partner (up to 2): Eq (W = none), Er, small; td, lo1, hi1, lo2, hi2
        eqbits = _bits(W)
        ch_layout = [("eq0", eqbits), ("er0", sbits), ("sm0", 1), ("eq1", eqbits), ("er1", sbits),
                     ("sm1", 1), ("td", sbits), ("lo1", sbits), ("hi1", sbits), ("lo2", sbits),
                     ("hi2", sbits)]
        ch_width = sum(w for _, w in ch_layout)

        def ch_pack(e):
            v = {"td": e["td"], "lo1": e["lo1"], "hi1": e["hi1"], "lo2": e["lo2"],
                 "hi2": e["hi2"]}
            for i in range(2):
                if i < len(e["nf"]):
                    E = e["nf"][i]
                    v[f"eq{i}"], v[f"er{i}"] = divmod(E, S)
                    v[f"sm{i}"] = int(E < S)
                else:
                    v[f"eq{i}"], v[f"er{i}"], v[f"sm{i}"] = W, 0, 0
            word, sh = 0, 0
            for name, w in ch_layout:
                assert 0 <= v[name] < (1 << w), (name, v[name])
                word |= v[name] << sh
                sh += w
            return word

        def ch_unpack(sig):
            out, sh = {}, 0
            for name, w in ch_layout:
                out[name] = sig[sh:sh + w]
                sh += w
            return out

        info_rom = Memory(shape=gbits + 11, depth=deep(len(info_init) + 34), init=info_init)
        chain_init = [ch_pack(e) for e in chain_rows] or [0]
        chain_rom = Memory(shape=ch_width, depth=deep(max(len(chain_init), 2)), init=chain_init)
        gap_rom = Memory(shape=stepbits + gapbits, depth=deep(len(gap_rows)),
                         init=[i | g << stepbits for i, g in gap_rows])
        divmod_rom = Memory(shape=stbits + ubits, depth=deep(GROUP),
                            init=[(s // S) | (s % S) << stbits for s in range(GROUP)])
        m.submodules.info_rom = info_rom
        m.submodules.chain_rom = chain_rom
        m.submodules.gap_rom = gap_rom
        m.submodules.divmod_rom = divmod_rom
        info_rp = info_rom.read_port()
        chain_rp = chain_rom.read_port()
        gap_rp = gap_rom.read_port(domain="comb")
        dm_rp = divmod_rom.read_port(domain="comb")

        # --- the engine's registers -----------------------------------------
        rate = Signal.like(self.st_rate, name="rate")
        max_iter = Signal(ITER_BITS)
        offset = Signal(OFFSET_BITS, attrs=FANOUT)
        short = Signal()
        ibits = _bits(len(info_init) + 34)
        cbits = _bits(max(len(chain_init), 2))
        gpbits = _bits(len(gap_rows))
        # the rate's constants, registered at SETUP from `rate` (so no per-rate
        # mux sits on a timed path)
        dm1_r = Signal(kbits, name="dm1_r")  # d - 1
        dm2_r = Signal(kbits, name="dm2_r")  # d - 2
        qm2_r = Signal(lbits + 1, name="qm2_r")  # q - 2
        G_r = Signal(gbits, name="G_r")
        Gq1_r = Signal(gbits, name="Gq1_r")  # G + q - 1
        steps_m1_r = Signal(stepbits, name="steps_m1_r")
        info_b_r = Signal(ibits, name="info_b_r")
        chain_b_r = Signal(cbits, name="chain_b_r")
        gap_b_r = Signal(gpbits, name="gap_b_r")
        g0_r = Signal(8, name="g0_r")
        ll0_r = Signal(name="ll0_r")  # q == 1: layer 0 is the last

        # --- the banks -------------------------------------------------------
        K = self.K
        kb = _bits(K - 1)
        banks = []
        for b in range(K):
            lanes = []
            for lam in range(S):
                mem = Memory(shape=9, depth=deep(NG * Wp), init=[])
                m.submodules[f"l{b}_{lam}"] = mem
                lanes.append((mem.read_port(), mem.write_port()))
            banks.append(lanes)
        rec_mem = Memory(shape=19 * S, depth=max_steps, init=[])
        sign_mem = Memory(shape=S, depth=max_edges, init=[])
        # the read-to-write FIFO in LUTRAM (as before the pipelining: Vivado
        # would otherwise take 10 block RAMs for its 714-bit word)
        fifo_u = Memory(shape=21 * S, depth=FIFO_DEPTH, init=[], attrs=LUTRAM)
        fifo_s = Memory(shape=gbits + 2 * stbits + ubits + edgebits, depth=FIFO_DEPTH, init=[],
                        attrs=LUTRAM)
        m.submodules.rec_mem = rec_mem
        m.submodules.sign_mem = sign_mem
        m.submodules.fifo_u = fifo_u
        m.submodules.fifo_s = fifo_s
        rec_rp, rec_wp = rec_mem.read_port(), rec_mem.write_port()
        sgn_rp, sgn_wp = sign_mem.read_port(), sign_mem.write_port()
        fu_rp, fu_wp = fifo_u.read_port(), fifo_u.write_port()
        fs_rp, fs_wp = fifo_s.read_port(), fifo_s.write_port()

        dec_buf = Signal(kb)  # the bank the engine decodes
        out_buf = Signal(kb)  # the bank the output streams
        out_active = Signal()
        busy = self.busy
        # dsel[b]: bank b's ports belong to the engine (registered: one cycle
        # behind busy and dec_buf; the output waits that long after out_go)
        dsel = [Signal(name=f"dsel{b}", attrs=FANOUT) for b in range(K)]
        m.d.sync += [dsel[b].eq(busy & (dec_buf == b)) for b in range(K)]

        # Every bank port is a register: the read address (A), the write
        # address, data and enable (Wf); the engine's, the output stream's and
        # the loader's requests are muxed into them a cycle ahead.
        lane_addr = [Signal(abits, name=f"la{lam}") for lam in range(S)]  # the engine's (I3)
        out_addr = Signal(abits)  # the output stream's, held between its reads
        wr_addr = [Signal(abits, name=f"wfa{lam}") for lam in range(S)]  # the engine's (We)
        wr_data = [Signal(9, name=f"wfd{lam}") for lam in range(S)]
        wr_en = [Signal(name=f"wfe{lam}") for lam in range(S)]
        ld_addr = Signal(abits)
        ld_data = Signal(9)
        ld_lane = Signal(range(S))
        ld_bank = Signal(kb)
        ld_en = Signal()
        bq = []  # the read data registers (a BRAM's output register)
        for b in range(K):
            row = []
            for lam, (rp, wp) in enumerate(banks[b]):
                ra = Signal(abits, name=f"ra{b}_{lam}")
                pa = Signal(abits, name=f"pa{b}_{lam}")
                pd = Signal(9, name=f"pd{b}_{lam}")
                pe = Signal(name=f"pe{b}_{lam}")
                q = Signal(9, name=f"bq{b}_{lam}")
                m.d.sync += [
                    ra.eq(Mux(dsel[b], lane_addr[lam], out_addr)),
                    pa.eq(Mux(dsel[b], wr_addr[lam], ld_addr)),
                    pd.eq(Mux(dsel[b], wr_data[lam], ld_data)),
                    pe.eq(Mux(dsel[b], wr_en[lam], ld_en & (ld_bank == b) & (ld_lane == lam))),
                    q.eq(rp.data),
                ]
                m.d.comb += [rp.addr.eq(ra), wp.addr.eq(pa), wp.data.eq(pd), wp.en.eq(pe)]
                row.append(q)
            bq.append(row)

        def pipe(src, name, fan=()):
            """The fields one stage on (those in `fan` replicated for their
            many loads)."""
            out = {}
            for key, sig in src.items():
                r = Signal.like(sig, name=f"{name}_{key}", attrs=FANOUT if key in fan else {})
                m.d.sync += r.eq(sig)
                out[key] = r
            return out

        valids = []  # every stage's valid: the engine drains when all are low

        # --- the sequencer (I0) ----------------------------------------------
        SETUP, FILL, PASS, WAITD, DECIDE_, DRAIN = range(6)
        phase = Signal(3)
        in_pass = Signal()  # phase == PASS, as a register
        layer = Signal(lbits)
        st = Signal(stbits)
        k = Signal(kbits)
        krem = Signal(kbits)  # d - 1 - k
        n_idx = Signal(stepbits)
        e_idx = Signal(edgebits)
        in_step = Signal()  # entries 1 .. d-1 of a step remain (k != 0)
        lastlayer = Signal()
        wait = Signal(8)  # idle cycles still owed before the next step
        wz = Signal()  # wait == 0, as a register
        layer_info = Signal(ibits)
        pass_no = self.passes
        gap_ptr = Signal(gpbits)
        nhit = Signal()  # the gap ROM's next entry is step n_idx + 1 (registered)
        gval_r = Signal(gapbits)
        fill_g = Signal(gbits)
        fill_w = Signal(wbits)
        fill_base = Signal(16)

        m.d.comb += gap_rp.addr.eq(gap_ptr)
        # two stages (the ROM, then the comparison): n_idx and gap_ptr change at
        # a step's last entry, and the next step's last entry is d >= 5 cycles on
        gstep_r = Signal(stepbits)
        m.d.sync += [gstep_r.eq(gap_rp.data[:stepbits]), gval_r.eq(gap_rp.data[stepbits:]),
                     nhit.eq(gstep_r == n_idx + 1)]
        issue = Signal()
        m.d.comb += issue.eq(in_pass & (in_step | wz))
        last_k = krem == 0
        last_st = st == W - 1
        pass_last = Signal()
        m.d.comb += pass_last.eq(last_k & last_st & lastlayer)
        kind0 = Mux(last_k, 2, Mux(krem == 1, 1, 0))  # 0 information, 1 p_{c-1}, 2 p_c
        m.d.comb += info_rp.addr.eq(layer_info + k)
        # The wait before a step (the hazard gap of ldpcsched.gaps): loaded at
        # the previous step's last entry (nhit and gval_r have been stable
        # since the step before began: d >= 5), counted down on idle cycles;
        # the next step's first entry issues when it is 0.
        wait_next = Signal(8)
        m.d.comb += wait_next.eq(wait)
        with m.If(issue & last_k):
            m.d.comb += wait_next.eq(Mux(pass_last, g0_r, Mux(nhit, gval_r, 0)))
        with m.Elif(~issue & (wait != 0)):
            m.d.comb += wait_next.eq(wait - 1)
        m.d.sync += [wait.eq(wait_next), wz.eq(wait_next == 0)]
        with m.If(issue):
            m.d.sync += e_idx.eq(e_idx + 1)
            with m.If(last_k):
                m.d.sync += [k.eq(0), krem.eq(dm1_r), in_step.eq(0), n_idx.eq(n_idx + 1),
                             gap_ptr.eq(Mux(pass_last, gap_b_r, gap_ptr + nhit))]
                with m.If(last_st):
                    m.d.sync += [st.eq(0), layer.eq(layer + 1), layer_info.eq(layer_info + dm2_r),
                                 lastlayer.eq(layer == qm2_r)]
                    with m.If(lastlayer):
                        m.d.sync += [phase.eq(WAITD), in_pass.eq(0)]
                with m.Else():
                    m.d.sync += st.eq(st + 1)
            with m.Else():
                m.d.sync += [k.eq(k + 1), krem.eq(krem - 1), in_step.eq(1)]

        # I0 -> I1: the info ROM's output
        i1 = dict(v=Signal(), kind=Signal(2), layer=Signal(lbits), st=Signal(stbits),
                  k=Signal(kbits), n=Signal(stepbits), e=Signal(edgebits), last=Signal(),
                  p1=Signal(), lastlayer=Signal(), k0=Signal())
        m.d.sync += [
            i1["v"].eq(issue), i1["kind"].eq(kind0), i1["layer"].eq(layer), i1["st"].eq(st),
            i1["k"].eq(k), i1["n"].eq(n_idx), i1["e"].eq(e_idx), i1["last"].eq(last_k),
            i1["p1"].eq(pass_no == 1), i1["lastlayer"].eq(lastlayer), i1["k0"].eq(~in_step),
        ]
        valids.append(i1["v"])
        # I1 -> I1b: the ROM's output register
        rom_q = Signal(info_rp.data.shape())
        m.d.sync += rom_q.eq(info_rp.data)
        i1b = pipe(i1, "i1b")
        valids.append(i1b["v"])
        # I1b: the entry (parity entries generated); the chain ROM's address
        rom_g = rom_q[:gbits]
        rom_s = rom_q[gbits:gbits + 9]
        rom_first = rom_q[gbits + 9]
        rom_chain = rom_q[gbits + 10]
        is_info = i1b["kind"] == 0
        g1 = Signal(gbits)
        s1 = Signal(9)
        first1 = Signal()
        m.d.comb += [
            g1.eq(Mux(is_info, rom_g,
                      Mux(i1b["kind"] == 2, G_r + i1b["layer"],
                          Mux(i1b["layer"] == 0, Gq1_r, G_r + i1b["layer"] - 1)))),
            s1.eq(Mux(is_info, rom_s, (i1b["kind"] == 1) & (i1b["layer"] == 0))),
            first1.eq(Mux(is_info, rom_first,
                          Mux(i1b["kind"] == 1, i1b["layer"] == 0, ~i1b["lastlayer"]))),
        ]
        chain_ptr = Signal(cbits)
        layer_chain = Signal(cbits)
        ch_addr = Mux(i1b["k0"], layer_chain, chain_ptr)
        is_chain = is_info & rom_chain
        ch_ra = Signal(cbits)  # the chain ROM's address, registered: its data at I2b
        m.d.sync += ch_ra.eq(ch_addr)
        m.d.comb += chain_rp.addr.eq(ch_ra)
        with m.If(i1b["v"]):
            m.d.sync += chain_ptr.eq(ch_addr + is_chain)
            with m.If(i1b["last"] & (i1b["st"] == W - 1)):
                m.d.sync += layer_chain.eq(ch_addr + is_chain)
        i2a = pipe(dict(i1b, g=g1, s=s1, first=first1, chain=is_chain,
                        absent=(i1b["kind"] == 1) & (i1b["layer"] == 0),
                        firstend=(i1b["kind"] == 2) & i1b["lastlayer"]), "i2a")
        valids.append(i2a["v"])
        # I2a: a, b = divmod(s, S) (a LUT ROM)
        m.d.comb += dm_rp.addr.eq(i2a["s"])
        i2a_out = dict(i2a, a=dm_rp.data[:stbits], b=dm_rp.data[stbits:])
        del i2a_out["s"]
        i2b = pipe(i2a_out, "i2b")
        valids.append(i2b["v"])
        # I2b: the two candidate words, the lane threshold
        diff0 = Signal(signed(stbits + 2))
        m.d.comb += diff0.eq(i2b["st"] - i2b["a"])
        w0 = Mux(diff0 < 0, diff0 + W, diff0)[:stbits]
        w1 = Mux(diff0 < 1, diff0 + W - 1, diff0 - 1)[:stbits]
        # the chain ROM's data (a synchronous read of the address registered
        # at I1b, two reads in flight at most), the defaults for a plain entry
        ch = ch_unpack(chain_rp.data)
        defaults = {"eq0": W, "eq1": W, "td": S, "lo1": S, "lo2": S}
        chf = {f"c_{n}": Mux(i2b["chain"], v, Const(defaults.get(n, 0), len(v)))
               for n, v in ch.items()}
        i3 = pipe(dict(i2b, w0=w0, w1=w1, thr=Const(S, sbits) - i2b["b"], **chf), "i3",
                  fan=("w0", "w1", "thr", "g"))
        valids.append(i3["v"])
        # I3: each lane's address into the bank registers; the record and sign
        # memories' (read only for a valid entry: `idle0`)
        for lam in range(S):
            m.d.comb += lane_addr[lam].eq(Cat(Mux(lam >= i3["thr"], i3["w1"], i3["w0"]), i3["g"]))
        rec_ra = Signal(stepbits, attrs=FANOUT)
        sgn_ra = Signal(edgebits)
        rd_en = Signal()
        m.d.sync += [rec_ra.eq(idle0(i3["v"], i3["n"])), sgn_ra.eq(idle0(i3["v"], i3["e"])),
                     rd_en.eq(i3["v"])]
        m.d.comb += [rec_rp.addr.eq(rec_ra), sgn_rp.addr.eq(sgn_ra), rec_rp.en.eq(rd_en),
                     sgn_rp.en.eq(rd_en)]
        x0 = pipe(i3, "x0")  # A: the addresses are at the memories
        valids.append(x0["v"])
        x1 = pipe(x0, "x1")  # A1: the memories' outputs into their registers
        valids.append(x1["v"])
        rec_q = Signal(19 * S)
        sgn_q = Signal(S)
        m.d.sync += [rec_q.eq(rec_rp.data), sgn_q.eq(sgn_rp.data)]
        x2 = pipe(x1, "x2")  # A2: the bank mux; the old messages' magnitudes
        valids.append(x2["v"])
        rq = Signal(9 * S)
        rsel = Signal(kb, attrs=FANOUT)  # dec_buf, a copy for the bank mux
        m.d.sync += rsel.eq(dec_buf)
        m.d.sync += rq.eq(cat([Array([bq[b][lam] for b in range(K)])[rsel] for lam in range(S)],
                              "rqcat"))
        rec_hold = Signal(19 * S)
        rec_now = Mux(x2["k0"], rec_q, rec_hold)
        with m.If(x2["v"] & x2["k0"]):
            m.d.sync += rec_hold.eq(rec_q)
        rmag_a = [Signal(7, name=f"rma_{u}") for u in range(S)]
        rneg_a = [Signal(name=f"rna_{u}") for u in range(S)]
        for u in range(S):
            rec = rec_now[19 * u:19 * u + 19]
            mag = Mux(x2["k"] == rec[14:19], rec[7:14], rec[0:7])
            m.d.sync += [rmag_a[u].eq(Mux(x2["p1"], 0, mag)), rneg_a[u].eq(sgn_q[u] & ~x2["p1"])]
        x3 = pipe(x2, "x3")  # A3: the rotation's first half
        valids.append(x3["v"])
        bits_rot = _bits(S - 1) if S > 1 else 0
        half = (bits_rot + 1) // 2
        rr1 = Signal(9 * S)
        m.d.sync += rr1.eq(_rotate(m, rq, 9, S, x3["b"], "rra", cat, 0, half))
        rmag_b = [Signal(7, name=f"rmb_{u}") for u in range(S)]
        rneg_b = [Signal(name=f"rnb_{u}") for u in range(S)]
        m.d.sync += [rmag_b[u].eq(rmag_a[u]) for u in range(S)]
        m.d.sync += [rneg_b[u].eq(rneg_a[u]) for u in range(S)]
        x4 = pipe(x3, "x4")  # A4: the rotation's second half; the per-unit flags
        valids.append(x4["v"])
        rotated = Signal(9 * S)
        m.d.comb += rotated.eq(_rotate(m, rr1, 9, S, x4["b"], "rrb", cat, half, bits_rot))
        L2 = [Signal(signed(8), name=f"L2_{u}") for u in range(S)]
        H2 = [Signal(name=f"H2_{u}") for u in range(S)]
        rneg2 = [Signal(name=f"rn2_{u}") for u in range(S)]
        rmag2 = [Signal(7, name=f"rm2_{u}") for u in range(S)]
        fl2 = [dict(first=Signal(name=f"f2_{u}"), valid=Signal(name=f"v2_{u}"),
                    dmode=Signal(name=f"d2_{u}"), acc=Signal(2, name=f"a2_{u}")) for u in range(S)]
        gt = [x4["st"] > x4["c_eq0"], x4["st"] > x4["c_eq1"]]
        eq = [x4["st"] == x4["c_eq0"], x4["st"] == x4["c_eq1"]]
        for u in range(S):
            word = rotated[9 * u:9 * u + 9]
            excl = Const(0)
            for i in range(2):
                er, sm = x4[f"c_er{i}"], x4[f"c_sm{i}"]
                excl = excl | ((gt[i] | (eq[i] & (u >= er))) & (~sm | (u < er)))
            first = Mux(x4["firstend"], (x4["st"] == W - 1) & Const(int(u == S - 1), 1),
                        x4["first"] & ~excl)
            acc = Mux((u >= x4["c_lo1"]) | (u < x4["c_hi1"]), 1,
                      Mux((u >= x4["c_lo2"]) | (u < x4["c_hi2"]), 2, 0))
            m.d.sync += [
                L2[u].eq(word[:8]), H2[u].eq(word[8]),
                rneg2[u].eq(rneg_b[u]), rmag2[u].eq(rmag_b[u]),
                fl2[u]["first"].eq(first),
                fl2[u]["valid"].eq(~(x4["absent"] & (x4["st"] == 0) & Const(int(u == 0), 1))),
                fl2[u]["dmode"].eq(u >= x4["c_td"]),
                fl2[u]["acc"].eq(acc),
            ]
        x5 = pipe(x4, "x5")  # A5: t + r (unsaturated); the old hard decision
        valids.append(x5["v"])

        def carry(prev, name):
            """The per-unit fields one stage on."""
            out = []
            for u in range(S):
                d_ = {n_: Signal.like(s_, name=f"{n_}{name}_{u}") for n_, s_ in prev[u].items()}
                m.d.sync += [d_[n_].eq(prev[u][n_]) for n_ in d_]
                out.append(d_)
            return out

        tw = [Signal(signed(10), name=f"tw_{u}") for u in range(S)]
        h3 = [Signal(name=f"h3_{u}") for u in range(S)]
        for u in range(S):
            add = Signal(signed(10), name=f"twa_{u}", attrs=NO_DSP)
            m.d.comb += add.eq(Mux(rneg2[u], L2[u] + rmag2[u], L2[u] - rmag2[u]))
            m.d.sync += [tw[u].eq(add),
                         h3[u].eq(fl2[u]["valid"] & Mux(fl2[u]["first"], L2[u] < 0, H2[u]))]
        fr3 = carry([dict(fl2[u], rmag=rmag2[u], rneg=rneg2[u]) for u in range(S)], "3")
        x6 = pipe(x5, "x6")  # A6: t saturated
        valids.append(x6["v"])
        t3 = [Signal(signed(8), name=f"t3_{u}") for u in range(S)]
        for u in range(S):
            sat_t = Mux(tw[u] > 127, 127, Mux(tw[u] < -128, -128, tw[u]))
            m.d.sync += t3[u].eq(Mux(fr3[u]["valid"], sat_t, 127))
        fr4 = carry([dict(fr3[u], h=h3[u]) for u in range(S)], "4")
        x7 = pipe(x6, "x7")  # A7: |t|; the FIFO push
        valids.append(x7["v"])
        a4 = [Signal(7, name=f"a4_{u}") for u in range(S)]
        neg4 = [Signal(name=f"ng4_{u}") for u in range(S)]
        h4 = [Signal(name=f"h4_{u}") for u in range(S)]
        push_ptr = Signal(_bits(FIFO_DEPTH - 1), attrs=FANOUT)
        fu_word = []
        for u in range(S):
            t = t3[u]
            f = fr4[u]
            m.d.sync += [
                a4[u].eq(Mux(t == -128, 127, Mux(t < 0, -t, t))),
                neg4[u].eq(t < 0), h4[u].eq(f["h"]),
            ]
            fu_word.append(Cat(t, f["rmag"], f["rneg"], f["h"], f["dmode"], f["acc"], f["valid"]))
        m.d.comb += [
            fu_wp.addr.eq(push_ptr), fu_wp.data.eq(cat(fu_word, "fucat")), fu_wp.en.eq(x7["v"]),
            fs_wp.addr.eq(push_ptr),
            fs_wp.data.eq(Cat(x7["g"], x7["w0"], x7["w1"], x7["b"], x7["e"])),
            fs_wp.en.eq(x7["v"]),
        ]
        with m.If(x7["v"]):
            m.d.sync += push_ptr.eq(push_ptr + 1)
        x8 = pipe(x7, "x8")  # A8: the check-node accumulators
        valids.append(x8["v"])
        min1 = [Signal(7, name=f"min1_{u}") for u in range(S)]
        min2 = [Signal(7, name=f"min2_{u}") for u in range(S)]
        imin = [Signal(5, name=f"imin_{u}") for u in range(S)]
        par = [Signal(name=f"par_{u}") for u in range(S)]
        hpar = [Signal(name=f"hpar_{u}") for u in range(S)]
        with m.If(x8["v"]):
            for u in range(S):
                a = a4[u]
                with m.If(x8["k0"]):
                    m.d.sync += [min1[u].eq(a), min2[u].eq(127), imin[u].eq(0),
                                 par[u].eq(neg4[u]), hpar[u].eq(h4[u])]
                with m.Else():
                    m.d.sync += [par[u].eq(par[u] ^ neg4[u]), hpar[u].eq(hpar[u] ^ h4[u])]
                    with m.If(a < min1[u]):
                        m.d.sync += [min2[u].eq(min1[u]), min1[u].eq(a), imin[u].eq(x8["k"])]
                    with m.Elif(a < min2[u]):
                        m.d.sync += min2[u].eq(a)
        xl = pipe(dict(v=x8["v"] & x8["last"], n=x8["n"]), "xl")  # L: the check's result
        valids.append(xl["v"])
        od1 = [Signal(signed(9), name=f"od1r_{u}") for u in range(S)]  # min - offset
        od2 = [Signal(signed(9), name=f"od2r_{u}") for u in range(S)]
        imq = [Signal(5, name=f"imq_{u}") for u in range(S)]
        parq = [Signal(name=f"parq_{u}") for u in range(S)]
        for u in range(S):
            # max(min - offset, 0): the subtraction here, the clamp (its sign)
            # in L1
            d1 = Signal(signed(9), name=f"od1_{u}", attrs=NO_DSP)
            d2 = Signal(signed(9), name=f"od2_{u}", attrs=NO_DSP)
            m.d.comb += [d1.eq(min1[u] - offset), d2.eq(min2[u] - offset)]
            m.d.sync += [od1[u].eq(d1), od2[u].eq(d2), imq[u].eq(imin[u]), parq[u].eq(par[u])]
        hvec = Signal(S)
        hvalid = Signal()
        lastn = Signal()
        m.d.sync += [hvalid.eq(xl["v"]), hvec.eq(cat(hpar, "hcat")),
                     lastn.eq(xl["v"] & (xl["n"] == steps_m1_r))]
        xm = pipe(xl, "xm")  # L1: latch the result, write the record; the write phase starts
        valids.append(xm["v"])
        oo1 = [Mux(od1[u][8], 0, od1[u][:7]) for u in range(S)]
        oo2 = [Mux(od2[u][8], 0, od2[u][:7]) for u in range(S)]
        rm1 = [Signal(7, name=f"rm1_{u}") for u in range(S)]
        rm2 = [Signal(7, name=f"rmm2_{u}") for u in range(S)]
        rim = [Signal(5, name=f"rim_{u}") for u in range(S)]
        rpar = [Signal(name=f"rpar_{u}") for u in range(S)]
        # the latch's enable and the record's write port: copies of xm's
        # valid and step, for their many loads
        latch = Signal(attrs=FANOUT)
        rec_wa = Signal(stepbits, attrs=FANOUT)
        rec_we = Signal(attrs=FANOUT)
        m.d.sync += [latch.eq(xl["v"]), rec_wa.eq(idle0(xl["v"], xl["n"])), rec_we.eq(xl["v"])]
        with m.If(latch):
            for u in range(S):
                m.d.sync += [rm1[u].eq(oo1[u]), rm2[u].eq(oo2[u]), rim[u].eq(imq[u]),
                             rpar[u].eq(parq[u])]
        m.d.comb += [rec_wp.addr.eq(rec_wa),
                     rec_wp.data.eq(cat([Cat(oo1[u], oo2[u], imq[u]) for u in range(S)], "reccat")),
                     rec_wp.en.eq(rec_we)]
        # popcount of the old hard decisions' check parities, in two stages
        chunks = [hvec[i:i + 8] for i in range(0, S, 8)]
        part = [Signal(4, name=f"pc{i}") for i in range(len(chunks))]
        pvalid = Signal()
        plast = Signal()
        for p_, c in zip(part, chunks):
            m.d.sync += p_.eq(sum(c[i] for i in range(len(c))))
        m.d.sync += [pvalid.eq(hvalid), plast.eq(lastn)]
        unsat = Signal(16)
        pass_end = Signal()
        m.d.sync += pass_end.eq(0)
        with m.If(pvalid):
            m.d.sync += unsat.eq(unsat + sum(part))
            m.d.sync += pass_end.eq(plast)

        # --- the write phase -------------------------------------------------
        pop_ptr = Signal(_bits(FIFO_DEPTH - 1), attrs=FANOUT)
        wk = Signal(kbits)
        wact = Signal()
        valids.append(wact)
        with m.If(xm["v"]):
            m.d.sync += [wact.eq(1), wk.eq(0)]
        with m.Elif(wact):
            m.d.sync += wk.eq(wk + 1)
            with m.If(wk == dm1_r):
                m.d.sync += wact.eq(0)
        popping = xm["v"] | (wact & (wk != dm1_r))
        m.d.comb += [fu_rp.addr.eq(pop_ptr), fs_rp.addr.eq(pop_ptr)]
        with m.If(popping):
            m.d.sync += pop_ptr.eq(pop_ptr + 1)
        wa_v = Signal()
        wa_k = Signal(kbits)
        m.d.sync += [wa_v.eq(popping), wa_k.eq(Mux(xm["v"], 0, wk + 1))]
        valids.append(wa_v)
        # Wa: the new message's magnitude and sign, -r_old
        wb_v = Signal(name="wb_v")
        wb_sh = Signal(fs_rp.data.shape(), name="wb_sh")
        m.d.sync += [wb_v.eq(wa_v), wb_sh.eq(fs_rp.data)]
        valids.append(wb_v)
        magb = [Signal(7, name=f"magb_{u}") for u in range(S)]
        negb = [Signal(name=f"negb_{u}") for u in range(S)]
        nrold = [Signal(signed(8), name=f"nrold_{u}") for u in range(S)]
        tb = [Signal(signed(8), name=f"tb_{u}") for u in range(S)]
        xb = [Signal(5, name=f"xb_{u}") for u in range(S)]  # h, dmode, acc, valid
        for u in range(S):
            f = fu_rp.data[21 * u:21 * u + 21]
            t, rmag, rneg, h, dmode, acc, valid = (f[:8].as_signed(), f[8:15], f[15], f[16],
                                                   f[17], f[18:20], f[20])
            m.d.sync += [
                magb[u].eq(Mux(wa_k == rim[u], rm2[u], rm1[u])),
                negb[u].eq(rpar[u] ^ (t < 0)),
                nrold[u].eq(Mux(rneg, rmag, -rmag)),
                tb[u].eq(t), xb[u].eq(Cat(h, dmode, acc, valid)),
            ]
        # Wb: v = t + r_new, or r_new - r_old for a later edge of a shared bit;
        # the sign memory write; the rotation amount and the lane threshold
        wc_v = Signal(name="wc_v")
        wc_sh = Signal(fs_rp.data.shape(), name="wc_sh")
        m.d.sync += [wc_v.eq(wb_v), wc_sh.eq(wb_sh)]
        valids.append(wc_v)
        sh_e = wb_sh[gbits + 2 * stbits + ubits:]
        sh_b = wb_sh[gbits + 2 * stbits:gbits + 2 * stbits + ubits]
        m.d.comb += [sgn_wp.addr.eq(idle0(wb_v, sh_e)), sgn_wp.en.eq(wb_v),
                     sgn_wp.data.eq(cat([negb[u] & (magb[u] != 0) for u in range(S)], "sgncat"))]
        vc = [Signal(signed(10), name=f"vc_{u}") for u in range(S)]
        xc = [Signal(4, name=f"xc_{u}") for u in range(S)]  # h, acc, valid
        for u in range(S):
            h, dmode, acc, valid = xb[u][0], xb[u][1], xb[u][2:4], xb[u][4]
            base = Mux(dmode, nrold[u], tb[u])
            val = Signal(signed(10), name=f"val_{u}", attrs=NO_DSP)
            m.d.comb += val.eq(Mux(negb[u], base - magb[u], base + magb[u]))
            m.d.sync += [vc[u].eq(val), xc[u].eq(Cat(h, acc, valid))]
        amt_c = Signal(ubits)  # lane lam <- unit (lam + b) mod S: a rotation by S - b
        thr_c = Signal(sbits)
        m.d.sync += [amt_c.eq(Mux(sh_b == 0, 0, S - sh_b)), thr_c.eq(Const(S, sbits) - sh_b)]
        # Wc: the rotation's first half; the lane addresses
        wd_v = Signal(name="wd_v")
        m.d.sync += wd_v.eq(wc_v)
        valids.append(wd_v)
        c_g = wc_sh[:gbits]
        c_w0 = wc_sh[gbits:gbits + stbits]
        c_w1 = wc_sh[gbits + stbits:gbits + 2 * stbits]
        units = cat([Cat(vc[u], xc[u]) for u in range(S)], "ucat")
        rw1 = Signal(14 * S)
        amt_d = Signal(ubits)
        m.d.sync += [rw1.eq(_rotate(m, units, 14, S, amt_c, "rwa", cat, 0, half)), amt_d.eq(amt_c)]
        wd_addr = [Signal(abits, name=f"wda{lam}") for lam in range(S)]
        for lam in range(S):
            m.d.sync += wd_addr[lam].eq(Cat(Mux(lam >= thr_c, c_w1, c_w0), c_g))
        # Wd: the rotation's second half
        we_v = Signal(name="we_v", attrs=FANOUT)
        m.d.sync += we_v.eq(wd_v)
        valids.append(we_v)
        lanes_w = _rotate(m, rw1, 14, S, amt_d, "rwb", cat, half, bits_rot)
        vd = [Signal(signed(10), name=f"vd_{lam}") for lam in range(S)]
        xd = [Signal(4, name=f"xd_{lam}") for lam in range(S)]
        we_addr = [Signal(abits, name=f"wea{lam}") for lam in range(S)]
        for lam in range(S):
            m.d.sync += [vd[lam].eq(lanes_w[14 * lam:14 * lam + 10].as_signed()),
                         xd[lam].eq(lanes_w[14 * lam + 10:14 * lam + 14]),
                         we_addr[lam].eq(wd_addr[lam])]
        # We: each lane's sum onto the pending sums of the entries one and two
        # back (the delta write-back); the sum registered for the port stage
        wf_v = Signal(name="wf_v")
        m.d.sync += wf_v.eq(we_v)
        valids.append(wf_v)
        sreg = [Signal(signed(11), name=f"sreg_{lam}") for lam in range(S)]
        sh_h = [Signal(name=f"shh_{lam}") for lam in range(S)]
        s_en = [Signal(name=f"sen_{lam}") for lam in range(S)]
        wf_addr = [Signal(abits, name=f"wfaa{lam}") for lam in range(S)]
        for lam in range(S):
            p1 = Signal(signed(11), name=f"pend1_{lam}")
            p2 = Signal(signed(11), name=f"pend2_{lam}")
            h, acc, valid = xd[lam][0], xd[lam][1:3], xd[lam][3]
            ssum = Signal(signed(11), name=f"sum_{lam}", attrs=NO_DSP)
            m.d.comb += ssum.eq(vd[lam] + Mux(acc == 1, p1, Mux(acc == 2, p2, 0)))
            with m.If(we_v):
                m.d.sync += [p2.eq(p1), p1.eq(ssum)]
            m.d.sync += [sreg[lam].eq(ssum), sh_h[lam].eq(h), s_en[lam].eq(we_v & valid),
                         wf_addr[lam].eq(we_addr[lam])]
        # Wf: into the bank's port registers (the write happens at their
        # edge); the fill of the known zeros takes the same registers
        fill_v = Signal(attrs=FANOUT)
        fill_addr = Signal(abits, attrs=FANOUT)
        fill_lo = Signal(range(S + 1), attrs=FANOUT)
        fill_hi = Signal(range(S + 1), attrs=FANOUT)
        for lam in range(S):
            sat8 = Mux(sreg[lam] > 127, 127, Mux(sreg[lam] < -128, -128, sreg[lam]))
            known = (lam < fill_lo) | (lam >= fill_hi)
            m.d.comb += [
                wr_en[lam].eq(Mux(fill_v, known, s_en[lam])),
                wr_data[lam].eq(Mux(fill_v, Const(127, 9), Cat(sat8[:8], sh_h[lam]))),
                wr_addr[lam].eq(Mux(fill_v, fill_addr, wf_addr[lam])),
            ]
        port_v = Signal(name="port_v")
        m.d.sync += port_v.eq(wf_v)
        valids.append(port_v)
        # verification: the entry index of the reads at the memories (A) and of
        # the writes (the port registers); synth/ldpc_latency.py measures
        # WRITE_LATENCY with them
        wk_b, wk_c, wk_d, wk_e, wk_f, wk_p = (Signal(kbits, name=f"wk_{x}") for x in "bcdefp")
        m.d.sync += [wk_b.eq(wa_k), wk_c.eq(wk_b), wk_d.eq(wk_c), wk_e.eq(wk_d), wk_f.eq(wk_e),
                     wk_p.eq(wk_f)]
        self._probe = dict(read_v=x0["v"], read_k=x0["k"], write_v=port_v, write_k=wk_p,
                           issue=issue, pass_last=pass_last)

        # --- the engine's control --------------------------------------------
        # registered: in DRAIN nothing issues, so once every stage is idle it stays
        # idle, and a cycle's lag only delays the finish
        wpipe_busy = Signal()
        m.d.sync += wpipe_busy.eq(Cat(*valids).any())
        cyc = self.cycles
        drain_left = Signal(4)
        out_go = Signal()
        m.d.comb += out_go.eq(0)

        # --- the result queue: committed codewords, in order ------------------
        qb = _bits(QUEUE_DEPTH - 1)
        assert QUEUE_DEPTH == 1 << qb
        q_drop = Array([Signal(name=f"q{i}_drop") for i in range(QUEUE_DEPTH)])
        q_bank = Array([Signal(kb, name=f"q{i}_bank") for i in range(QUEUE_DEPTH)])
        q_rate = Array([Signal.like(self.st_rate, name=f"q{i}_rate") for i in range(QUEUE_DEPTH)])
        q_max = Array([Signal(ITER_BITS, name=f"q{i}_max") for i in range(QUEUE_DEPTH)])
        q_off = Array([Signal(OFFSET_BITS, name=f"q{i}_off") for i in range(QUEUE_DEPTH)])
        q_short = Array([Signal(name=f"q{i}_short") for i in range(QUEUE_DEPTH)])
        q_wr = Signal(qb + 1)  # next entry to write
        q_dq = Signal(qb + 1)  # next entry the engine takes
        q_oq = Signal(qb + 1)  # next entry whose result leaves
        cur_q = Signal(qb + 1)  # the entry the engine decodes
        e_iter = Signal(ITER_BITS)
        e_conv = Signal()
        e_unsat = Signal(16)
        head = q_dq[:qb]
        take = Signal()  # the engine takes the queue's next entry
        m.d.comb += take.eq(~busy & (q_dq != q_wr))
        drop_emit = Signal()  # the result record of a dropped codeword leaves
        m.d.comb += drop_emit.eq(~out_active & ~out_go & (q_oq != q_dq) & q_drop[q_oq[:qb]])
        info_start, info_len = self.shortened
        info_end = info_start + info_len
        # the fill: counters, then (registered) the clamped bounds and the
        # address, then the port registers
        fill_on = Signal()
        m.d.sync += [
            fill_v.eq(fill_on), fill_addr.eq(Cat(fill_w, fill_g)),
            fill_lo.eq(Mux(fill_base >= info_start, 0,
                           Mux(fill_base + S <= info_start, S, info_start - fill_base))),
            fill_hi.eq(Mux(fill_base >= info_end, 0,
                           Mux(fill_base + S <= info_end, S, info_end - fill_base))),
        ]
        dec_conv = Signal()
        at_limit = Signal()  # pass_no - 1 == max_iter
        with m.If(busy):
            m.d.sync += cyc.eq(cyc + 1)
        with m.If(take):
            m.d.sync += q_dq.eq(q_dq + 1)
        with m.If(take & ~q_drop[head]):
            m.d.sync += [
                busy.eq(1), dec_buf.eq(q_bank[head]), cur_q.eq(q_dq), rate.eq(q_rate[head]),
                max_iter.eq(q_max[head]), offset.eq(q_off[head]), short.eq(q_short[head]),
                phase.eq(SETUP), in_pass.eq(0), pass_no.eq(1), cyc.eq(0),
            ]
        with m.If(busy & (phase == SETUP)):
            # the rate's constants; the first pass's start (no wait: nothing
            # has been written yet)
            rd = [r.d for r in rates]
            rq_ = [r.q for r in rates]
            rG = [r.G for r in rates]
            m.d.sync += [
                dm1_r.eq(R("dm1", [d - 1 for d in rd])[rate]),
                dm2_r.eq(R("dm2", [d - 2 for d in rd])[rate]),
                qm2_r.eq(R("qm2", [max(q - 2, 0) for q in rq_])[rate]),
                G_r.eq(R("G", rG, gbits)[rate]),
                Gq1_r.eq(R("Gq1", [g + q - 1 for g, q in zip(rG, rq_)], gbits)[rate]),
                steps_m1_r.eq(R("ns", [r.steps - 1 for r in rates], stepbits)[rate]),
                info_b_r.eq(R("ib", info_base, ibits)[rate]),
                chain_b_r.eq(R("cb", chain_base, cbits)[rate]),
                gap_b_r.eq(R("gb", gap_base, gpbits)[rate]),
                g0_r.eq(R("g0", gap0, 8)[rate]),
                ll0_r.eq(R("ll0", [int(q == 1) for q in rq_])[rate]),
                layer_info.eq(R("ib", info_base, ibits)[rate]),
                layer_chain.eq(R("cb", chain_base, cbits)[rate]),
                gap_ptr.eq(R("gb", gap_base, gpbits)[rate]),
                krem.eq(R("dm1", [d - 1 for d in rd])[rate]),
                lastlayer.eq(R("ll0", [int(q == 1) for q in rq_])[rate]),
                layer.eq(0), st.eq(0), k.eq(0), n_idx.eq(0), e_idx.eq(0), in_step.eq(0),
                wait.eq(0), wz.eq(1), unsat.eq(0), fill_g.eq(0), fill_w.eq(0), fill_base.eq(0),
                push_ptr.eq(0), pop_ptr.eq(0), at_limit.eq(max_iter == 0),
                phase.eq(Mux(short, FILL, PASS)), in_pass.eq(~short),
            ]
        m.d.comb += fill_on.eq(busy & (phase == FILL))
        with m.If(fill_on):
            m.d.sync += fill_base.eq(fill_base + S)
            with m.If(fill_w == W - 1):
                m.d.sync += [fill_w.eq(0), fill_g.eq(fill_g + 1)]
                with m.If(fill_g == G_r - 1):
                    m.d.sync += [phase.eq(PASS), in_pass.eq(1)]
            with m.Else():
                m.d.sync += fill_w.eq(fill_w + 1)
        with m.If(busy & (phase == WAITD) & pass_end):
            # the pass's syndrome weight (iteration pass_no - 1) is complete
            m.d.sync += [phase.eq(DECIDE_), dec_conv.eq(unsat == 0)]
        with m.If(busy & (phase == DECIDE_)):
            with m.If(dec_conv | at_limit):
                m.d.sync += [phase.eq(DRAIN), drain_left.eq(3), e_iter.eq(pass_no - 1),
                             e_conv.eq(dec_conv), e_unsat.eq(unsat)]
            with m.Else():
                m.d.sync += [phase.eq(PASS), in_pass.eq(1), pass_no.eq(pass_no + 1),
                             at_limit.eq(pass_no == max_iter),
                             layer.eq(0), st.eq(0), k.eq(0), krem.eq(dm1_r), n_idx.eq(0),
                             e_idx.eq(0), unsat.eq(0), layer_info.eq(info_b_r),
                             layer_chain.eq(chain_b_r), lastlayer.eq(ll0_r)]
        with m.If(busy & (phase == DRAIN)):
            with m.If(~wpipe_busy):
                # the result leaves once every earlier one has (in order) and
                # the output stream is free
                with m.If(drain_left == 0):
                    with m.If(~out_active & (q_oq == cur_q)):
                        m.d.sync += busy.eq(0)
                        m.d.comb += out_go.eq(1)
                with m.Else():
                    m.d.sync += drain_left.eq(drain_left - 1)

        # --- the output stream -------------------------------------------------
        # Reads of the output bank through its address register and its
        # output register (OLAT cycles from the request to the data), into a
        # prefetch queue of OQ words and a head word; the packer takes the
        # head whenever fewer than 16 bits wait. `credits` counts the free
        # places (queue + head) not yet promised to a read under way.
        OLAT = 4
        OQ = 8
        og = Signal(gbits, attrs=FANOUT)
        ow = Signal(wbits, attrs=FANOUT)
        o_more = Signal()  # words left to read
        o_wait = Signal(2)  # cycles before the first read (dsel's lag)
        ov = Signal(OLAT)  # reads under way, by age
        credits = Signal(range(OQ + 2))
        oq_mem = Array([Signal(S, name=f"oq{i}") for i in range(OQ)])
        oq_w = Signal(_bits(OQ))
        oq_r = Signal(_bits(OQ))
        hd = Signal(S)
        hd_v = Signal()
        buf = Signal(S + 16)
        cnt = Signal(range(S + 17))
        bits_left = Signal(17)
        drained = Signal()  # nothing under way, queued or held (credits == OQ + 1)
        # the packer's comparisons of cnt, registered with it: >= 16, < 32,
        # != 0, and cnt - 16
        ge16 = Signal()
        lt32 = Signal()
        nz = Signal()
        cm16 = Signal(4)
        m.d.comb += out_addr.eq(Cat(ow, og))
        m.d.sync += self.res_valid.eq(out_go | drop_emit)
        with m.If(out_go | drop_emit):
            m.d.sync += q_oq.eq(q_oq + 1)
        with m.If(out_go):
            m.d.sync += [out_active.eq(1), out_buf.eq(dec_buf), og.eq(0), ow.eq(0), o_more.eq(1),
                         o_wait.eq(3), cnt.eq(0), bits_left.eq(self.n), credits.eq(OQ + 1),
                         ge16.eq(0), lt32.eq(1), nz.eq(0),
                         hd_v.eq(0), oq_w.eq(0), oq_r.eq(0), drained.eq(1),
                         self.res_iterations.eq(e_iter), self.res_converged.eq(e_conv),
                         self.res_unsat.eq(e_unsat), self.res_overrun.eq(0)]
        with m.If(drop_emit):
            m.d.sync += [self.res_iterations.eq(0), self.res_converged.eq(0),
                         self.res_unsat.eq(0), self.res_overrun.eq(1)]
        free_mask = Signal(K)  # the bank whose output stream ends this cycle
        emit = Signal()
        flush = ~o_more & drained & nz
        m.d.comb += [
            emit.eq(out_active & self.o_ready & (ge16 | flush)),
            self.o_valid.eq(out_active & (ge16 | flush)),
            self.o_data.eq(buf[:16]),
            self.o_last.eq(bits_left <= 16),
        ]
        oread = Signal()
        # (out_go only comes with the stream idle: no read or packing then)
        m.d.comb += oread.eq(out_active & o_more & (o_wait == 0) & (credits != 0))
        hq = Signal(S)  # the output bank's hard decisions, registered
        osel = Signal(kb, attrs=FANOUT)  # out_buf, a copy for the bank mux
        m.d.sync += osel.eq(out_buf)
        m.d.sync += hq.eq(cat([Array([bq[b][lam][8] for b in range(K)])[osel]
                               for lam in range(S)], "hbcat"))
        arrive = ov[OLAT - 1]
        m.d.sync += ov.eq(Cat(oread, ov[:OLAT - 1]))
        cnt_after = Signal.like(cnt)
        buf_after = Signal.like(buf)
        m.d.comb += [cnt_after.eq(Mux(emit, Mux(ge16, cnt - 16, 0), cnt)),
                     buf_after.eq(Mux(emit, buf >> 16, buf))]
        take_hd = Signal()  # the packer takes the head word (when cnt_after < 16)
        m.d.comb += take_hd.eq(out_active & hd_v & Mux(emit, lt32 | ~ge16, ~ge16))
        # the head word at cnt_after (< 16): from the registered cnt or cnt - 16
        placed = Mux(emit, hd << cm16, hd << cnt[:4])
        oq_has = oq_w != oq_r
        refill = Signal()  # the head is (re)loaded from the queue
        m.d.comb += refill.eq(oq_has & (~hd_v | take_hd))
        with m.If(out_active):
            with m.If(o_wait != 0):
                m.d.sync += o_wait.eq(o_wait - 1)
            with m.If(arrive):
                m.d.sync += [oq_mem[oq_w[:_bits(OQ - 1)]].eq(hq), oq_w.eq(oq_w + 1)]
            with m.If(refill):
                m.d.sync += [hd.eq(oq_mem[oq_r[:_bits(OQ - 1)]]), hd_v.eq(1), oq_r.eq(oq_r + 1)]
            with m.Elif(take_hd):
                m.d.sync += hd_v.eq(0)
            cnt_next = Signal.like(cnt)
            m.d.comb += cnt_next.eq(cnt_after + Mux(take_hd, S, 0))
            m.d.sync += [buf.eq(buf_after | Mux(take_hd, placed, 0)), cnt.eq(cnt_next),
                         ge16.eq(cnt_next >= 16), lt32.eq(cnt_next < 32), nz.eq(cnt_next != 0),
                         cm16.eq((cnt_next - 16)[:4])]
            credits_next = Signal.like(credits)
            m.d.comb += credits_next.eq(credits - oread + take_hd)
            m.d.sync += [credits.eq(credits_next), drained.eq(credits_next == OQ + 1)]
            with m.If(oread):
                with m.If(ow == W - 1):
                    m.d.sync += [ow.eq(0), og.eq(og + 1)]
                    with m.If(og == NG - 1):
                        m.d.sync += o_more.eq(0)
                with m.Else():
                    m.d.sync += ow.eq(ow + 1)
            with m.If(emit):
                m.d.sync += bits_left.eq(bits_left - 16)
                with m.If(self.o_last):
                    m.d.sync += out_active.eq(0)
                    m.d.comb += free_mask.eq(1 << out_buf)
        # --- the input mapping (position -> lane, address) ----------------------
        # information (pos < k): g = pos div 374, j = pos mod 374; parity
        # (c = pos - k): g = G + (c mod q), j = c div q; then the word j div S
        # and the lane j mod S (a ROM). Both divisions restoring, one quotient
        # bit per stage (pos div 374 < 128, c div q < 374 < 512): 9 stages.
        # bank allocation at `begin`, per load context: the lowest free bank,
        # or the codeword is dropped (no bank, counted, its result an overrun
        # record in order); a context still holding the bank of a codeword
        # it gave up takes that one again, and `abort` frees it (docstring,
        # "Abandoned codewords")
        alloc = Signal(K)  # 1: the bank holds a codeword (loading .. output)
        ctx_bank = Array([Signal(kb, name=f"ctx{c}_bank") for c in range(2)])
        ctx_drop = Array([Signal(name=f"ctx{c}_drop", init=1) for c in range(2)])
        ctx_open = Array([Signal(name=f"ctx{c}_open") for c in range(2)])  # begun, not committed
        full = alloc.all()
        lowest = Signal(kb)
        for b in reversed(range(K)):
            with m.If(~alloc[b]):
                m.d.comb += lowest.eq(b)
        set_mask = Signal(K)
        abort_mask = Signal(K)  # the bank `abort` frees this cycle
        holds = Signal()  # in_ctx holds the bank of an uncommitted codeword
        reuse = Signal()  # this `begin` takes that bank again
        m.d.comb += [holds.eq(ctx_open[self.in_ctx] & ~ctx_drop[self.in_ctx]),
                     reuse.eq(self.begin & holds & ~self.start)]
        fresh = self.begin & ~reuse
        m.d.comb += self.load_ready.eq(~full)
        with m.If(self.begin):
            m.d.sync += ctx_open[self.in_ctx].eq(1)
            with m.If(reuse):
                m.d.sync += self.reused.eq(self.reused + 1)
            with m.Else():
                m.d.sync += [ctx_bank[self.in_ctx].eq(lowest), ctx_drop[self.in_ctx].eq(full)]
                with m.If(full):
                    m.d.sync += self.overruns.eq(self.overruns + 1)
                with m.Else():
                    m.d.comb += set_mask.eq(1 << lowest)
        with m.Elif(self.start):
            m.d.sync += ctx_open[self.in_ctx].eq(0)
        with m.Elif(self.abort):
            m.d.sync += [ctx_open[self.in_ctx].eq(0), ctx_drop[self.in_ctx].eq(1)]
            with m.If(holds):
                m.d.comb += abort_mask.eq(1 << ctx_bank[self.in_ctx])
                m.d.sync += self.aborted.eq(self.aborted + 1)
        m.d.sync += alloc.eq((alloc | set_mask) & ~free_mask & ~abort_mask)
        m.d.sync += self.held.eq(sum(alloc[b] for b in range(K)))
        w_bank = Mux(fresh, lowest, ctx_bank[self.in_ctx])
        w_drop = Mux(fresh, full, ctx_drop[self.in_ctx])

        in_k = R("ik", [r.k for r in rates], 16)[self.in_rate]
        in_G = R("iG", [r.G for r in rates], gbits)[self.in_rate]
        in_q = R("iq", [r.q for r in rates])[self.in_rate]
        st0 = dict(v=Signal(name="ld_v"), llr=Signal(signed(8), name="ld_llr"),
                   isp=Signal(name="ld_isp"), G=Signal(gbits, name="ld_G"),
                   q=Signal(len(in_q), name="ld_q"), r=Signal(16, name="ld_r"),
                   quo=Signal(9, name="ld_quo"), bank=Signal(kb, name="ld_bank"),
                   tok=Signal(name="ld_tok"), tdrop=Signal(name="ld_tdrop"),
                   tbank=Signal(kb, name="ld_tbank"), trate=Signal.like(self.st_rate),
                   tmax=Signal(ITER_BITS), toff=Signal(OFFSET_BITS), tshort=Signal())
        isp = self.in_pos >= in_k
        # the commit token travels with the writes, so the entry enters the
        # queue after the codeword's last write has reached its bank
        m.d.sync += [
            st0["v"].eq(self.in_valid & ~w_drop), st0["llr"].eq(self.in_llr),
            st0["isp"].eq(isp), st0["G"].eq(in_G), st0["q"].eq(in_q),
            st0["r"].eq(Mux(isp, self.in_pos - in_k, self.in_pos)), st0["quo"].eq(0),
            st0["bank"].eq(w_bank), st0["tok"].eq(self.start),
            st0["tdrop"].eq(ctx_drop[self.in_ctx]), st0["tbank"].eq(ctx_bank[self.in_ctx]),
            st0["trate"].eq(self.st_rate), st0["tmax"].eq(self.st_max_iter),
            st0["toff"].eq(self.st_offset), st0["tshort"].eq(self.st_short),
        ]
        passthru = ("v", "llr", "isp", "G", "q", "bank", "tok", "tdrop", "tbank", "trate",
                    "tmax", "toff", "tshort")
        cur = st0
        for i in range(8, -1, -1):
            dv = Mux(cur["isp"], cur["q"] << i, GROUP << i if i <= 6 else (1 << 17) - 1)
            ge = cur["r"] >= dv
            nxt = {key: Signal.like(sig, name=f"ld{i}_{key}") for key, sig in cur.items()}
            for key in passthru:
                m.d.sync += nxt[key].eq(cur[key])
            m.d.sync += [nxt["r"].eq(Mux(ge, cur["r"] - dv, cur["r"])),
                         nxt["quo"].eq(cur["quo"] | (ge << i))]
            cur = nxt
        g_ld = Mux(cur["isp"], cur["G"] + cur["r"], cur["quo"])
        j_ld = Mux(cur["isp"], cur["quo"], cur["r"])
        regs = cur
        jdm = Memory(shape=stbits + ubits, depth=deep(GROUP),
                     init=[(j // S) | (j % S) << stbits for j in range(GROUP)])
        m.submodules.jdm = jdm
        jrp = jdm.read_port(domain="comb")
        m.d.comb += jrp.addr.eq(j_ld)
        fin = dict(v=Signal(name="ldf_v", attrs=FANOUT), g=Signal(gbits, name="ldf_g", attrs=FANOUT),
                   w=Signal(stbits, name="ldf_w", attrs=FANOUT),
                   lane=Signal(ubits, name="ldf_lane", attrs=FANOUT),
                   llr=Signal(signed(8), name="ldf_llr", attrs=FANOUT))
        fin_bank = Signal(kb, name="ldf_bank", attrs=FANOUT)
        m.d.sync += [fin["v"].eq(regs["v"]), fin["g"].eq(g_ld), fin["w"].eq(jrp.data[:stbits]),
                     fin["lane"].eq(jrp.data[stbits:]), fin["llr"].eq(regs["llr"]),
                     fin_bank.eq(regs["bank"])]
        m.d.comb += [ld_addr.eq(Cat(fin["w"][:wbits], fin["g"])), ld_data.eq(Cat(fin["llr"], 0)),
                     ld_lane.eq(fin["lane"]), ld_en.eq(fin["v"]), ld_bank.eq(fin_bank)]
        # the token one stage later than the last write: into the queue
        tok = {key: Signal.like(regs[key], name=f"tk_{key}") for key in passthru[6:]}
        for key in tok:
            m.d.sync += tok[key].eq(regs[key])
        slot = q_wr[:qb]
        with m.If(tok["tok"]):
            with m.If((q_wr - q_oq)[:qb + 1] == QUEUE_DEPTH):
                m.d.sync += self.queue_overflow.eq(1)
            with m.Else():
                m.d.sync += [
                    q_wr.eq(q_wr + 1), q_drop[slot].eq(tok["tdrop"]), q_bank[slot].eq(tok["tbank"]),
                    q_rate[slot].eq(tok["trate"]), q_max[slot].eq(tok["tmax"]),
                    q_off[slot].eq(tok["toff"]), q_short[slot].eq(tok["tshort"]),
                ]
        return m
