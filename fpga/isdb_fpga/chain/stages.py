"""Per-stage simulation of the chain's blocks against the vectors (the
stages after stage 1; `sim.py` dispatches here). Each stage is driven
through a small bench wrapper that takes one record per cycle from the
engine's input stream and routes it to the block's ports, so the compiled
backend (`simx.py`) can feed millions of records without Python in the
loop.
"""

import os
import time

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal, signed

from .common import FRAME, FSYNC, SLOT, SLOTS, SSYNC, SYNC_REF, word_symbols
from .eq import ENTRIES, META, META_SMALL, Equaliser
from .frame import FrameCtl, lay_pack
from .phase import SlotPhase
from .simx import engine
from .sim import first_mismatch


# --- stage 3 -------------------------------------------------------------------

class PhaseBench(Elaboratable):
    """Records: kind 0 = an NCO input with its phase (at, x0, x1, phi); kind
    1 = a head (at, word, theta, amp, numbered, number, frame, and in phi the
    carrier register after it)."""

    def __init__(self, in_bits=6):
        self.dut = SlotPhase(in_bits)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal()
        self.at = Signal(48)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.word = Signal(2)
        self.numbered = Signal()
        self.number = Signal(7)
        self.frame = Signal(32)
        self.phi = Signal(32)

    def elaborate(self, platform):
        m = Module()
        m.submodules.dut = d = self.dut
        sym = self.i_valid & self.i_ready & (self.kind == 0)
        head = self.i_valid & self.i_ready & (self.kind == 1)
        m.d.comb += [
            self.i_ready.eq(~d.full),
            d.i_valid.eq(sym), d.i_at.eq(self.at), d.i_x[0].eq(self.a), d.i_x[1].eq(self.b),
            d.i_phi.eq(self.phi), d.h_f.eq(self.phi),
            d.h_valid.eq(head), d.h_late.eq(0), d.h_at.eq(self.at), d.h_word.eq(self.word),
            d.h_theta.eq(self.a), d.h_amp.eq(self.b), d.h_numbered.eq(self.numbered),
            d.h_number.eq(self.number), d.h_frame.eq(self.frame),
        ]
        return m

    def ports(self):
        d = self.dut
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "at": self.at,
            "a": self.a, "b": self.b, "word": self.word, "numbered": self.numbered,
            "number": self.number, "frame": self.frame, "phi": self.phi,
            "ov": d.o_valid, "oat": d.o_at, "oj": d.o_j, "ophi": d.o_phi, "og": d.o_gain,
            "oy0": d.o_y[0], "oy1": d.o_y[1], "or0": d.o_r[0], "or1": d.o_r[1],
        }


def head_record(h, f=0):
    at, word, _c0, _c1, theta, amp, number, frame = h
    theta_s = theta - (1 << 32) if theta >= 1 << 31 else theta
    return (1, at, theta_s, amp, word, int(number >= 0), max(number, 0), max(frame, 0), f & 0xFFFFFFFF)


def run_phase(doc, backend):
    heads = doc["heads"]
    r = doc["r"]
    x, phi = doc["x"], doc["phi"]
    a0 = heads[0][0]
    recs = []
    # the slots' symbols (the NCO's input and phase); each head after its
    # window's last symbol (as the model emits it, 24 symbols in) with the
    # carrier register after it (f from its symbol + 24 on), the stream
    # padded past the last
    n = len(r) // 2

    def f_at(i):
        return (phi[i + 1] - phi[i]) & 0xFFFFFFFF if i + 1 < n else 0

    pad = 24 + 256
    hq = {h[0] + 24: h for h in heads}
    for i in range(n + pad):
        at = a0 + i
        if at in hq:
            recs.append(head_record(hq[at], f_at(at - a0) if at - a0 < n else 0))
        v = (x[2 * i], x[2 * i + 1], phi[i]) if i < n else (0, 0, 0)
        recs.append((0, at, v[0], v[1], 0, 0, 0, 0, v[2]))
    b = PhaseBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain2")
    g = e.group("ov", ["oat", "oj", "ophi", "og", "oy0", "oy1", "or0", "or1"])
    t0 = time.time()
    e.run("i_valid", "i_ready", ["kind", "at", "a", "b", "word", "numbered", "number", "frame", "phi"],
          recs, tail=9600)
    got = e.read(g)
    cyc = e.cycles()
    dt = time.time() - t0
    exp = [tuple(o) for o in doc["out"]]
    bad = first_mismatch("y", [x[:6] for x in got], exp, key=lambda i, v: f"symbol at {v[0]} (j {v[1]})")
    exp_r = [(r[2 * i], r[2 * i + 1]) for i in range(n)]
    bad += first_mismatch("r", [x[6:] for x in got], exp_r, key=lambda i, v: f"#{i}")
    what = f"{len(got)} symbols corrected over 2 slots; {cyc} cycles in {dt:.1f} s"
    return what, bad


# --- stage 4 -------------------------------------------------------------------

K_SYM, K_READ, K_LAY, K_COMMIT, K_TAP, K_TAPNOW = range(6)


def pack_meta(**f):
    v, o = 0, 0
    for n, w in META:
        v |= (f.get(n, 0) & ((1 << w) - 1)) << o
        o += w
    return v


def unpack_meta(v):
    out, o = {}, 0
    for n, w in META:
        out[n] = (v >> o) & ((1 << w) - 1)
        o += w
    return out


def wire_eq_frame(m, eq, fc):
    """The equaliser's centre to the frame bookkeeping (as the chain's back
    end wires them)."""
    cm = eq.c_meta
    m.d.comb += [
        fc.cv.eq(eq.cv),
        fc.c_numbered.eq(eq.meta_field(cm, "numbered")),
        fc.c_number.eq(eq.meta_field(cm, "number")),
        fc.c_frame.eq(eq.meta_field(cm, "frame")),
        fc.c_j.eq(eq.meta_field(cm, "j")),
        fc.c_at.eq(eq.meta_field(cm, "at")),
        eq.t_en.eq(fc.active & fc.has_layout & (fc.is_sync | fc.is_pilot)),
        eq.t_pilot.eq(fc.is_pilot),
        eq.t_t.eq(fc.t),
        eq.t_mod.eq(fc.s_mod),
        eq.t_rate.eq(fc.s_rate),
        eq.frame_open.eq(fc.start & fc.has_layout),
        eq.frame_close.eq(fc.o_done & fc.o_has_layout),
        eq.close_frame.eq(fc.o_frame),
    ]


