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
decision of the previous iteration, below). Two such banks (double
buffer): one loads the next codeword while the other decodes and then
streams its result. A step of layer l reads, per table entry (g, s = a S +
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

Schedule, cycle by cycle (entry k of a step; A = the cycle its lane
addresses reach the memories; one entry per cycle, a step's entries on
consecutive cycles, steps back to back unless the hazard gap says
otherwise):

  I0  sequencer: step (layer, st), entry k; info ROM address
  I1  ROM data: g, s, first, chain; (a, b) = divmod(s, S) by a LUT ROM;
      chain ROM address (a running pointer over the layer's chain entries)
  I2  w0 = (st - a) mod W, w1 = (st - a - 1) mod W; per lane the address
      {g, lam >= S - b ? w1 : w0}
  A   lane read addresses; record memory (k = 0) and sign memory read
  A+1 lane data rotated to the units (by b); r_old; per-unit flags (first
      read, valid, delta mode) from the chain thresholds
  A+2 t = sat(L - r_old), the old hard decision h
  A+3 |t| (saturating); t, r_old, h, flags into a 64-deep FIFO
  A+4 min1, min2, imin, sign parity, syndrome parity (reset at k = 0)
  L   (= A+5 of the step's last entry) latch the check's result (offset
      applied), write the record; the syndrome popcount starts
  L+1+k   the FIFO's entry k: r_new
  L+2+k   v = t + r_new or r_new - r_old; the sign memory write
  L+3+k   rotated back to the lanes; the lane write addresses
  L+4+k   per lane: sum = v + (the pending sum of the entry 1 or 2 back);
          write {h, sat8(sum)}

So entry k is written WRITE_LATENCY = 8 cycles after d + k cycles past its
read (d the layer degree). A step may read a word only after the last
write of it: `ldpcsched.gaps` computes, at elaboration, the idle cycles
before each step (a steady state over passes; most steps need none) and
the gap ROM holds the nonzero ones. A pass's first step also waits for the
previous pass's decision (DECIDE = 12 cycles after its last step's
reads end, plus the ROM gap).

Clock target 200 MHz: every stage above is at most one carry chain plus a
few LUT levels (the rotators are three LUT levels of 4:1 muxes at S = 34).
No DSP: the input address mapping divides by 374 and by q with pipelined
restoring dividers.

Ports (all in the sync domain):
  in_valid, in_pos (16), in_llr (i8), in_rate (rate index), in_ready
      one LLR per cycle into the loading bank, any order; the codeword's
      rate must be on in_rate (the parity positions map through q);
  start, st_rate, st_max_iter (8), st_offset (7), st_short, start_ready
      decode the loaded bank; st_short: the shortened TMCC code: the
      decoder writes +127 itself into the known-zero positions
      [0, info_start) and [info_start + info_len, k) (`shortened`, by
      default (1870, 9614)) before the first pass, so only the 31 680 sent
      positions need loading;
  o_valid, o_data (16), o_last, o_ready
      the hard decisions, 16 per word, bit 0 first: the NG groups in
      order, 374 bits each (so the information bits in codeword order,
      then the parity in layer order: p_l, p_{l+q}, ... per l; the bits
      of the last word past n are 0);
  res_valid (one cycle, before the first word), res_iterations,
  res_converged, res_unsat
      the codeword's result;
  busy, passes, cycles (verification: the engine's passes and cycles of
      the last codeword).
"""

import os

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from . import ldpcmodel, ldpcsched
from .ldpcsched import GROUP

WRITE_LATENCY = 8
DECIDE = 12
FIFO_DEPTH = 64
OFFSET_BITS = 7
ITER_BITS = 8
NO_DSP = {"use_dsp": "no"}


def _bits(x):
    return max(1, int(x).bit_length())


def _rotate(m, vec, width, n, amount, name):
    """vec (n fields of `width` bits) rotated so that field i of the result
    is field (i - amount) mod n of vec (amount < n): a barrel rotator, one
    2:1 mux level per bit of `amount`."""
    cur = vec
    for i in range(_bits(n - 1)):
        sh = 1 << i
        if sh >= n:
            break
        rot = Cat(*[cur[width * ((f - sh) % n):width * ((f - sh) % n) + width] for f in range(n)])
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

    def __init__(self, units, codes=None, ngroups=120, shortened=ldpcmodel.TMCC_SHORT):
        self.S = S = units
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
        assert self.max_d <= 32 and self.max_d + 2 * WRITE_LATENCY < FIFO_DEPTH
        nr = len(self.rates)
        self.in_valid = Signal()
        self.in_pos = Signal(16)
        self.in_llr = Signal(signed(8))
        self.in_rate = Signal(range(max(nr, 2)))
        self.in_ready = Signal()
        self.start = Signal()
        self.st_rate = Signal(range(max(nr, 2)))
        self.st_max_iter = Signal(ITER_BITS)
        self.st_offset = Signal(OFFSET_BITS)
        self.st_short = Signal()
        self.start_ready = Signal()
        self.o_valid = Signal()
        self.o_data = Signal(16)
        self.o_last = Signal()
        self.o_ready = Signal()
        self.res_valid = Signal()
        self.res_iterations = Signal(ITER_BITS)
        self.res_converged = Signal()
        self.res_unsat = Signal(16)
        self.busy = Signal()
        self.passes = Signal(ITER_BITS + 1)
        self.cycles = Signal(24)

    def ports(self):
        return [self.in_valid, self.in_pos, self.in_llr, self.in_rate, self.in_ready, self.start,
                self.st_rate, self.st_max_iter, self.st_offset, self.st_short, self.start_ready,
                self.o_valid, self.o_data, self.o_last, self.o_ready, self.res_valid,
                self.res_iterations, self.res_converged, self.res_unsat, self.busy]

    def elaborate(self, platform):
        m = Module()
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
            for i, g in enumerate(r.gaps):
                if g:
                    gap_rows.append((i, g))
            gap_rows.append(((1 << stepbits) - 1, 0))  # sentinel
        gapbits = _bits(max([g for _, g in gap_rows] + [1]))

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

        info_rom = Memory(shape=gbits + 11, depth=len(info_init) + 34, init=info_init)
        chain_init = [ch_pack(e) for e in chain_rows] or [0]
        chain_rom = Memory(shape=ch_width, depth=max(len(chain_init), 2), init=chain_init)
        gap_rom = Memory(shape=stepbits + gapbits, depth=len(gap_rows),
                         init=[i | g << stepbits for i, g in gap_rows])
        divmod_rom = Memory(shape=stbits + ubits, depth=GROUP,
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
        offset = Signal(OFFSET_BITS)
        short = Signal()
        r_G = R("G", [r.G for r in rates])[rate]
        r_q = R("q", [r.q for r in rates])[rate]
        r_d = R("d", [r.d for r in rates])[rate]
        r_info = R("ib", info_base + [0])[rate]
        r_chain = R("cb", chain_base + [0])[rate]
        r_gap = R("gb", gap_base + [0])[rate]
        r_steps = R("ns", [r.steps for r in rates])[rate]

        # --- the banks -------------------------------------------------------
        banks = []
        for b in range(2):
            lanes = []
            for lam in range(S):
                mem = Memory(shape=9, depth=NG * Wp, init=[])
                m.submodules[f"l{b}_{lam}"] = mem
                lanes.append((mem.read_port(), mem.write_port()))
            banks.append(lanes)
        rec_mem = Memory(shape=19 * S, depth=max_steps, init=[])
        sign_mem = Memory(shape=S, depth=max_edges, init=[])
        fifo_u = Memory(shape=21 * S, depth=FIFO_DEPTH, init=[])
        fifo_s = Memory(shape=gbits + 2 * stbits + ubits + edgebits, depth=FIFO_DEPTH, init=[])
        m.submodules.rec_mem = rec_mem
        m.submodules.sign_mem = sign_mem
        m.submodules.fifo_u = fifo_u
        m.submodules.fifo_s = fifo_s
        rec_rp, rec_wp = rec_mem.read_port(), rec_mem.write_port()
        sgn_rp, sgn_wp = sign_mem.read_port(), sign_mem.write_port()
        fu_rp, fu_wp = fifo_u.read_port(), fifo_u.write_port()
        fs_rp, fs_wp = fifo_s.read_port(), fifo_s.write_port()

        load_buf = Signal()
        dec_buf = Signal()
        out_buf = Signal()
        out_active = Signal()
        busy = self.busy

        # per-bank port drive (filled below)
        dec_raddr = [Signal(abits, name=f"dra{lam}") for lam in range(S)]
        dec_waddr = [Signal(abits, name=f"dwa{lam}") for lam in range(S)]
        dec_wdata = [Signal(9, name=f"dwd{lam}") for lam in range(S)]
        dec_wen = [Signal(name=f"dwe{lam}") for lam in range(S)]
        out_raddr = Signal(abits)
        ld_addr = Signal(abits)
        ld_data = Signal(9)
        ld_lane = Signal(range(S))
        ld_en = Signal()
        for b in range(2):
            mine = busy & (dec_buf == b)
            for lam, (rp, wp) in enumerate(banks[b]):
                m.d.comb += [
                    rp.addr.eq(Mux(mine, dec_raddr[lam], out_raddr)),
                    wp.addr.eq(Mux(mine, dec_waddr[lam], ld_addr)),
                    wp.data.eq(Mux(mine, dec_wdata[lam], ld_data)),
                    wp.en.eq(Mux(mine, dec_wen[lam], ld_en & (load_buf == b) & (ld_lane == lam))),
                ]
        rdata = [Mux(dec_buf, banks[1][lam][0].data, banks[0][lam][0].data) for lam in range(S)]
        odata = [Mux(out_buf, banks[1][lam][0].data, banks[0][lam][0].data) for lam in range(S)]

        # --- the sequencer (I0) ----------------------------------------------
        FILL, PASS, WAITD, DRAIN = range(4)
        phase = Signal(2)
        layer = Signal(lbits)
        st = Signal(stbits)
        k = Signal(kbits)
        n_idx = Signal(stepbits)
        e_idx = Signal(edgebits)
        in_step = Signal()  # entries 1 .. d-1 of a step remain
        idle = Signal(8)
        layer_info = Signal(_bits(max(len(info_init), 2)))
        pass_no = self.passes
        decided = Signal()
        gap_ptr = Signal(_bits(len(gap_rows)))
        fill_g = Signal(gbits)
        fill_w = Signal(wbits)
        fill_base = Signal(16)

        gap_step = gap_rp.data[:stepbits]
        gap_val = gap_rp.data[stepbits:]
        need = Mux(gap_step == n_idx, gap_val, 0)
        issue = Signal()
        m.d.comb += [
            gap_rp.addr.eq(gap_ptr),
            issue.eq(busy & (phase == PASS) & (in_step | ((idle >= need) & (decided | (n_idx != 0))))),
        ]
        last_k = k == r_d - 1
        last_st = st == W - 1
        last_layer = layer == r_q - 1

        # entry kind at I0: 0 information, 1 p_{c-1}, 2 p_c
        kind0 = Mux(k == r_d - 1, 2, Mux(k == r_d - 2, 1, 0))
        m.d.comb += info_rp.addr.eq(layer_info + k)

        with m.If(issue):
            m.d.sync += [idle.eq(0), e_idx.eq(e_idx + 1)]
            with m.If((k == 0) & (gap_step == n_idx)):
                m.d.sync += gap_ptr.eq(gap_ptr + 1)
            with m.If(last_k):
                m.d.sync += [k.eq(0), in_step.eq(0), n_idx.eq(n_idx + 1)]
                with m.If(last_st):
                    m.d.sync += [st.eq(0), layer.eq(layer + 1), layer_info.eq(layer_info + r_d - 2)]
                    with m.If(last_layer):
                        m.d.sync += [phase.eq(WAITD), decided.eq(0)]
                with m.Else():
                    m.d.sync += st.eq(st + 1)
            with m.Else():
                m.d.sync += [k.eq(k + 1), in_step.eq(1)]
        with m.Elif(idle != 255):
            m.d.sync += idle.eq(idle + 1)

        # I0 -> I1
        i1 = dict(v=Signal(), kind=Signal(2), layer=Signal(lbits), st=Signal(stbits),
                  k=Signal(kbits), n=Signal(stepbits), e=Signal(edgebits), last=Signal(),
                  p1=Signal(), lastlayer=Signal())
        m.d.sync += [
            i1["v"].eq(issue), i1["kind"].eq(kind0), i1["layer"].eq(layer), i1["st"].eq(st),
            i1["k"].eq(k), i1["n"].eq(n_idx), i1["e"].eq(e_idx), i1["last"].eq(last_k),
            i1["p1"].eq(pass_no == 1), i1["lastlayer"].eq(last_layer),
        ]
        # I1: ROM data; parity entries generated
        rom_g = info_rp.data[:gbits]
        rom_s = info_rp.data[gbits:gbits + 9]
        rom_first = info_rp.data[gbits + 9]
        rom_chain = info_rp.data[gbits + 10]
        is_info = i1["kind"] == 0
        g1 = Signal(gbits)
        s1 = Signal(9)
        first1 = Signal()
        m.d.comb += [
            g1.eq(Mux(is_info, rom_g,
                      Mux(i1["kind"] == 2, r_G + i1["layer"],
                          Mux(i1["layer"] == 0, r_G + r_q - 1, r_G + i1["layer"] - 1)))),
            s1.eq(Mux(is_info, rom_s, (i1["kind"] == 1) & (i1["layer"] == 0))),
            first1.eq(Mux(is_info, rom_first,
                          Mux(i1["kind"] == 1, i1["layer"] == 0, ~i1["lastlayer"]))),
            dm_rp.addr.eq(s1),
        ]
        chain_ptr = Signal(_bits(max(len(chain_init), 2)))
        layer_chain = Signal.like(chain_ptr)
        ch_addr = Mux(i1["k"] == 0, layer_chain, chain_ptr)
        is_chain = is_info & rom_chain
        m.d.comb += chain_rp.addr.eq(ch_addr)
        with m.If(i1["v"]):
            m.d.sync += chain_ptr.eq(ch_addr + is_chain)
            with m.If(i1["last"] & (i1["st"] == W - 1)):
                m.d.sync += layer_chain.eq(ch_addr + is_chain)

        def pipe(src, name):
            out = {}
            for key, sig in src.items():
                r = Signal.like(sig, name=f"{name}_{key}")
                m.d.sync += r.eq(sig)
                out[key] = r
            return out

        i2 = pipe(dict(i1, g=g1, first=first1, chain=is_chain,
                       absent=(i1["kind"] == 1) & (i1["layer"] == 0),
                       firstend=(i1["kind"] == 2) & i1["lastlayer"],
                       a=dm_rp.data[:stbits], b=dm_rp.data[stbits:]), "i2")
        # I2: the two candidate words and the lane addresses; the chain ROM data
        w0 = Signal(stbits)
        w1 = Signal(stbits)
        diff0 = Signal(signed(stbits + 2))
        m.d.comb += diff0.eq(i2["st"] - i2["a"])
        m.d.comb += [
            w0.eq(Mux(diff0 < 0, diff0 + W, diff0)),
            w1.eq(Mux(diff0 < 1, diff0 + W - 1, diff0 - 1)),
        ]
        thr = Signal(sbits)
        m.d.comb += thr.eq(S - i2["b"])
        for lam in range(S):
            m.d.sync += dec_raddr[lam].eq(Cat(Mux(lam >= thr, w1, w0), i2["g"]))
        ch = ch_unpack(chain_rp.data)
        x0 = pipe(dict(i2, w0=w0, w1=w1, **{f"c_{n}": Mux(i2["chain"], v, Const(
            {"eq0": W, "eq1": W, "td": S, "lo1": S, "lo2": S}.get(n, 0), len(v)))
            for n, v in ch.items()}), "x0")
        # A (= X0): lane addresses at the memories; record and sign reads
        m.d.comb += [rec_rp.addr.eq(x0["n"]), sgn_rp.addr.eq(x0["e"])]
        x1 = pipe(x0, "x1")

        # --- X1: rotate lanes to units, r_old, the per-unit flags ------------
        rec_hold = Signal(19 * S)
        rec_now = Mux(x1["k"] == 0, rec_rp.data, rec_hold)
        with m.If(x1["v"] & (x1["k"] == 0)):
            m.d.sync += rec_hold.eq(rec_rp.data)
        L2 = [Signal(signed(8), name=f"L2_{u}") for u in range(S)]
        H2 = [Signal(name=f"H2_{u}") for u in range(S)]
        rneg2 = [Signal(name=f"rn2_{u}") for u in range(S)]
        rmag2 = [Signal(7, name=f"rm2_{u}") for u in range(S)]
        fl2 = [dict(first=Signal(name=f"f2_{u}"), valid=Signal(name=f"v2_{u}"),
                    dmode=Signal(name=f"d2_{u}"), acc=Signal(2, name=f"a2_{u}")) for u in range(S)]
        bsel = x1["b"]
        rotated = Signal(9 * S)
        # unit u <- lane (u - b) mod S: a right rotation of the lane vector by b
        m.d.comb += rotated.eq(_rotate(m, Cat(*rdata), 9, S, bsel, "rr"))
        gt = [x1["st"] > x1["c_eq0"], x1["st"] > x1["c_eq1"]]
        eq = [x1["st"] == x1["c_eq0"], x1["st"] == x1["c_eq1"]]
        for u in range(S):
            word = rotated[9 * u:9 * u + 9]
            rec = rec_now[19 * u:19 * u + 19]
            mag = Mux(x1["k"] == rec[14:19], rec[7:14], rec[0:7])
            excl = Const(0)
            for i in range(2):
                er, sm = x1[f"c_er{i}"], x1[f"c_sm{i}"]
                excl = excl | ((gt[i] | (eq[i] & (u >= er))) & (~sm | (u < er)))
            first = Mux(x1["firstend"], (x1["st"] == W - 1) & Const(int(u == S - 1), 1), x1["first"] & ~excl)
            acc = Mux((u >= x1["c_lo1"]) | (u < x1["c_hi1"]), 1,
                      Mux((u >= x1["c_lo2"]) | (u < x1["c_hi2"]), 2, 0))
            m.d.sync += [
                L2[u].eq(word[:8]), H2[u].eq(word[8]),
                rneg2[u].eq(sgn_rp.data[u] & ~x1["p1"]),
                rmag2[u].eq(Mux(x1["p1"], 0, mag)),
                fl2[u]["first"].eq(first),
                fl2[u]["valid"].eq(~(x1["absent"] & (x1["st"] == 0) & Const(int(u == 0), 1))),
                fl2[u]["dmode"].eq(u >= x1["c_td"]),
                fl2[u]["acc"].eq(acc),
            ]
        x2 = pipe(x1, "x2")

        # --- X2: t and the old hard decision ---------------------------------
        t3 = [Signal(signed(8), name=f"t3_{u}") for u in range(S)]
        h3 = [Signal(name=f"h3_{u}") for u in range(S)]
        rneg3 = [Signal(name=f"rn3_{u}") for u in range(S)]
        rmag3 = [Signal(7, name=f"rm3_{u}") for u in range(S)]
        fl3 = [{n: Signal.like(s, name=f"{n}3_{u}") for n, s in fl2[u].items()} for u in range(S)]
        for u in range(S):
            tw = Signal(signed(10), name=f"tw_{u}", attrs=NO_DSP)
            m.d.comb += tw.eq(Mux(rneg2[u], L2[u] + rmag2[u], L2[u] - rmag2[u]))
            sat_t = Mux(tw > 127, 127, Mux(tw < -128, -128, tw))
            valid = fl2[u]["valid"]
            m.d.sync += [
                t3[u].eq(Mux(valid, sat_t, 127)),
                h3[u].eq(valid & Mux(fl2[u]["first"], L2[u] < 0, H2[u])),
                rneg3[u].eq(rneg2[u]), rmag3[u].eq(rmag2[u]),
            ]
            m.d.sync += [fl3[u][n].eq(fl2[u][n]) for n in fl2[u]]
        x3 = pipe(x2, "x3")

        # --- X3: |t|, the FIFO push -----------------------------------------
        a4 = [Signal(7, name=f"a4_{u}") for u in range(S)]
        neg4 = [Signal(name=f"ng4_{u}") for u in range(S)]
        h4 = [Signal(name=f"h4_{u}") for u in range(S)]
        push_ptr = Signal(6)
        fu_word = []
        for u in range(S):
            t = t3[u]
            m.d.sync += [
                a4[u].eq(Mux(t == -128, 127, Mux(t < 0, -t, t))),
                neg4[u].eq(t < 0), h4[u].eq(h3[u]),
            ]
            fu_word.append(Cat(t, rmag3[u], rneg3[u], h3[u], fl3[u]["dmode"], fl3[u]["acc"],
                               fl3[u]["valid"]))
        m.d.comb += [
            fu_wp.addr.eq(push_ptr), fu_wp.data.eq(Cat(*fu_word)), fu_wp.en.eq(x3["v"]),
            fs_wp.addr.eq(push_ptr),
            fs_wp.data.eq(Cat(x3["g"], x3["w0"], x3["w1"], x3["b"], x3["e"])),
            fs_wp.en.eq(x3["v"]),
        ]
        with m.If(x3["v"]):
            m.d.sync += push_ptr.eq(push_ptr + 1)
        x4 = pipe(x3, "x4")

        # --- X4: the check-node accumulators ---------------------------------
        min1 = [Signal(7, name=f"min1_{u}") for u in range(S)]
        min2 = [Signal(7, name=f"min2_{u}") for u in range(S)]
        imin = [Signal(5, name=f"imin_{u}") for u in range(S)]
        par = [Signal(name=f"par_{u}") for u in range(S)]
        hpar = [Signal(name=f"hpar_{u}") for u in range(S)]
        with m.If(x4["v"]):
            for u in range(S):
                a = a4[u]
                with m.If(x4["k"] == 0):
                    m.d.sync += [min1[u].eq(a), min2[u].eq(127), imin[u].eq(0),
                                 par[u].eq(neg4[u]), hpar[u].eq(h4[u])]
                with m.Else():
                    m.d.sync += [par[u].eq(par[u] ^ neg4[u]), hpar[u].eq(hpar[u] ^ h4[u])]
                    with m.If(a < min1[u]):
                        m.d.sync += [min2[u].eq(min1[u]), min1[u].eq(a), imin[u].eq(x4["k"])]
                    with m.Elif(a < min2[u]):
                        m.d.sync += min2[u].eq(a)
        x5 = pipe(dict(v=x4["v"] & x4["last"], n=x4["n"]), "x5")

        # --- L (X5 of the last entry): latch, record, syndrome ---------------
        rm1 = [Signal(7, name=f"rm1_{u}") for u in range(S)]
        rm2 = [Signal(7, name=f"rmm2_{u}") for u in range(S)]
        rim = [Signal(5, name=f"rim_{u}") for u in range(S)]
        rpar = [Signal(name=f"rpar_{u}") for u in range(S)]
        hvec = Signal(S)
        hvalid = Signal()
        rec_word = []
        for u in range(S):
            o1 = Mux(min1[u] > offset, min1[u] - offset, 0)
            o2 = Mux(min2[u] > offset, min2[u] - offset, 0)
            rec_word.append(Cat(o1[:7], o2[:7], imin[u]))
            with m.If(x5["v"]):
                m.d.sync += [rm1[u].eq(o1), rm2[u].eq(o2), rim[u].eq(imin[u]), rpar[u].eq(par[u])]
        m.d.comb += [rec_wp.addr.eq(x5["n"]), rec_wp.data.eq(Cat(*rec_word)), rec_wp.en.eq(x5["v"])]
        m.d.sync += [hvalid.eq(x5["v"]), hvec.eq(Cat(*hpar))]
        # popcount in two stages
        chunks = [hvec[i:i + 8] for i in range(0, S, 8)]
        part = [Signal(4, name=f"pc{i}") for i in range(len(chunks))]
        pvalid = Signal()
        plast = Signal()
        lastn = Signal()
        m.d.sync += lastn.eq(x5["v"] & (x5["n"] == r_steps - 1))
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
        pop_ptr = Signal(6)
        wk = Signal(kbits)
        wact = Signal()
        with m.If(x5["v"]):
            m.d.sync += [wact.eq(1), wk.eq(0)]
        with m.Elif(wact):
            m.d.sync += wk.eq(wk + 1)
            with m.If(wk == r_d - 1):
                m.d.sync += wact.eq(0)
        popping = x5["v"] | (wact & (wk != r_d - 1))
        m.d.comb += [fu_rp.addr.eq(pop_ptr), fs_rp.addr.eq(pop_ptr)]
        with m.If(popping):
            m.d.sync += pop_ptr.eq(pop_ptr + 1)
        wa_v = Signal()
        wa_k = Signal(kbits)
        m.d.sync += [wa_v.eq(popping), wa_k.eq(Mux(x5["v"], 0, wk + 1))]
        # Wa: r_new
        wb = dict(v=Signal(name="wb_v"), sh=Signal(fs_rp.data.shape(), name="wb_sh"))
        m.d.sync += [wb["v"].eq(wa_v), wb["sh"].eq(fs_rp.data)]
        rnew_b = [Signal(signed(8), name=f"rnb_{u}") for u in range(S)]
        fub = [Signal(21, name=f"fub_{u}") for u in range(S)]
        for u in range(S):
            f = fu_rp.data[21 * u:21 * u + 21]
            t = f[:8].as_signed()
            mag = Mux(wa_k == rim[u], rm2[u], rm1[u])
            neg = rpar[u] ^ (t < 0)
            m.d.sync += [rnew_b[u].eq(Mux(neg, -mag, mag)), fub[u].eq(f)]
        # Wb: v, the sign memory write
        wc = dict(v=Signal(name="wc_v"), sh=Signal(fs_rp.data.shape(), name="wc_sh"))
        m.d.sync += [wc["v"].eq(wb["v"]), wc["sh"].eq(wb["sh"])]
        vc = [Signal(signed(10), name=f"vc_{u}") for u in range(S)]
        xc = [Signal(4, name=f"xc_{u}") for u in range(S)]  # h, acc, valid
        sh_e = wb["sh"][gbits + 2 * stbits + ubits:]
        m.d.comb += [sgn_wp.addr.eq(sh_e), sgn_wp.en.eq(wb["v"]),
                     sgn_wp.data.eq(Cat(*[rnew_b[u] < 0 for u in range(S)]))]
        for u in range(S):
            f = fub[u]
            t, rmag, rneg, h, dmode, acc, valid = (f[:8].as_signed(), f[8:15], f[15], f[16],
                                                   f[17], f[18:20], f[20])
            rold = Mux(rneg, -rmag, rmag)
            val = Signal(signed(10), name=f"val_{u}", attrs=NO_DSP)
            m.d.comb += val.eq(Mux(dmode, rnew_b[u] - rold, t + rnew_b[u]))
            m.d.sync += [vc[u].eq(val), xc[u].eq(Cat(h, acc, valid))]
        # Wc: rotate units -> lanes (lane lam <- unit (lam + b) mod S), lane addresses
        wd_v = Signal()
        m.d.sync += wd_v.eq(wc["v"])
        # verification: the entry index of the reads at the memories and of
        # the writes (synth/ldpc_latency.py measures WRITE_LATENCY with them)
        wk_b, wk_c, wk_d = (Signal(kbits, name=f"wk_{x}") for x in "bcd")
        m.d.sync += [wk_b.eq(wa_k), wk_c.eq(wk_b), wk_d.eq(wk_c)]
        self._probe = dict(read_v=x0["v"], read_k=x0["k"], write_v=wd_v, write_k=wk_d)
        sh = wc["sh"]
        c_g = sh[:gbits]
        c_w0 = sh[gbits:gbits + stbits]
        c_w1 = sh[gbits + stbits:gbits + 2 * stbits]
        c_b = sh[gbits + 2 * stbits:gbits + 2 * stbits + ubits]
        units = [Cat(vc[u], xc[u]) for u in range(S)]
        lanes_w = Signal(14 * S)
        # lane lam <- unit (lam + b) mod S: the inverse rotation
        m.d.comb += lanes_w.eq(_rotate(m, Cat(*units), 14, S,
                                       Mux(c_b == 0, 0, S - c_b), "rw"))
        wd_addr = [Signal(abits, name=f"wda{lam}") for lam in range(S)]
        vd = [Signal(signed(10), name=f"vd_{lam}") for lam in range(S)]
        xd = [Signal(4, name=f"xd_{lam}") for lam in range(S)]
        thr_w = Signal(sbits)
        m.d.comb += thr_w.eq(S - c_b)
        for lam in range(S):
            m.d.sync += [vd[lam].eq(lanes_w[14 * lam:14 * lam + 10].as_signed()),
                         xd[lam].eq(lanes_w[14 * lam + 10:14 * lam + 14]),
                         wd_addr[lam].eq(Cat(Mux(lam >= thr_w, c_w1, c_w0), c_g))]
        # Wd: the lane sums, the write
        fill_en = Signal()
        fill_lo = Signal(range(S + 1))
        fill_hi = Signal(range(S + 1))
        fill_addr = Signal(abits)
        for lam in range(S):
            p1 = Signal(signed(11), name=f"pend1_{lam}")
            p2 = Signal(signed(11), name=f"pend2_{lam}")
            h, acc, valid = xd[lam][0], xd[lam][1:3], xd[lam][3]
            ssum = Signal(signed(11), name=f"sum_{lam}", attrs=NO_DSP)
            m.d.comb += ssum.eq(vd[lam] + Mux(acc == 1, p1, Mux(acc == 2, p2, 0)))
            sat8 = Mux(ssum > 127, 127, Mux(ssum < -128, -128, ssum))
            with m.If(wd_v):
                m.d.sync += [p2.eq(p1), p1.eq(ssum)]
            known = (lam < fill_lo) | (lam >= fill_hi)
            m.d.comb += [
                dec_wen[lam].eq(Mux(fill_en, known, wd_v & valid)),
                dec_wdata[lam].eq(Mux(fill_en, Const(127, 9), Cat(sat8[:8], h))),
            ]
            m.d.comb += dec_waddr[lam].eq(Mux(fill_en, fill_addr, wd_addr[lam]))

        # --- the engine's control --------------------------------------------
        wpipe_busy = x0["v"] | x1["v"] | x2["v"] | x3["v"] | x4["v"] | x5["v"] | wact \
            | wa_v | wb["v"] | wc["v"] | wd_v | i1["v"] | i2["v"]
        cyc = self.cycles
        drain_left = Signal(4)
        ld_busy = Signal()
        m.d.comb += self.start_ready.eq(~busy & ~ld_busy & ~(out_active & (out_buf == load_buf)))
        out_go = Signal()
        m.d.comb += out_go.eq(0)
        info_start, info_len = self.shortened
        info_end = info_start + info_len
        m.d.comb += [fill_en.eq(busy & (phase == FILL)), fill_addr.eq(Cat(fill_w, fill_g))]
        lo_v = Signal(signed(17))
        hi_v = Signal(signed(17))
        m.d.comb += [lo_v.eq(info_start - fill_base), hi_v.eq(info_end - fill_base)]
        m.d.comb += [
            fill_lo.eq(Mux(lo_v < 0, 0, Mux(lo_v > S, S, lo_v))),
            fill_hi.eq(Mux(hi_v < 0, 0, Mux(hi_v > S, S, hi_v))),
        ]
        with m.If(busy):
            m.d.sync += cyc.eq(cyc + 1)
        with m.If(self.start & self.start_ready):
            m.d.sync += [
                busy.eq(1), dec_buf.eq(load_buf), load_buf.eq(~load_buf), rate.eq(self.st_rate),
                max_iter.eq(self.st_max_iter), offset.eq(self.st_offset), short.eq(self.st_short),
                phase.eq(Mux(self.st_short, FILL, PASS)), pass_no.eq(1), cyc.eq(0),
                layer.eq(0), st.eq(0), k.eq(0), n_idx.eq(0), e_idx.eq(0), in_step.eq(0),
                idle.eq(255), decided.eq(1), unsat.eq(0), fill_g.eq(0), fill_w.eq(0),
                fill_base.eq(0), push_ptr.eq(0), pop_ptr.eq(0),
                layer_info.eq(Array([Const(v, len(layer_info)) for v in info_base + [0]])[
                    self.st_rate]),
                layer_chain.eq(Array([Const(v, len(layer_chain)) for v in chain_base + [0]])[
                    self.st_rate]),
                gap_ptr.eq(Array([Const(v, len(gap_ptr)) for v in gap_base + [0]])[
                    self.st_rate]),
            ]
        with m.If(busy & (phase == FILL)):
            m.d.sync += fill_base.eq(fill_base + S)
            with m.If(fill_w == W - 1):
                m.d.sync += [fill_w.eq(0), fill_g.eq(fill_g + 1)]
                with m.If(fill_g == r_G - 1):
                    m.d.sync += phase.eq(PASS)
            with m.Else():
                m.d.sync += fill_w.eq(fill_w + 1)
        with m.If(busy & (phase == WAITD) & pass_end):
            # the pass's syndrome weight (iteration pass_no - 1) is complete
            total = unsat + 0
            with m.If((total == 0) | (pass_no - 1 == max_iter)):
                m.d.sync += [phase.eq(DRAIN), drain_left.eq(3),
                             self.res_iterations.eq(pass_no - 1),
                             self.res_converged.eq(total == 0), self.res_unsat.eq(total)]
            with m.Else():
                m.d.sync += [phase.eq(PASS), pass_no.eq(pass_no + 1), decided.eq(1),
                             layer.eq(0), st.eq(0), k.eq(0), n_idx.eq(0), e_idx.eq(0),
                             unsat.eq(0), layer_info.eq(r_info), layer_chain.eq(r_chain),
                             gap_ptr.eq(r_gap)]
        with m.If(busy & (phase == DRAIN)):
            with m.If(~wpipe_busy):
                with m.If(drain_left == 0):
                    m.d.sync += busy.eq(0)
                    m.d.comb += out_go.eq(1)
                with m.Else():
                    m.d.sync += drain_left.eq(drain_left - 1)

        # --- the output stream -------------------------------------------------
        og = Signal(gbits)
        ow = Signal(wbits)
        o_more = Signal()  # words left to read
        o_inflight = Signal()
        buf = Signal(S + 16)
        cnt = Signal(range(S + 17))
        bits_left = Signal(17)
        m.d.comb += out_raddr.eq(Cat(ow, og))
        m.d.sync += self.res_valid.eq(out_go)
        with m.If(out_go):
            m.d.sync += [out_active.eq(1), out_buf.eq(dec_buf), og.eq(0), ow.eq(0), o_more.eq(1),
                         cnt.eq(0), bits_left.eq(self.n), o_inflight.eq(0)]
        emit = Signal()
        m.d.comb += [
            emit.eq(out_active & self.o_ready & ((cnt >= 16) | (~o_more & ~o_inflight & (cnt != 0)))),
            self.o_valid.eq(out_active & ((cnt >= 16) | (~o_more & ~o_inflight & (cnt != 0)))),
            self.o_data.eq(buf[:16]),
            self.o_last.eq(bits_left <= 16),
        ]
        cnt_after = Signal.like(cnt)
        buf_after = Signal.like(buf)
        m.d.comb += [cnt_after.eq(Mux(emit, Mux(cnt >= 16, cnt - 16, 0), cnt)),
                     buf_after.eq(Mux(emit, buf >> 16, buf))]
        hbits = Cat(*[odata[lam][8] for lam in range(S)])
        with m.If(out_active & ~out_go):
            with m.If(o_inflight):
                m.d.sync += [buf.eq(buf_after | (hbits << cnt_after)), cnt.eq(cnt_after + S)]
            with m.Else():
                m.d.sync += [buf.eq(buf_after), cnt.eq(cnt_after)]
            read_now = o_more & ~o_inflight & (cnt_after < 16)
            m.d.sync += o_inflight.eq(read_now)
            with m.If(read_now):
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
        # --- the input mapping (position -> lane, address) ----------------------
        # information (pos < k): g = pos div 374, j = pos mod 374; parity
        # (c = pos - k): g = G + (c mod q), j = c div q; then the word j div S
        # and the lane j mod S (a ROM). Both divisions restoring, one quotient
        # bit per stage (pos div 374 < 128, c div q < 374 < 512): 9 stages.
        in_k = R("ik", [r.k for r in rates], 16)[self.in_rate]
        in_G = R("iG", [r.G for r in rates], gbits)[self.in_rate]
        in_q = R("iq", [r.q for r in rates])[self.in_rate]
        st0 = dict(v=Signal(name="ld_v"), llr=Signal(signed(8), name="ld_llr"),
                   isp=Signal(name="ld_isp"), G=Signal(gbits, name="ld_G"),
                   q=Signal(len(in_q), name="ld_q"), r=Signal(16, name="ld_r"),
                   quo=Signal(9, name="ld_quo"))
        isp = self.in_pos >= in_k
        m.d.sync += [
            st0["v"].eq(self.in_valid & self.in_ready), st0["llr"].eq(self.in_llr),
            st0["isp"].eq(isp), st0["G"].eq(in_G), st0["q"].eq(in_q),
            st0["r"].eq(Mux(isp, self.in_pos - in_k, self.in_pos)), st0["quo"].eq(0),
        ]
        stages = [st0["v"]]
        cur = st0
        for i in range(8, -1, -1):
            dv = Mux(cur["isp"], cur["q"] << i, GROUP << i if i <= 6 else (1 << 17) - 1)
            ge = cur["r"] >= dv
            nxt = {key: Signal.like(sig, name=f"ld{i}_{key}") for key, sig in cur.items()}
            for key in ("v", "llr", "isp", "G", "q"):
                m.d.sync += nxt[key].eq(cur[key])
            m.d.sync += [nxt["r"].eq(Mux(ge, cur["r"] - dv, cur["r"])),
                         nxt["quo"].eq(cur["quo"] | (ge << i))]
            stages.append(nxt["v"])
            cur = nxt
        g_ld = Mux(cur["isp"], cur["G"] + cur["r"], cur["quo"])
        j_ld = Mux(cur["isp"], cur["quo"], cur["r"])
        regs = cur
        jdm = Memory(shape=stbits + ubits, depth=GROUP,
                     init=[(j // S) | (j % S) << stbits for j in range(GROUP)])
        m.submodules.jdm = jdm
        jrp = jdm.read_port(domain="comb")
        m.d.comb += jrp.addr.eq(j_ld)
        fin = dict(v=Signal(name="ldf_v"), g=Signal(gbits, name="ldf_g"),
                   w=Signal(stbits, name="ldf_w"), lane=Signal(ubits, name="ldf_lane"),
                   llr=Signal(signed(8), name="ldf_llr"))
        m.d.sync += [fin["v"].eq(regs["v"]), fin["g"].eq(g_ld), fin["w"].eq(jrp.data[:stbits]),
                     fin["lane"].eq(jrp.data[stbits:]), fin["llr"].eq(regs["llr"])]
        stages.append(fin["v"])
        m.d.comb += [ld_addr.eq(Cat(fin["w"][:wbits], fin["g"])), ld_data.eq(Cat(fin["llr"], 0)),
                     ld_lane.eq(fin["lane"]), ld_en.eq(fin["v"])]
        m.d.comb += ld_busy.eq(Cat(*stages).any())
        m.d.comb += self.in_ready.eq(~(out_active & (out_buf == load_buf)))
        return m
