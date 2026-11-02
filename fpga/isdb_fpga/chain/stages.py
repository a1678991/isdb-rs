"""Per-stage simulation of the chain's blocks against the vectors (the
stages after stage 1; `sim.py` dispatches here). Each stage is driven
through a small bench wrapper that takes one record per cycle from the
engine's input stream and routes it to the block's ports, so the compiled
backend (`simx.py`) can feed millions of records without Python in the
loop.
"""

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
    """Records: kind 0 = an NCO output (at, r0, r1); kind 1 = a head (at,
    word, theta, amp, numbered, number, frame)."""

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

    def elaborate(self, platform):
        m = Module()
        m.submodules.dut = d = self.dut
        sym = self.i_valid & self.i_ready & (self.kind == 0)
        head = self.i_valid & self.i_ready & (self.kind == 1)
        m.d.comb += [
            self.i_ready.eq(~d.full),
            d.i_valid.eq(sym), d.i_at.eq(self.at), d.i_r[0].eq(self.a), d.i_r[1].eq(self.b),
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
            "number": self.number, "frame": self.frame,
            "ov": d.o_valid, "oat": d.o_at, "oj": d.o_j, "ophi": d.o_phi, "og": d.o_gain,
            "oy0": d.o_y[0], "oy1": d.o_y[1], "or0": d.o_r[0], "or1": d.o_r[1],
        }


def head_record(h):
    at, word, _c0, _c1, theta, amp, number, frame = h
    theta_s = theta - (1 << 32) if theta >= 1 << 31 else theta
    return (1, at, theta_s, amp, word, int(number >= 0), max(number, 0), max(frame, 0))


def run_phase(doc, backend):
    heads = doc["heads"]
    r = doc["r"]
    a0 = heads[0][0]
    recs = []
    # the slots' symbols; each head after its window's last symbol (as
    # the model emits it, 24 symbols in), the stream padded past the last
    n = len(r) // 2
    pad = 24 + 256
    hq = {h[0] + 24: h for h in heads}
    for i in range(n + pad):
        at = a0 + i
        if at in hq:
            recs.append(head_record(hq[at]))
        v = (r[2 * i], r[2 * i + 1]) if i < n else (0, 0)
        recs.append((0, at, v[0], v[1], 0, 0, 0, 0))
    b = PhaseBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain2")
    g = e.group("ov", ["oat", "oj", "ophi", "og", "oy0", "oy1", "or0", "or1"])
    t0 = time.time()
    e.run("i_valid", "i_ready", ["kind", "at", "a", "b", "word", "numbered", "number", "frame"],
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
            bch.q_beat.eq(r.bch_q), r.bch_mask.eq(bch.o_mask),
        ]
        return m

    def ports(self):
        s, r, lay = self.soft, self.res, self.res.layout
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
            "b": self.b, "c": self.c, "d": self.d,
            "sv": s.s_valid, "sidx": s.s_idx, "ssoft": s.s_soft,
            "bv": self.taken, "bop": s.b_op, "bpos": s.b_pos, "bllr": s.b_llr, "btag": s.b_tag,
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
    gb = e.group("bv", ["bop", "bpos", "bllr", "btag"])
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
    got_b = [(x[0],) if x[0] != OP_WRITE else (x[0], x[1], x[2]) for x in burst]
    bad += first_mismatch("burst", got_b, exp_b, key=lambda i, v: f"beat {i}")
    tags = {x[3] for x in burst}
    if tags != {TAG_TMCC | (F & 0xFF) << 8}:
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
    raise NotImplementedError(stage)