class EqBench(Elaboratable):
    """Records (kind, a, b, c, meta): K_SYM y = (a, b) with meta; K_READ
    sums word a; K_LAY layout entry of slot a = meta; K_COMMIT the layout
    for frame a (from frame b); K_TAP tap a = (b, c); K_TAPNOW."""

    def __init__(self):
        self.eq = Equaliser()
        self.fc = FrameCtl()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(3)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(signed(32))
        self.meta = Signal(META_SMALL)  # the meta without `at` (ports <= 64 bits)
        self.at = Signal(48)
        self.o_ms = Signal(META_SMALL)
        self.o_at = Signal(48)

    def elaborate(self, platform):
        m = Module()
        m.submodules.eq = eq = self.eq
        m.submodules.fc = fc = self.fc
        wire_eq_frame(m, eq, fc)
        go = self.i_valid & self.i_ready
        k = self.kind
        m.d.comb += [
            self.i_ready.eq(Mux(k == K_READ, ~eq.rd_busy, 1)),
            eq.i_valid.eq(go & (k == K_SYM)), eq.i_y[0].eq(self.a), eq.i_y[1].eq(self.b),
            eq.i_meta.eq(Cat(self.meta, self.at)),
            self.o_ms.eq(eq.o_meta[:META_SMALL]), self.o_at.eq(eq.o_meta[META_SMALL:]),
            eq.rd_req.eq(go & (k == K_READ)), eq.rd_addr.eq(self.a),
            fc.lw_en.eq(go & (k == K_LAY)), fc.lw_slot.eq(self.a), fc.lw_data.eq(self.meta),
            fc.lw_commit.eq(go & (k == K_COMMIT)), fc.lw_frame.eq(self.a), fc.lw_from.eq(self.b),
            eq.tap_we.eq(go & (k == K_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c),
            eq.tap_now.eq(go & (k == K_TAPNOW)),
        ]
        return m

    def ports(self):
        e = self.eq
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
            "b": self.b, "c": self.c, "meta": self.meta, "at": self.at, "oat": self.o_at,
            "ov": e.o_valid, "oz0": e.o_z[0], "oz1": e.o_z[1], "ometa": self.o_ms,
            "dv": e.d_valid, "dat": e.d_at, "d0": e.d_ref[0], "d1": e.d_ref[1],
            "rack": e.rd_ack, "rdata": e.rd_data, "sframe": e.sums_frame, "scount": e.sums_count,
        }


def slot_words(training, slots=SLOTS):
    """The word of each slot of a frame, read off its sync references."""
    fs = [(a * SYNC_REF, b * SYNC_REF) for a, b in word_symbols(FSYNC)]
    ss = [(a * SYNC_REF, b * SYNC_REF) for a, b in word_symbols(SSYNC)]
    out = []
    for k in range(slots):
        d = [tuple(t[1:]) for t in training[k * 56:k * 56 + 24]]
        out.append(0 if d == ss else 1 if d == fs else 2)
    return out


def layout_records(layout, frame, from_frame):
    recs = []
    for s in layout:
        slot, mode, mod, rate, dummy, start, syms = s
        recs.append((K_LAY, slot, 0, 0, lay_pack(mode, mod, rate, dummy, start, syms)))
    recs.append((K_COMMIT, frame, from_frame, 0, 0))
    return recs


def run_eq(doc, layout, backend, symbols=None):
    """Stage 4 over chain-3-eq's frame: z of every symbol, every training
    reference, the latched sums and their count. `layout`: the frame's
    layout (chain-4-tmcc's: the synthetic signal's allocation is constant;
    the training references check it). `symbols`: a prefix of the frame
    (no sums then)."""
    F, at = doc["frame"], doc["at"]
    y = doc["y"]
    words = slot_words(doc["training"])
    recs = []
    for i, t in enumerate(doc["taps"][0::2]):
        recs.append((K_TAP, i, t, doc["taps"][2 * i + 1], 0))
    recs.append((K_TAPNOW, 0, 0, 0, 0))
    recs += layout_records(layout, F, F - 2)
    n = len(y) // 2 if symbols is None else min(len(y) // 2, symbols + 10)
    whole = n == len(y) // 2
    for i in range(n):
        idx = doc["y_from"] + i
        rel = idx - at
        if rel < 0:
            fr, k, j = F - 1, SLOTS - 1, SLOT + rel
        elif rel >= FRAME:
            fr, k, j = F + 1, 0, rel - FRAME
        else:
            fr, k, j = F, rel // SLOT, rel % SLOT
        w = words[k] if fr == F else (0 if k == SLOTS - 1 else 1)
        recs.append((K_SYM, y[2 * i], y[2 * i + 1], 0,
                     pack_meta(j=j, number=k, numbered=1, frame=fr, word=w), idx))
    if whole:
        recs += [(K_READ, a, 0, 0, 0) for a in range(4 * ENTRIES)]
    b = EqBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain3")
    gz = e.group("ov", ["oz0", "oz1", "ometa"])
    gd = e.group("dv", ["dat", "d0", "d1"])
    gr = e.group("rack", ["rdata", "sframe", "scount"])
    t0 = time.time()
    recs = [r if len(r) == 6 else r + (0,) for r in recs]
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "meta", "at"], recs, tail=100)
    zs = e.read(gz)
    ds = e.read(gd)
    rs = e.read(gr)
    cyc = e.cycles()
    dt = time.time() - t0
    bad = []
    got_z = [(z0, z1) for z0, z1, mt in zs if unpack_meta(mt)["frame"] == F]
    zz = doc["z"]
    nz = len(zz) // 2 if whole else max(0, n - 10)
    exp_z = [(zz[2 * i], zz[2 * i + 1]) for i in range(nz)]
    bad += first_mismatch("z", got_z, exp_z,
                          key=lambda i, v: f"frame symbol {i} (slot {i // SLOT}, j {i % SLOT})")
    exp_d = [tuple(t) for t in doc["training"] if t[0] < at + nz]
    bad += first_mismatch("training ref", ds, exp_d, key=lambda i, v: f"at {v[0]}")
    if whole:
        exp_r = []
        for v in doc["sums"]["r"] + doc["sums"]["p"]:
            (rh, rl), (ih, il) = v
            exp_r += [rl, rh & 0xFFFFFFFF, il, ih & 0xFFFFFFFF]
        bad += first_mismatch("sums word", [(r[0],) for r in rs], [(v,) for v in exp_r],
                              key=lambda i, v: f"entry {i // 4} part {i % 4}")
        want = (doc["sums"]["frame"], doc["sums"]["count"])
        if not rs or (rs[-1][1], rs[-1][2]) != want:
            bad.append(f"sums frame/count {rs[-1][1:] if rs else None} != {want}")
    what = (f"{len(got_z)} frame symbols equalised, {len(ds)} training references, {len(rs)} sum words; "
            f"{cyc} cycles in {dt:.1f} s")
    return what, bad


# --- stage 5 -------------------------------------------------------------------

T_SYM, T_DONE, T_HEAD, T_BEAT = range(4)


class TmccBench(Elaboratable):
    """TmccSoft (soft bits, staging buffer, burst) and TmccResult (with its
    own BCH unit here) between a stub decoder: records T_SYM (z0, z1, odd,
    idx), T_DONE (frame), T_HEAD (iterations, converged, frame) and T_BEAT
    (bits, last) of the stub's result. The port grant follows a fixed
    pseudo-random pattern (the demapper's priority, about half the cycles)."""

    def __init__(self):
        from .finish import BchUnit
        from .tmcc import TMCC_DATA, TmccResult, TmccSoft

        self.soft = TmccSoft()
        self.res = TmccResult()
        self.bch = BchUnit([TMCC_DATA])
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(2)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(32)
        self.d = Signal(16)
        self.taken = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.soft = s = self.soft
        m.submodules.res = r = self.res
        m.submodules.bch = bch = self.bch
        go = self.i_valid & self.i_ready
        k = self.kind
        lfsr = Signal(16, init=0xACE1)
        m.d.sync += lfsr.eq(Cat(lfsr[1:], lfsr[0] ^ lfsr[2] ^ lfsr[3] ^ lfsr[5]))
        m.d.comb += [
            self.i_ready.eq(Mux(k >= T_HEAD, r.r_ready, 1)),
            s.t_valid.eq(go & (k == T_SYM)), s.t_z[0].eq(self.a), s.t_z[1].eq(self.b),
            s.t_odd.eq(self.c[0]), s.t_idx.eq(self.d),
            s.frame_done.eq(go & (k == T_DONE)), s.frame.eq(self.a),
            s.grant.eq(s.b_valid & lfsr[0]),
            self.taken.eq(s.b_valid & lfsr[0]),
            r.r_valid.eq(go & (k >= T_HEAD)), r.r_head.eq(k == T_HEAD),
            r.r_iterations.eq(self.a), r.r_converged.eq(self.b[0]), r.frame.eq(self.c),
            r.r_bits.eq(self.a), r.r_last.eq(self.b[0]),
            bch.start.eq(r.bch_start), bch.n_sel.eq(0), bch.i_valid.eq(r.bch_valid),
            bch.i_bits.eq(r.bch_bits), bch.i_last.eq(r.bch_last),
            r.bch_done.eq(bch.done), r.bch_ok.eq(bch.ok), r.bch_corrected.eq(bch.corrected),
            r.bch_nroots.eq(bch.nroots), *[r.bch_roots[x].eq(bch.roots[x]) for x in range(12)],
        ]
        return m

    def ports(self):
        s, r, lay = self.soft, self.res, self.res.layout
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
            "b": self.b, "c": self.c, "d": self.d,
            "sv": s.s_valid, "sidx": s.s_idx, "ssoft": s.s_soft,
            "bv": self.taken, "bop": s.b_op, "bn": s.b_n, "btag": s.b_tag,
            **{f"bpos{k}": s.b_pos[k] for k in range(4)}, **{f"bllr{k}": s.b_llr[k] for k in range(4)},
            "oh": r.o_head, "ofr": r.o_frame, "oit": r.o_iter, "ocv": r.o_conv, "obok": r.o_bch_ok,
            "obch": r.o_bch, "ov": r.o_valid, "obits": r.o_bits, "olast": r.o_last,
            "lwe": lay.lw_en, "lws": lay.lw_slot, "lwd": lay.lw_data, "lwc": lay.lw_commit,
            "lwf": lay.lw_frame, "lwfrom": lay.lw_from, "over": s.overrun,
        }


def tmcc_stub_bits(info_bits):
    """The LDPC decoder's first 22 814 hard decisions of the frame's TMCC
    word for the information bits the vectors give (converged, no BCH
    correction): the known zeros 0, the 9 614 data bits = the TMCC
    dispersal of the BCH codeword, the rest of the first k (known zeros) 0;
    as 1 426 beats."""
    from . import gf
    from .tables import TMCC_INIT, prbs
    from .tmcc import NULL_HEAD

    data = gf.encode(info_bits)
    pr = prbs(TMCC_INIT, 14, 15, len(data))
    word = [0] * 22814
    for i, (d, p) in enumerate(zip(data, pr)):
        word[NULL_HEAD + i] = d ^ p
    return [sum(word[16 * q + b] << b for b in range(16) if 16 * q + b < len(word))
            for q in range((len(word) + 15) // 16)]


def run_tmcc(doc, backend, flips=()):
    """Stage 5 on chain-4-tmcc. `flips`: decoder output positions to flip
    (bit errors the BCH must correct: the vectors have none; the expected
    result is then the vectors' with that many corrections)."""
    from .frame import lay_pack
    from .ldpc_if import OP_BEGIN, OP_DONE, OP_WRITE, TAG_TMCC

    F = doc["frame"]
    z = doc["z_tmcc"]
    recs = []
    for b, j in enumerate(doc["j"]):
        recs.append((T_SYM, z[2 * b], z[2 * b + 1], j & 1, b))
    recs.append((T_DONE, F, 0, 0, 0))
    gaps = [0] * len(recs)
    gaps[-1] = 80_000  # the burst (about half the cycles granted)
    beats = tmcc_stub_bits(doc["bits"])
    for p_ in flips:
        beats[p_ // 16] ^= 1 << (p_ % 16)
    it = doc["ldpc_iterations"]
    recs.append((T_HEAD, it if it is not None else 50, int(it is not None), F, 0))
    recs += [(T_BEAT, v, int(q == len(beats) - 1), 0, 0) for q, v in enumerate(beats)]
    gaps += [0] * (len(beats) + 1)
    bn = TmccBench()
    p = bn.ports()
    e = engine(bn, p, backend, tag="chain4")
    gs = e.group("sv", ["sidx", "ssoft"])
    gb = e.group("bv", ["bop", "btag", "bn"] + [f"bpos{k}" for k in range(4)] + [f"bllr{k}" for k in range(4)])
    gh = e.group("oh", ["ofr", "oit", "ocv", "obok", "obch"])
    go = e.group("ov", ["obits", "olast"])
    gl = e.group("lwe", ["lws", "lwd"])
    gc = e.group("lwc", ["lwf", "lwfrom"])
    t0 = time.time()
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d"], recs, tail=20_000, gaps=gaps)
    soft, burst, head, out, lw, com = (e.read(g) for g in (gs, gb, gh, go, gl, gc))
    over = e.get("over")
    cyc = e.cycles()
    dt = time.time() - t0
    bad = []
    bad += first_mismatch("soft", soft, list(enumerate(doc["soft"])), key=lambda i, v: f"TMCC bit {i}")
    exp_b = [(OP_BEGIN,)] + [(OP_WRITE, pos, v) for pos, v in zip(doc["position"], doc["soft"])] + [(OP_DONE,)]
    got_b = []
    for x in burst:
        if x[0] != OP_WRITE:
            got_b.append((x[0],))
        else:
            got_b += [(x[0], x[3 + k], x[7 + k]) for k in range(x[2])]
    bad += first_mismatch("burst", got_b, exp_b, key=lambda i, v: f"value {i}")
    tags = {x[1] for x in burst}
    if tags != {TAG_TMCC | 3 << 8 | F << 32}:
        bad.append(f"burst tags {tags}")
    bits = doc["bits"]
    exp_o = [(sum(bits[16 * q + b] << b for b in range(16) if 16 * q + b < len(bits)),
              int(q == (len(bits) + 15) // 16 - 1)) for q in range((len(bits) + 15) // 16)]
    bad += first_mismatch("tmcc bits", out, exp_o, key=lambda i, v: f"beat {i}")
    bch = doc["bch_corrected"] + len(flips) if doc["bch_corrected"] is not None else None
    want_h = [(F, it if it is not None else 50, int(it is not None), int(bch is not None), bch or 0)]
    bad += first_mismatch("tmcc result", head, want_h)
    exp_l = [(s[0], lay_pack(s[1], s[2], s[3], s[4], s[5], s[6])) for s in doc["layout"]]
    bad += first_mismatch("layout", lw, exp_l, key=lambda i, v: f"slot {v[0]}")
    bad += first_mismatch("layout commit", com, [(doc["applies_to"], F)])
    if over:
        bad.append(f"staging overrun {over}")
    what = (f"{len(soft)} TMCC soft bits, {len(burst)} burst beats, {len(out)} information beats, "
            f"{len(lw)} layout entries; {cyc} cycles in {dt:.1f} s")
    return what, bad


# --- stage 6 (and 4, 5 together: the back end) -----------------------------------

class BackBench(Elaboratable):
    """`Back` (equaliser, FrameCtl, TMCC input side, demapper, the port
    arbiter) fed as `EqBench`; the decoder port always ready."""

    def __init__(self):
        from .back import Back

        self.back = Back()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(3)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(signed(32))
        self.meta = Signal(META_SMALL)
        self.at = Signal(48)
        self.pv = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.back = bk = self.back
        eq, fc = bk.eq, bk.fc
        go = self.i_valid & self.i_ready
        k = self.kind
        p = bk.port
        m.d.comb += [
            self.i_ready.eq(1),
            eq.i_valid.eq(go & (k == K_SYM)), eq.i_y[0].eq(self.a), eq.i_y[1].eq(self.b),
            eq.i_meta.eq(Cat(self.meta, self.at)),
            fc.lw_en.eq(go & (k == K_LAY)), fc.lw_slot.eq(self.a), fc.lw_data.eq(self.meta),
            fc.lw_commit.eq(go & (k == K_COMMIT)), fc.lw_frame.eq(self.a), fc.lw_from.eq(self.b),
            eq.tap_we.eq(go & (k == K_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c),
            eq.tap_now.eq(go & (k == K_TAPNOW)),
            p.in_ready.eq(1),
            self.pv.eq(p.in_valid & (p.in_ctx == 0)),
        ]
        return m

    def ports(self):
        bk = self.back
        p, dm = bk.port, bk.demap
        out = {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
            "b": self.b, "c": self.c, "meta": self.meta, "at": self.at,
            "pv": self.pv, "pn": p.in_n, "pf": p.in_first, "pl": p.in_last, "ptag": p.in_tag,
            "lpv": dm.lp_valid, "lpa": dm.lp_addr, "lpd": dm.lp_data,
            "nxv": dm.nx_valid, "nxn0": dm.nx_n0, "nxk": dm.nx_k, "late": dm.late,
            "kk": dm.kk, "tv": bk.tmcc.s_valid, "tidx": bk.tmcc.s_idx, "tsoft": bk.tmcc.s_soft,
        }
        for i in range(5):
            out[f"pp{i}"] = p.in_pos[i]
            out[f"pl{i}"] = p.in_llr[i]
        return out


def run_back(eqdoc, demapdoc, tmccdoc, backend, symbols=None):
    """The back end on chain-3-eq's frame (whose equalised symbols chain-5
    demaps): every demapped beat of the two codewords chain-5 carries in
    full (positions and LLRs of every symbol, in label order), the beat
    count and flags of all 96 codewords, the learned points and n0, K for
    the next frame; and the TMCC soft bits against chain-4 (the same
    frame). `symbols`: a prefix of the frame (then only the codewords and
    TMCC bits it completes are compared)."""
    F, at = eqdoc["frame"], eqdoc["at"]
    assert demapdoc["frame"] == F == tmccdoc["frame"]
    y = eqdoc["y"]
    words = slot_words(eqdoc["training"])
    recs = []
    for i, t in enumerate(eqdoc["taps"][0::2]):
        recs.append((K_TAP, i, t, eqdoc["taps"][2 * i + 1], 0, 0))
    recs.append((K_TAPNOW, 0, 0, 0, 0, 0))
    recs += [r + (0,) for r in layout_records(tmccdoc["layout"], F, F - 2)]
    n = len(y) // 2 if symbols is None else min(len(y) // 2, symbols + 5)
    whole = n == len(y) // 2
    for i in range(n):
        idx = eqdoc["y_from"] + i
        rel = idx - at
        if rel < 0:
            fr, k, j = F - 1, SLOTS - 1, SLOT + rel
        elif rel >= FRAME:
            fr, k, j = F + 1, 0, rel - FRAME
        else:
            fr, k, j = F, rel // SLOT, rel % SLOT
        w = words[k] if fr == F else (0 if k == SLOTS - 1 else 1)
        recs.append((K_SYM, y[2 * i], y[2 * i + 1], 0,
                     pack_meta(j=j, number=k, numbered=1, frame=fr, word=w), idx))
    b = BackBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain5")
    gp = e.group("pv", ["pn", "pf", "pl", "ptag"] + [f"pp{i}" for i in range(5)] + [f"pl{i}" for i in range(5)])
    gl = e.group("lpv", ["lpa", "lpd"])
    gn = e.group("nxv", ["nxn0", "nxk"])
    gt = e.group("tv", ["tidx", "tsoft"])
    t0 = time.time()
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "meta", "at"], recs, tail=2000)
    beats, learned, nx, tsoft = e.read(gp), e.read(gl), e.read(gn), e.read(gt)
    late = e.get("late")
    cyc = e.cycles()
    dt = time.time() - t0
    bad = []
    by_slot = {}
    for x in beats:
        by_slot.setdefault(x[3] & 0x7F, []).append(x)
    lay = {s[0]: s for s in tmccdoc["layout"]}
    valid = [s for s in tmccdoc["layout"] if not s[4]]
    if not whole:
        valid = [s for s in valid if s[0] in by_slot and len(by_slot[s[0]]) == s[6]]
    elif sorted(by_slot) != sorted(s[0] for s in valid):
        bad.append(f"codewords of slots {sorted(by_slot)[:5]}... != the valid slots")
    for s in valid:
        got = by_slot.get(s[0], [])
        if len(got) != s[6] or not got or not got[0][1] or not got[-1][2] \
                or any(x[1] or x[2] for x in got[1:-1]):
            bad.append(f"slot {s[0]}: {len(got)} beats (expected {s[6]}) or first/last flags wrong")
            break
        tag = got[0][3]
        if (tag >> 32, tag >> 8 & 0xF, tag >> 12 & 0xF, tag >> 16 & 7) != (F, s[3], s[2], s[1]):
            bad.append(f"slot {s[0]}: tag {tag:#x}")
            break
    for sd in demapdoc["slots"]:
        if not whole and sd["slot"] not in [s[0] for s in valid]:
            continue
        nb = {1: 1, 2: 2, 3: 3, 4: 4, 5: 5}[sd["modulation"]]
        perm, lab = sd["perm"], sd["llr_label"]
        exp = []
        for s_ in range(len(sd["z"]) // 2):
            exp.append(tuple(perm[s_ * nb + q] for q in range(nb)) + tuple(lab[s_ * nb + q] for q in range(nb)))
        got = [x[4:4 + nb] + x[9:9 + nb] for x in by_slot.get(sd["slot"], [])]
        bad += first_mismatch(f"slot {sd['slot']} beat (positions, LLRs)", got, exp,
                              key=lambda i, v: f"codeword symbol {i}")
        # the de-interleaved word equals the vectors'
        word = [None] * 44880
        for g in got:
            for q in range(nb):
                word[g[q]] = g[nb + q]
        if word != sd["llr"]:
            bad.append(f"slot {sd['slot']}: the de-interleaved word differs")
    exp_l = []
    for mi, pts in enumerate(demapdoc["learned"]):
        if pts:
            exp_l += [(lb | mi << 5, (pts[2 * lb] & 0xFFFF) | (pts[2 * lb + 1] & 0xFFFF) << 16)
                      for lb in range(len(pts) // 2)]
    exp_t = list(enumerate(tmccdoc["soft"]))
    if whole:
        bad += first_mismatch("learned point", learned, exp_l, key=lambda i, v: f"label {v[0] & 31} of {v[0] >> 5}")
        bad += first_mismatch("next n0, K", nx, [(demapdoc["n0_next"], demapdoc["k_next"])])
    else:
        exp_t = exp_t[:len(tsoft)]
        if len(valid) == 0:
            bad.append("the prefix completes no codeword")
    bad += first_mismatch("TMCC soft", tsoft, exp_t, key=lambda i, v: f"bit {i}")
    if late:
        bad.append(f"K late on {late} symbols")
    del lay
    what = (f"{len(beats)} demapped beats of {len(by_slot)} codewords, {len(learned)} learned points, "
            f"{len(tsoft)} TMCC soft bits; {cyc} cycles in {dt:.1f} s")
    return what, bad


def run(stage, doc, backend, symbols):
    from .sim import load

    path = doc.get("_path", "")
    if stage == "2-phase":
        return run_phase(doc, backend)
    if stage == "3-eq":
        lay = load(path.replace("3-eq", "4-tmcc"))["layout"]
        return run_eq(doc, lay, backend, symbols)
    if stage == "4-tmcc":
        return run_tmcc(doc, backend)
    if stage == "5-demap":
        return run_back(load(path.replace("5-demap", "3-eq")), doc, load(path.replace("5-demap", "4-tmcc")),
                        backend, symbols)
    if stage == "6-chain":
        return run_chain(doc, load(path.replace("6-chain", "3-eq"))["taps"], backend, symbols)
    if stage in ("8-loss", "9-fade") and os.environ.get("FPGA_LDPC") == "stub":
        return run_chain(doc, None, backend, symbols)
    if stage == "7-hdl":
        return run_chain_real(doc, load(path.replace("7-hdl", "3-eq"))["taps"], backend, symbols)
    if stage in ("8-loss", "9-fade"):
        return run_chain_real(doc, None, backend, symbols)
    raise NotImplementedError(stage)


# --- the whole chain (stages 1-6, 8, 9 with the LDPC stub) ----------------------

C_SYM, C_TAP, C_TAPNOW, C_HEAD, C_WORD, C_LOSS = range(6)


class ChainBench(Elaboratable):
    """`SlotsChain` with the LDPC stub as records: C_SYM (x0, x1), C_TAP
    (tap a = (b, c)) and C_TAPNOW (the host's taps at open), C_HEAD (tag
    low a, tag high b, iterations c, converged d) and C_WORD (bits a, last
    b) of the stub's results; a result record waits until `d`/`c` ... its
    frame's TMCC codeword has been committed on the port (`need` in e:
    that many TMCC OP_DONEs), so the stub never answers a codeword before
    the chain has sent it."""

    def __init__(self):
        from .top import SlotsChain

        self.ch = SlotsChain()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(3)
        self.a = Signal(signed(33))
        self.b = Signal(signed(33))
        self.c = Signal(signed(32))
        self.d = Signal(signed(32))
        self.e = Signal(16)

    def elaborate(self, platform):
        m = Module()
        m.submodules.ch = ch = self.ch
        eq = ch.back.eq
        p = ch.port
        k = self.kind
        res = (k == C_HEAD) | (k == C_WORD)
        gate = ch.tmcc_done >= self.e
        m.d.comb += [
            self.i_ready.eq(Mux((k == C_SYM) | (k == C_LOSS), ch.i_ready, Mux(res, gate & p.out_ready, 1))),
            ch.i_valid.eq(self.i_valid & ((k == C_SYM) | (k == C_LOSS))),
            ch.i_loss.eq(k == C_LOSS), ch.i_lost.eq(self.a),
            ch.i_x[0].eq(self.a), ch.i_x[1].eq(self.b),
            eq.tap_we.eq(self.i_valid & (k == C_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c), eq.tap_now.eq(self.i_valid & (k == C_TAPNOW)),
            p.in_ready.eq(1),
            p.out_valid.eq(self.i_valid & res & gate),
            p.out_head.eq(k == C_HEAD),
            p.out_tag.eq(Cat(self.a[:32], self.b[:32])),
            p.out_iterations.eq(self.c), p.out_converged.eq(self.d[0]),
            p.out_bits.eq(self.a), p.out_last.eq(self.b[0]),
        ]
        return m

    def ports(self):
        ch = self.ch
        return {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
                "b": self.b, "c": self.c, "d": self.d, "e": self.e,
                "ov": ch.o_valid, "ow": ch.o_word, "tdone": ch.tmcc_done,
                "frames_done": ch.finish.counters["frames"], "tbusy": ch.back.tmcc.busy,
                "port_lost": ch.back.port_lost, "late": ch.back.demap.late,
                "overrun": ch.back.tmcc.overrun}


def stub_results(words):
    """The LDPC stub's results per frame, from the expected `Slots` words:
    {frame: [records]} in the frame order the model emits (its slots, then
    its TMCC), with `need` (the TMCC commits that must have happened: the
    frame's index + 1) filled in by the caller."""
    from . import gf
    from . import slots as S
    from .tables import SLOT_INIT, prbs

    out = {}
    frames = []
    for kind, _seq, frame, pay in S.blocks(words):
        if frame not in out:
            out[frame] = {"slots": [], "tmcc": None}
            frames.append(frame)
        if kind == S.SLOT:
            f = S.slot_fields(pay)
            if not f["dummy"]:
                out[frame]["slots"].append(f)
        elif kind == S.TMCC:
            out[frame]["tmcc"] = pay
    recs = {}
    for frame in frames:
        r = []
        sl = out[frame]["slots"]
        kbits = [374 * S.RATES[f["rate"] - 1] for f in sl]
        pr = prbs(SLOT_INIT, 22, 25, sum(kbits))
        o = 0
        for f, kb in zip(sl, kbits):
            assert f["converged"] and f["bch"] == 0, "the stub rebuilds converged, uncorrected slots only"
            word = gf.encode(f["bits"]) + [1] * 6
            assert len(word) == kb
            word = [b ^ pr[o + i] for i, b in enumerate(word)]
            o += kb
            tag = f["slot"] | f["rate"] << 8 | f["mod"] << 12 | f["mode"] << 16
            r.append((C_HEAD, tag, frame, f["iterations"], 1))
            nw = (kb + 15) // 16
            r += [(C_WORD, sum(word[16 * q + b] << b for b in range(16) if 16 * q + b < kb), int(q == nw - 1), 0, 0)
                  for q in range(nw)]
        tp = out[frame]["tmcc"]
        info = S.unpack_bits(tp[4:], 9422)
        it = tp[1]
        r.append((C_HEAD, 1 << 7 | 3 << 8, frame, 0 if it == 0xFFFF else it, int(it != 0xFFFF)))
        beats = tmcc_stub_bits(info)
        r += [(C_WORD, v, int(q == len(beats) - 1), 0, 0) for q, v in enumerate(beats)]
        recs[frame] = r
    return frames, recs


def block_diff(got, exp, limit=12):
    """The blocks that differ: (index, kind, frame, the first payload word
    that differs, its values)."""
    from . import slots as S

    out = []
    try:
        gb = S.blocks(got)
    except AssertionError as err:
        return [f"the output does not parse as Slots blocks near word {err}"]
    eb = S.blocks(exp)
    for i, (g_, e_) in enumerate(zip(gb, eb)):
        if g_ != e_:
            w = next((k for k, (x, y) in enumerate(zip(g_[3], e_[3])) if x != y), None)
            out.append(f"block {i} (kind {e_[0]}, frame {e_[2]}): payload word {w}: "
                       f"{g_[3][w] if w is not None else len(g_[3])} != {e_[3][w] if w is not None else len(e_[3])}")
            if len(out) >= limit:
                break
    return out


def run_chain(doc, taps, backend, symbols=None):
    """The whole chain on chain-6: the input symbols in, the `Slots` words
    out, compared word for word with the model's (`slots_words`), with the
    host's taps at open (chain-3's: the model's run wrote the same) and the
    LDPC stub's results after each frame's TMCC codeword (frame order)."""
    from .common import FRAME

    x = doc["input"]
    n = len(x) // 2 if symbols is None else min(symbols, len(x) // 2)
    frames, res = stub_results(doc["slots_words"])
    ats = {f["frame"]: f["at"] for f in doc["frames"]}
    inject = {}
    for i, fr in enumerate(frames):
        # its last symbol needs the FIR's 5 look-ahead symbols, which stage 3
        # corrects once the head after the next slot's is in: two slots on
        at = ats[fr] + FRAME + 2 * 9296
        # the chain's index counts the lost symbols; the input's does not
        at -= sum(k for b, k in doc.get("losses", []) if b + k <= at)
        if at <= n:  # a prefix: only the frames it completes
            inject.setdefault(at, []).extend([r + (i + 1,) for r in res[fr]])
    recs = []
    if taps is not None:
        recs = [(C_TAP, i, taps[2 * i], taps[2 * i + 1], 0, 0) for i in range(11)] + [(C_TAPNOW, 0, 0, 0, 0, 0)]
    losses = {at: k for at, k in doc.get("losses", [])}
    for i in range(n):
        recs.extend(inject.get(i, []))
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0, 0, 0))
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0, 0, 0))
    recs.extend(inject.get(n, []))
    b = ChainBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain6")
    e.stall_limit = 3_000_000
    g = e.group("ov", ["ow"])
    t0 = time.time()
    stuck = None
    try:
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "d", "e"], recs, tail=200_000)
    except RuntimeError as err:
        stuck = f"{err} (record kinds around: {[r[0] for r in recs[max(0, e.stalled - 2):e.stalled + 3]]})"
    got = [w for (w,) in e.read(g)]
    import json
    import os

    with open(os.path.join(os.path.dirname(__file__), "..", "..", "build", "chain-6-got.json"), "w") as f:
        json.dump(got, f)
    diag = {k: e.get(k) for k in ("tdone", "port_lost", "late", "overrun", "frames_done", "tbusy")}
    cyc = e.cycles()
    dt = time.time() - t0
    exp = doc["slots_words"]
    bad = [stuck] if stuck else []
    if symbols is not None:
        exp = exp[:len(got)]
    for i, (a_, b_) in enumerate(zip(got, exp)):
        if a_ != b_:
            bad.append(f"Slots word {i}: {a_:#06x} != {b_:#06x}")
            break
    if bad:
        bad += block_diff(got, exp)
    if len(got) != len(exp):
        bad.append(f"{len(got)} Slots words, {len(exp)} expected")
    for k_, v in diag.items():
        if k_ not in ("tdone", "frames_done", "tbusy") and v:
            bad.append(f"{k_} {v}")
    what = (f"{n} symbols in, {len(got)} Slots words out ({len(frames)} frames, {diag}); "
            f"{cyc} cycles in {dt:.1f} s")
    return what, bad


# --- the whole chain with the real decoder ---------------------------------------



class RealChainBench(Elaboratable):
    """`SlotsChain` with the real decoder: `LdpcBridge` and `ldpc.LdpcDecoder`
    (S = 34, K = 3, every rate; the `ldpc` domain, 5x the symbol clock in
    the simulation), MAX_ITER 16. Records: C_SYM (x0, x1), C_TAP / C_TAPNOW
    (the host's taps at open), C_LOSS (a symbols lost before the next)."""

    def __init__(self, units=34, banks=3):
        from amaranth.hdl import DomainRenamer

        from ..ldpc import LdpcDecoder
        from .ldpc_if import LdpcBridge
        from .top import SlotsChain

        self.ch = SlotsChain(max_iter=16)
        self.br = LdpcBridge(self.ch.port)
        self.dec = LdpcDecoder(units, banks=banks, sim=True)  # the CXXRTL-safe build
        self.dec_r = DomainRenamer("ldpc")(self.dec)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(3)
        self.a = Signal(signed(33))
        self.b = Signal(signed(33))
        self.c = Signal(signed(32))

    def elaborate(self, platform):
        m = Module()
        m.submodules.ch = ch = self.ch
        m.submodules.br = br = self.br
        m.submodules.dec = self.dec_r
        from .ldpc_if import connect_decoder

        connect_decoder(m, br, self.dec)
        eq = ch.back.eq
        k = self.kind
        go = self.i_valid & self.i_ready
        m.d.comb += [
            self.i_ready.eq(Mux((k == C_SYM) | (k == C_LOSS), ch.i_ready, 1)),
            ch.i_valid.eq(self.i_valid & ((k == C_SYM) | (k == C_LOSS))),
            ch.i_loss.eq(k == C_LOSS), ch.i_lost.eq(self.a),
            ch.i_x[0].eq(self.a), ch.i_x[1].eq(self.b),
            eq.tap_we.eq(go & (k == C_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c), eq.tap_now.eq(go & (k == C_TAPNOW)),
            ch.r_overrun.eq(br.out_overrun),
        ]
        return m

    def ports(self):
        ch = self.ch
        return {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
                "b": self.b, "c": self.c,
                "ov": ch.o_valid, "ow": ch.o_word, "tdone": ch.tmcc_done,
                "frames_done": ch.finish.counters["frames"], "port_lost": ch.back.port_lost,
                "late": ch.back.demap.late, "overrun": ch.back.tmcc.overrun,
                "in_lost": self.br.in_lost, "dec_overruns": self.dec.overruns,
                "queue_overflow": self.dec.queue_overflow}


def blocks_by_frame(words):
    """{(frame, kind, slot): payload} of a `Slots` stream (slot: the slot
    index for a slot block, else 0), and the stream's block order as
    [(frame, kind, slot)]: the comparison tolerant of the decoder's result
    order (a frame's TMCC and status blocks may follow the next frame's
    first slot blocks: the host re-associates them by frame counter)."""
    from . import slots as S

    out, order, twice = {}, [], []
    for kind, _seq, frame, pay in S.blocks(words):
        key = (frame, kind, pay[0] & 0x7F if kind == S.SLOT else 0)
        if key in out:
            twice.append(key)
        out[key] = pay
        order.append(key)
    return out, order, twice


def lateness(order):
    """The largest number of the next frame's slot blocks that precede a
    frame's TMCC block (0 in the model's order)."""
    from . import slots as S

    worst = 0
    for i, (frame, kind, _s) in enumerate(order):
        if kind == S.TMCC:
            before = sum(1 for f, k, _ in order[:i] if k == S.SLOT and f > frame)
            worst = max(worst, before)
    return worst


def run_chain_real(doc, taps, backend="cxxrtl", symbols=None):
    """The whole chain with the real decoder on a whole-chain set written
    with the decoder's settings (chain-7-hdl, chain-8-loss, chain-9-fade):
    every block of the model's stream, compared per (frame, kind, slot)."""
    x = doc["input"]
    n = len(x) // 2 if symbols is None else min(symbols, len(x) // 2)
    losses = {at: k for at, k in doc.get("losses", [])}
    recs = []
    if taps is not None:
        recs = [(C_TAP, i, taps[2 * i], taps[2 * i + 1]) for i in range(11)] + [(C_TAPNOW, 0, 0, 0)]
    # the front end's rate: 33.756 Msymbol/s in the 40 MHz clock (an idle
    # cycle after about every 5.4 symbols), so the TMCC burst gets the
    # free port cycles it has on the board
    gaps = [0] * len(recs)
    acc = 0.0
    for i in range(n):
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0))
            gaps.append(0)
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0))
        acc += 40e6 / 33.7561e6 - 1
        gaps.append(int(acc))
        acc -= int(acc)
    b = RealChainBench()
    p = b.ports()
    t0 = time.time()
    e = engine(b, p, backend, tag="chainreal")
    t_build = time.time() - t0
    e.stall_limit = 3_000_000
    g = e.group("ov", ["ow"])
    t0 = time.time()
    stuck = None
    try:
        e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], recs, tail=1_500_000, gaps=gaps)
    except RuntimeError as err:
        stuck = str(err)
    got = [w for (w,) in e.read(g)]
    import json

    with open(os.path.join(os.path.dirname(__file__), "..", "..", "build", f"chain-real-got-{n}.json"), "w") as f:
        json.dump(got, f)
    diag = {k: e.get(k) for k in ("tdone", "port_lost", "late", "overrun", "frames_done", "in_lost",
                                  "dec_overruns", "queue_overflow")}
    cyc = e.cycles()
    dt = time.time() - t0
    bad = [stuck] if stuck else []
    try:
        gb, gorder, twice = blocks_by_frame(got)
    except AssertionError as err:
        return "", bad + [f"the output does not parse as Slots blocks: {err}"]
    eb, _, _ = blocks_by_frame(doc["slots_words"])
    if symbols is not None:
        # a prefix: the frames it completes
        done = {k[0] for k in gb}
        eb = {k: v for k, v in eb.items() if k[0] in done}
    # frames the model did not send (a frame dropped, or not finished when
    # the input ended): the chain sends the slot blocks it decoded before
    # (README "Integration"); they are reported, not compared
    model_frames = {k[0] for k in eb}
    extra = sorted({k[0] for k in gb} - model_frames)
    extra_blocks = {f: sum(1 for k in gb if k[0] == f) for f in extra}
    gb = {k: v for k, v in gb.items() if k[0] in model_frames}
    bad += [f"block {k} twice" for k in twice if k[0] in model_frames]
    for k_ in sorted(set(eb) | set(gb)):
        if k_ not in gb:
            bad.append(f"block {k_} missing")
        elif k_ not in eb:
            bad.append(f"block {k_} not in the model's stream")
        elif gb[k_] != eb[k_]:
            w = next(i for i, (u, v) in enumerate(zip(gb[k_], eb[k_])) if u != v) \
                if len(gb[k_]) == len(eb[k_]) else None
            bad.append(f"block {k_}: payload word {w} differs" if w is not None
                       else f"block {k_}: {len(gb[k_])} words, {len(eb[k_])} expected")
        if len(bad) > 12:
            break
    for k_ in ("port_lost", "late", "overrun", "in_lost", "dec_overruns", "queue_overflow"):
        if diag[k_]:
            bad.append(f"{k_} {diag[k_]}")
    what = (f"{n} symbols in, {len(got)} Slots words, {len(gb)} blocks of the model's frames ({diag}); "
            f"blocks of frames the model did not send: {extra_blocks}; lateness {lateness(gorder)} "
            f"slot blocks; build {t_build:.0f} s, {cyc} cycles in {dt:.1f} s ({cyc / max(dt, 1e-9):.0f}/s)")
    return what, bad
