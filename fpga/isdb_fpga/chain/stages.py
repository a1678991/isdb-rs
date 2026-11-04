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

from . import monitors as mon
from .common import EQ_APPLY_FRAMES, FRAME, FSYNC, PILOTS, SLOT, SLOTS, SSYNC, SYNC, SYNC_REF, word_symbols
from .eq import ENTRIES, META, META_SMALL, Equaliser
from .frame import FrameCtl, lay_pack
from .phase import SlotPhase
from .simx import engine
from .sim import first_mismatch


def monitored(bench, monitors=True, rename=None):
    """The bench's invariant monitors (monitors.py): enabled in every
    monitored module it holds (`monitors`), and `bench.mon` their
    `Collector`, whose ports the bench's `ports()` adds and whose report
    each run adds to its mismatches."""
    if monitors:
        mon.enable(bench, rename)
    bench.mon = mon.Collector(bench)
    return bench


# --- stage 3 -------------------------------------------------------------------

class PhaseBench(Elaboratable):
    """Records: kind 0 = an NCO input with its phase (at, x0, x1, phi); kind
    1 = a head (at, word, theta, amp, numbered, number, frame, and in phi the
    carrier register after it). `snapshots`: the status snapshot as
    `SyncFront` gives it, a register and its strobe: after head k's record
    its decision's snapshot (1000 + k) 6 cycles later, then a period end's
    (2000 + k) 40 cycles after that, as when the slot sync locked a few
    positions before its period's end (16-bit snapshots); else none."""

    def __init__(self, in_bits=6, snapshots=False):
        self.snapshots = snapshots
        self.dut = SlotPhase(in_bits, snap_bits=16 if snapshots else 1)
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
        if self.snapshots:
            k = Signal(16)
            t = Signal(8)
            snap = Signal(16)
            strobe = Signal()
            with m.If(head):
                m.d.sync += t.eq(1)
            with m.Elif(t == 46):
                m.d.sync += [t.eq(0), k.eq(k + 1)]
            with m.Elif(t != 0):
                m.d.sync += t.eq(t + 1)
            m.d.sync += strobe.eq((t == 6) | (t == 46))
            with m.If(t == 6):
                m.d.sync += snap.eq(1000 + k)
            with m.If(t == 46):
                m.d.sync += snap.eq(2000 + k)
            m.d.comb += [d.h_snap.eq(snap), d.h_snap_valid.eq(strobe)]
        else:
            m.d.comb += d.h_snap_valid.eq(1)  # (no snapshot here)
        return m

    def ports(self):
        d = self.dut
        return {
            "i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "at": self.at,
            "a": self.a, "b": self.b, "word": self.word, "numbered": self.numbered,
            "number": self.number, "frame": self.frame, "phi": self.phi,
            "ov": d.o_valid, "oat": d.o_at, "oj": d.o_j, "ophi": d.o_phi, "og": d.o_gain,
            "oy0": d.o_y[0], "oy1": d.o_y[1], "or0": d.o_r[0], "or1": d.o_r[1], "osnap": d.o_snap,
        }


def head_record(h, f=0):
    at, word, _c0, _c1, theta, amp, number, frame = h
    theta_s = theta - (1 << 32) if theta >= 1 << 31 else theta
    return (1, at, theta_s, amp, word, int(number >= 0), max(number, 0), max(frame, 0), f & 0xFFFFFFFF)


def run_phase(doc, backend, snapshots=False):
    """Stage 3 on chain-2-phase (`PhaseBench`). `snapshots`: also the
    status snapshot a slot carries (`PhaseBench` with `snapshots`, the
    first head numbered 0, so the first slot is a frame's slot 0 and its
    snapshot goes to `o_snap`): it must be head 1's decision's (1001), the
    model's state after the push that gives the head, not the later
    period end's (2001)."""
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
            rec = head_record(hq[at], f_at(at - a0) if at - a0 < n else 0)
            if snapshots and at == a0 + 24:
                rec = rec[:5] + (1, 0) + rec[7:]  # numbered, slot 0
            recs.append(rec)
        v = (x[2 * i], x[2 * i + 1], phi[i]) if i < n else (0, 0, 0)
        recs.append((0, at, v[0], v[1], 0, 0, 0, 0, v[2]))
    b = PhaseBench(snapshots=snapshots)
    p = b.ports()
    e = engine(b, p, backend, tag="chain2s" if snapshots else "chain2")
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
    if snapshots and e.get("osnap") != 1001:
        bad.append(f"the frame's snapshot {e.get('osnap')}, head 1's (1001) expected")
    what = f"{len(got)} symbols corrected over 2 slots; {cyc} cycles in {dt:.1f} s"
    return what, bad


# --- stage 4 -------------------------------------------------------------------

K_SYM, K_READ, K_LAY, K_COMMIT, K_TAP, K_TAPNOW, K_CAP, K_OPEN, K_RESTART = range(9)


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
    for frame a (from frame b); K_TAP tap a = (b, c); K_TAPNOW; K_CAP the
    host's SUMS_CAPTURE strobe; K_RESTART the chain's restart (START)."""

    def __init__(self, monitors=True):
        self.eq = Equaliser()
        self.fc = FrameCtl()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(4)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(signed(32))
        self.meta = Signal(META_SMALL)  # the meta without `at` (ports <= 64 bits)
        self.at = Signal(48)
        self.o_ms = Signal(META_SMALL)
        self.o_at = Signal(48)
        self.rd_lat = Signal(16)  # cycles from a read's request to its rd_ack
        self.rd_lat_max = Signal(16)
        monitored(self, monitors)

    def elaborate(self, platform):
        m = Module()
        m.submodules.eq = eq = self.eq
        m.submodules.fc = fc = self.fc
        wire_eq_frame(m, eq, fc)
        with m.If(eq.rd_req):
            m.d.sync += self.rd_lat.eq(1)
        with m.Elif(eq.rd_busy):
            m.d.sync += self.rd_lat.eq(self.rd_lat + 1)
        with m.If(eq.rd_ack & (self.rd_lat > self.rd_lat_max)):
            m.d.sync += self.rd_lat_max.eq(self.rd_lat)
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
            eq.cap_strobe.eq(go & (k == K_CAP)),
            eq.restart.eq(go & (k == K_RESTART)), fc.restart.eq(go & (k == K_RESTART)),
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
            "cvalid": e.cap_valid, "carmed": e.cap_armed, "cseq": e.cap_seq, "rlat": self.rd_lat_max,
            **self.mon.ports(),
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
    gaps = [0] * len(recs)
    if whole:
        # read once the frame has closed (its last symbol at the FIR's
        # centre, a few cycles after the last input: a sum read is answered
        # in 3 cycles and would see the latch before the close)
        gaps[-1] = 64  # idle cycles after the last symbol
        recs += [(K_READ, a, 0, 0, 0) for a in range(4 * ENTRIES)]
        gaps += [0] * (4 * ENTRIES)
    b = EqBench()
    p = b.ports()
    e = engine(b, p, backend, tag="chain3")
    gz = e.group("ov", ["oz0", "oz1", "ometa"])
    gd = e.group("dv", ["dat", "d0", "d1"])
    gr = e.group("rack", ["rdata", "sframe", "scount"])
    t0 = time.time()
    recs = [r if len(r) == 6 else r + (0,) for r in recs]
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "meta", "at"], recs, tail=100, gaps=gaps)
    zs = e.read(gz)
    ds = e.read(gd)
    rs = e.read(gr)
    cyc = e.cycles()
    dt = time.time() - t0
    bad = b.mon.report(e.get)
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


def run_eq_capture(doc, layout, backend, strobe=True, read_during=False, release=False):
    """The training sums' capture (SUMS_CAPTURE) over chain-3-eq's frame F
    sent three times as frames F, F + 1, F + 2 (each with its layout), the
    strobe (`strobe`) before F: the latch takes F's sums and holds them
    while F + 1 and F + 2 are summed and close; read after F + 2, the sums
    are F's (the model's, bit for bit), with VALID set and SEQ 1 (one
    latch). Without the strobe the latch moves on to F + 2 (the frame
    number checked, SEQ 3). `read_during`: the 308 words are also read in
    frame F + 1 while the MAC unit sums its slot 10 (issue #27): F's sums
    again, every read answered 3 cycles after its request (it waited for
    the MAC unit before). `release` (with the strobe): after the reads a
    restart (START), its layout again and the frame once more as F + 3:
    the restart released the hold, so F + 3's end latches (SUMS_FRAME
    F + 3, SEQ 2, VALID and ARMED clear; without the release the latch
    would stay on F). Returns (what, bad)."""
    F, at = doc["frame"], doc["at"]
    y = doc["y"]
    words = slot_words(doc["training"])
    recs = []
    for i, t in enumerate(doc["taps"][0::2]):
        recs.append((K_TAP, i, t, doc["taps"][2 * i + 1], 0))
    recs.append((K_TAPNOW, 0, 0, 0, 0))
    # F's layout now, F + r + 1's a slot into frame F + r (one queued at
    # a time, as the TMCC stage commits them)
    recs += layout_records(layout, F, F - 2)
    if strobe:
        recs.append((K_CAP, 0, 0, 0, 0))
    n = len(y) // 2
    rel0 = doc["y_from"] - at  # the first symbol's position (before F)
    body = [i for i in range(n) if 0 <= doc["y_from"] + i - at < FRAME]
    reps = 4 if release else 3
    for r in range(reps):
        if r == 3:
            # the host's reads of the held set, then START: the hold goes,
            # the frame bookkeeping restarts (its layout for F + 3 again)
            recs += [(K_READ, a, 0, 0, 0) for a in range(4 * ENTRIES)]
            recs.append((K_RESTART, 0, 0, 0, 0))
            recs += [x + (0,) for x in layout_records(layout, F + 3, F + 1)]
        idx_list = ([i for i in range(n) if doc["y_from"] + i - at < 0] if r in (0, 3) else []) + body
        if r == reps - 1:
            idx_list += [i for i in range(n) if doc["y_from"] + i - at >= FRAME]
        for q, i in enumerate(idx_list):
            if r < 2 and q == len(idx_list) // 2:
                recs += [x + (0,) for x in layout_records(layout, F + r + 1, F + r - 1)]
            rel = doc["y_from"] + i - at
            if read_during and r == 1 and rel == 10 * SLOT + 70:
                # the slot's MACs (from its position 55 on) are under way
                recs += [(K_READ, a, 0, 0, 0) for a in range(4 * ENTRIES)]
            if rel < 0:
                fr, k, j = F + r - 1, SLOTS - 1, SLOT + rel
            elif rel >= FRAME:
                fr, k, j = F + r + 1, 0, rel - FRAME
            else:
                fr, k, j = F + r, rel // SLOT, rel % SLOT
            w = words[k] if F <= fr <= F + reps - 1 else (0 if k == SLOTS - 1 else 1)
            recs.append((K_SYM, y[2 * i], y[2 * i + 1], 0,
                         pack_meta(j=j, number=k, numbered=1, frame=fr, word=w), at + r * FRAME + rel))
    del rel0
    recs += [(K_READ, a, 0, 0, 0) for a in range(4 * ENTRIES)]
    b = EqBench()
    e = engine(b, b.ports(), backend, tag="chain3")
    gr = e.group("rack", ["rdata", "sframe", "scount", "cvalid", "carmed", "cseq"])
    t0 = time.time()
    recs = [r if len(r) == 6 else r + (0,) for r in recs]
    e.run("i_valid", "i_ready", ["kind", "a", "b", "c", "meta", "at"], recs, tail=100)
    rs = e.read(gr)
    lat = e.get("rlat")
    bad = b.mon.report(e.get)
    exp_r = []
    for v in doc["sums"]["r"] + doc["sums"]["p"]:
        (rh, rl), (ih, il) = v
        exp_r += [rl, rh & 0xFFFFFFFF, il, ih & 0xFFFFFFFF]
    sets = [rs[k:k + len(exp_r)] for k in range(0, len(rs), len(exp_r))]
    if len(sets) != 1 + int(read_during) + int(release):
        bad.append(f"{len(rs)} sum words read")
    if strobe:
        # F's sums (F + 1's and F + 2's differ a little: their first
        # training lines hold the end of the repeated frame); with the
        # release the last set is F + 3's (not checked word for word)
        for set_ in sets[:len(sets) - int(release)]:
            bad += first_mismatch("sums word", [(r[0],) for r in set_], [(v,) for v in exp_r],
                                  key=lambda i, v: f"entry {i // 4} part {i % 4}")
        if release and len(sets) >= 2 and tuple(sets[-2][0][1:]) != (F, doc["sums"]["count"], 1, 0, 1):
            bad.append(f"before the restart the held set read {tuple(sets[-2][0][1:])}, not F's")
    want = ((F + 3, doc["sums"]["count"], 0, 0, 2) if release
            else (F if strobe else F + 2, doc["sums"]["count"], int(strobe), 0, 1 if strobe else 3))
    got = tuple(rs[-1][1:]) if rs else None
    if got != want:
        bad.append(f"sums frame/count/valid/armed/seq {got} != {want}")
    if lat != 3:
        bad.append(f"a sum read waited {lat} cycles for its rd_ack (3 expected)")
    what = (f"{len(rs)} sum words, frame/count/valid/armed/seq {got}, read latency {lat}; "
            f"{e.cycles()} cycles in {time.time() - t0:.1f} s")
    return what, bad


# --- stage 5 -------------------------------------------------------------------

T_SYM, T_DONE, T_HEAD, T_BEAT = range(4)


class TmccBench(Elaboratable):
    """TmccSoft (soft bits, staging buffer, burst) and TmccResult (with its
    own BCH unit here) between a stub decoder: records T_SYM (z0, z1, odd,
    idx), T_DONE (frame), T_HEAD (iterations, converged, frame) and T_BEAT
    (bits, last) of the stub's result. The port grant follows a fixed
    pseudo-random pattern (the demapper's priority, about half the cycles)."""

    def __init__(self, monitors=True):
        from .finish import FastBch
        from .tmcc import TMCC_DATA, TmccResult, TmccSoft

        self.soft = TmccSoft()
        self.res = TmccResult()
        self.bch = FastBch([TMCC_DATA])
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(2)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(32)
        self.d = Signal(16)
        self.taken = Signal()
        monitored(self, monitors)

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
            **self.mon.ports(),
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
    """Stage 5 on chain-4-tmcc (or its `noisy` record). `flips`: decoder
    output positions to flip (bit errors the BCH must correct; the expected
    result is then the vectors' with that many corrections more); the
    record's own `errors` (its decoder output's flips, counted in its
    `bch_corrected`) are flipped too."""
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
    for p_ in list(doc.get("errors", ())) + list(flips):
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
    bad = bn.mon.report(e.get)
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

    def __init__(self, monitors=True):
        from .back import Back

        self.back = Back()
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(4)
        self.a = Signal(signed(32))
        self.b = Signal(signed(32))
        self.c = Signal(signed(32))
        self.meta = Signal(META_SMALL)
        self.at = Signal(48)
        self.pv = Signal()
        monitored(self, monitors)

    def elaborate(self, platform):
        from .ldpc_if import OP_WRITE

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
            fc.lw_open.eq(go & (k == K_OPEN)),
            fc.lw_en.eq(go & (k == K_LAY)), fc.lw_slot.eq(self.a), fc.lw_data.eq(self.meta),
            fc.lw_commit.eq(go & (k == K_COMMIT)), fc.lw_frame.eq(self.a), fc.lw_from.eq(self.b),
            eq.tap_we.eq(go & (k == K_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c),
            eq.tap_now.eq(go & (k == K_TAPNOW)),
            p.in_ready.eq(1),
            # the demapper's beats (not the abort beat after a drop, back.py)
            self.pv.eq(p.in_valid & (p.in_ctx == 0) & (p.in_op == OP_WRITE)),
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
            **self.mon.ports(),
        }
        for i in range(5):
            out[f"pp{i}"] = p.in_pos[i]
            out[f"pl{i}"] = p.in_llr[i]
        return out


def run_back(eqdoc, demapdoc, tmccdoc, backend, symbols=None, next_start=False):
    """The back end on chain-3-eq's frame (whose equalised symbols chain-5
    demaps): every demapped beat of the two codewords chain-5 carries in
    full (positions and LLRs of every symbol, in label order), the beat
    count and flags of all 96 codewords, the learned points and n0, K for
    the next frame; and the TMCC soft bits against chain-4 (the same
    frame). `symbols`: a prefix of the frame (then only the codewords and
    TMCC bits it completes are compared). `next_start`: the next frame has
    a layout too and its first 72 symbols (the frame's own at the same
    positions: sync, pilots, 16 main symbols) follow the frame's last at
    once, as on a stream without gaps: its start reaches the demapper a
    cycle after the frame's end, before the noise division, and its first
    main symbols need the new K (`late`); its beats are not compared."""
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
    if next_start:
        assert whole
        # into a free bank (K_OPEN: FrameCtl's lw_open, as the TMCC stage)
        recs.append((K_OPEN, 0, 0, 0, 0, 0))
        recs += [r + (0,) for r in layout_records(tmccdoc["layout"], F + 1, F - 1)]
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
    if next_start:
        # rel FRAME + 5 .. FRAME + 76: the next frame's symbols 5 .. 76, so
        # the FIR puts out its symbols 0 .. 71
        for j in range(5, SYNC + PILOTS + 5 + 16):
            i = j + (at - eqdoc["y_from"])
            recs.append((K_SYM, y[2 * i], y[2 * i + 1], 0,
                         pack_meta(j=j, number=0, numbered=1, frame=F + 1, word=1), at + FRAME + j))
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
    beats = [x for x in beats if x[3] >> 32 == F]
    late = e.get("late")
    cyc = e.cycles()
    dt = time.time() - t0
    bad = b.mon.report(e.get)
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
    if stage in ("7-hdl", "10-bch"):
        return run_chain_real(doc, load(path.replace(stage, "3-eq"))["taps"], backend, symbols)
    if stage in ("8-loss", "9-fade", "11-taps", "12-fail", "13-modes"):
        return run_chain_real(doc, None, backend, symbols)
    raise NotImplementedError(stage)


# --- the whole chain (stages 1-6, 8, 9 with the LDPC stub) ----------------------

C_SYM, C_TAP, C_TAPNOW, C_HEAD, C_WORD, C_LOSS, C_START, C_TAPARM, C_AGC = range(9)


def host_records(doc, width=4):
    """The host's register writes a whole-chain set's run assumed (its
    `host_writes`, `examples/chainvectors`), as bench records by the input
    index they come before: TAPS (11 C_TAP) then TAPS_FROM (C_TAPARM, a = the
    frame: the taps in use from frame a + EQ_APPLY_FRAMES, as `SlotsRx`'s
    REG_TAPS_FROM write arms the equaliser), AGC (C_AGC, a = the gain).
    Records padded with zeros to `width` fields."""
    out = {}
    for w in doc.get("host_writes", ()):
        recs = out.setdefault(w["at"], [])
        if "taps" in w:
            t = w["taps"]
            recs += [(C_TAP, k, t[2 * k], t[2 * k + 1]) for k in range(11)]
            recs.append((C_TAPARM, w["taps_from"], 0, 0))
        if "agc" in w:
            recs.append((C_AGC, w["agc"], 0, 0))
    return {at: [r + (0,) * (width - len(r)) for r in recs] for at, recs in out.items()}


class ChainBench(Elaboratable):
    """`SlotsChain` with the LDPC stub as records: C_SYM (x0, x1), C_TAP
    (tap a = (b, c)) and C_TAPNOW (the host's taps at open), C_HEAD (tag
    low a, tag high b, iterations c, converged d) and C_WORD (bits a, last
    b) of the stub's results; a result record waits until `d`/`c` ... its
    frame's TMCC codeword has been committed on the port (`need` in e:
    that many TMCC OP_DONEs), so the stub never answers a codeword before
    the chain has sent it. The chain's stop (`SlotsChain.stop_after`) is
    off: the stub's results of a frame come as one run of records, with
    no input for longer than a stop. C_TAPARM and C_AGC: the host's TAPS_FROM
    and AGC writes (`host_records`)."""

    def __init__(self, monitors=True):
        from .top import SlotsChain

        self.ch = SlotsChain(stop_after=None)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(4)
        self.a = Signal(signed(33))
        self.b = Signal(signed(33))
        self.c = Signal(signed(32))
        self.d = Signal(signed(32))
        self.e = Signal(16)
        monitored(self, monitors)

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
            eq.tap_arm.eq(self.i_valid & (k == C_TAPARM)), eq.tap_at.eq(self.a + EQ_APPLY_FRAMES),
            eq.tap_from_in.eq(self.a),
            p.in_ready.eq(1),
            p.out_valid.eq(self.i_valid & res & gate),
            p.out_head.eq(k == C_HEAD),
            p.out_tag.eq(Cat(self.a[:32], self.b[:32])),
            p.out_iterations.eq(self.c), p.out_converged.eq(self.d[0]),
            p.out_bits.eq(self.a), p.out_last.eq(self.b[0]),
        ]
        with m.If(self.i_valid & (k == C_AGC)):
            m.d.sync += ch.agc.eq(self.a)
        return m

    def ports(self):
        ch = self.ch
        return {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
                "b": self.b, "c": self.c, "d": self.d, "e": self.e,
                "ov": ch.o_valid, "ow": ch.o_word, "tdone": ch.tmcc_done,
                "frames_done": ch.finish.counters["frames"], "tbusy": ch.back.tmcc.busy,
                "port_lost": ch.back.port_lost, "late": ch.back.demap.late,
                "overrun": ch.back.tmcc.overrun, **self.mon.ports()}


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


def chain_records(doc, taps, n):
    """`ChainBench` records for the first `n` input symbols of a
    whole-chain set: the host's taps at open, the symbols (and the set's
    losses), the LDPC stub's results after each frame's TMCC codeword."""
    from .common import FRAME

    x = doc["input"]
    frames, res = stub_results(doc["slots_words"])
    ats = {f["frame"]: f["at"] for f in doc["frames"]}
    inject = {}
    for i, fr in enumerate(frames):
        at = ats[fr] + FRAME + 2 * 9296
        at -= sum(k for b, k in doc.get("losses", []) if b + k <= at)
        if at <= n:
            inject.setdefault(at, []).extend([r + (i + 1,) for r in res[fr]])
    recs = []
    if taps is not None:
        recs = [(C_TAP, i, taps[2 * i], taps[2 * i + 1], 0, 0) for i in range(11)] + [(C_TAPNOW, 0, 0, 0, 0, 0)]
    losses = {at: k for at, k in doc.get("losses", [])}
    host = host_records(doc, 6)
    for i in range(n):
        recs.extend(inject.get(i, []))
        recs.extend(host.get(i, []))
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0, 0, 0))
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0, 0, 0))
    recs.extend(inject.get(n, []))
    return recs


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
    host = host_records(doc, 6)
    for i in range(n):
        recs.extend(inject.get(i, []))
        recs.extend(host.get(i, []))
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
    bad = ([stuck] if stuck else []) + b.mon.report(e.get)
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
    the simulation), MAX_ITER `ldpc.DEFAULT_MAX_ITER` (14). Records: C_SYM (x0, x1), C_TAP / C_TAPNOW
    (the host's taps at open), C_LOSS (a symbols lost before the next),
    C_START (what `SlotsRx`'s CTRL START does to the chain: a loss of 0
    symbols, the carrier register loaded with the host's CARRIER, 0, the
    taps written loaded as with INIT bit 0, the blocks' sequence from 0),
    C_TAPARM and C_AGC (the host's TAPS_FROM and AGC writes, `host_records`).
    `errors`: [(frame, slot, [positions])] bits to flip in the decoder's
    output stream (a set's `bch_errors`: the model flipped the same in its
    decoder's output), between the bridge and the chain; `tmcc_errors`:
    [(frame, [positions])] the same in a frame's TMCC word (a set's
    `tmcc_errors`). A flip is found by (frame, the tag's low byte: the slot,
    or 0x80 for the TMCC, word), each entry compared with every beat (the
    results leave in commit order: the TMCC of frame N follows the first
    slots of N + 1, and both may have flips). The chain's stop
    is on (`SlotsChain.stop_after`, `top.STOP_AFTER` symbol clocks without
    an input record), as in `SlotsRx`: a bench's pause is a host's stop."""

    def __init__(self, units=34, banks=3, errors=(), tmcc_errors=(), monitors=True):
        from amaranth.hdl import DomainRenamer

        from ..ldpc import LdpcDecoder
        from .ldpc_if import TAG_TMCC, LdpcBridge
        from .top import SlotsChain

        from .ldpc_if import LdpcPort

        from .. import ldpc

        self.ch = SlotsChain(max_iter=ldpc.DEFAULT_MAX_ITER)
        self.bport = LdpcPort("brg")
        self.br = LdpcBridge(self.bport)
        flips = {}
        for fr, sl, pos in list(errors) + [(fr, TAG_TMCC, pos) for fr, pos in tmcc_errors]:
            for p_ in pos:
                key = (fr, sl, p_ // 16)
                flips[key] = flips.get(key, 0) | 1 << (p_ % 16)
        self.flips = sorted(flips.items())
        self.dec = LdpcDecoder(units, banks=banks, sim=True)  # the CXXRTL-safe build
        self.dec_r = DomainRenamer("ldpc")(self.dec)
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.kind = Signal(4)
        self.a = Signal(signed(33))
        self.b = Signal(signed(33))
        self.c = Signal(signed(32))
        # the monitors' ports (`mon_*`) are this bench's: the decoder's
        # cycles are its own (`ldpc`, 5 per symbol clock)
        monitored(self, monitors, rename={"dec": {"sync": "ldpc"}})

    def elaborate(self, platform):
        m = Module()
        m.submodules.ch = ch = self.ch
        m.submodules.br = br = self.br
        m.submodules.dec = self.dec_r
        from .ldpc_if import connect_decoder

        connect_decoder(m, br, self.dec)
        # the chain's port <-> the bridge's, with the bit errors in the results
        cp, bp = ch.port, self.bport
        m.d.comb += [getattr(bp, n).eq(getattr(cp, n)) for n in (
            "in_valid", "in_op", "in_ctx", "in_n", "in_rate", "in_max_iter", "in_offset", "in_shortened",
            "in_first", "in_last", "in_tag", "out_ready")]
        m.d.comb += [bp.in_pos[k].eq(cp.in_pos[k]) for k in range(len(cp.in_pos))]
        m.d.comb += [bp.in_llr[k].eq(cp.in_llr[k]) for k in range(len(cp.in_llr))]
        m.d.comb += [getattr(cp, n).eq(getattr(bp, n)) for n in (
            "in_ready", "out_valid", "out_head", "out_tag", "out_iterations", "out_converged", "out_last")]
        mask = Signal(16)
        m.d.comb += cp.out_bits.eq(bp.out_bits ^ mask)
        if self.flips:
            from amaranth.hdl import Const

            cur = Signal(52)  # frame, slot (or the TMCC flag), word of the beat in hand
            take = bp.out_valid & cp.out_ready
            with m.If(take & bp.out_head):
                m.d.sync += cur.eq(Cat(Const(0, 12), bp.out_tag[0:8], bp.out_tag[32:64]))
            with m.If(take & ~bp.out_head):
                m.d.sync += cur.eq(cur + 1)
                # the keys are distinct: at most one entry matches
                hit = Const(0, 16)
                for (fr, sl, w), mk in self.flips:
                    hit = hit | Mux(cur == (fr << 20 | sl << 12 | w), mk, 0)
                m.d.comb += mask.eq(hit)
        eq = ch.back.eq
        k = self.kind
        go = self.i_valid & self.i_ready
        to_chain = (k == C_SYM) | (k == C_LOSS) | (k == C_START)
        start = go & (k == C_START)
        m.d.comb += [
            self.i_ready.eq(Mux(to_chain, ch.i_ready, 1)),
            ch.i_valid.eq(self.i_valid & to_chain),
            ch.i_loss.eq((k == C_LOSS) | (k == C_START)), ch.i_lost.eq(Mux(k == C_START, 0, self.a)),
            ch.i_x[0].eq(self.a), ch.i_x[1].eq(self.b),
            eq.tap_we.eq(go & (k == C_TAP)), eq.tap_idx.eq(self.a), eq.tap_val[0].eq(self.b),
            eq.tap_val[1].eq(self.c), eq.tap_now.eq(go & ((k == C_TAPNOW) | (k == C_START))),
            eq.tap_arm.eq(go & (k == C_TAPARM)), eq.tap_at.eq(self.a + EQ_APPLY_FRAMES),
            eq.tap_from_in.eq(self.a),
            ch.front.nco.f_load.eq(start), ch.front.nco.f_in.eq(0), ch.finish.packer.start.eq(start),
            ch.r_overrun.eq(br.out_overrun),
        ]
        with m.If(go & (k == C_AGC)):
            m.d.sync += ch.agc.eq(self.a)
        return m

    def ports(self):
        ch = self.ch
        return {"i_valid": self.i_valid, "i_ready": self.i_ready, "kind": self.kind, "a": self.a,
                "b": self.b, "c": self.c,
                "ov": ch.o_valid, "ow": ch.o_word, "tdone": ch.tmcc_done,
                "frames_done": ch.finish.counters["frames"], "port_lost": ch.back.port_lost,
                "late": ch.back.demap.late, "overrun": ch.back.tmcc.overrun,
                "in_lost": self.br.in_lost, "dec_overruns": self.dec.overruns,
                "fifo_max": ch.results.max_level,
                "queue_overflow": self.dec.queue_overflow,
                # the decoder's banks and the bridge's queues (issue #28)
                "held": self.dec.held, "load_ready": self.dec.load_ready, "aborted": self.dec.aborted,
                "reused": self.dec.reused, "aborts": ch.back.aborts, "stops": ch.stops,
                "in_level": self.br.v_in_level,
                "tag_level": self.br.v_tags, "out_level": self.br.v_out_level, "rf_level": ch.results.level,
                **self.mon.ports()}


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


def run_chain_real(doc, taps, backend=None, symbols=None, words=None):
    """The whole chain with the real decoder on a whole-chain set written
    with the decoder's settings (chain-7-hdl, chain-8-loss, chain-9-fade,
    chain-10-bch): every block of the model's stream, compared per (frame,
    kind, slot). `backend` None: the compiled one (`simx.compiled_backend`:
    `$FPGA_SIM` if it names one, else Verilator where installed, else
    CXXRTL); the decoder makes pysim impractical here. `words`: a list
    that receives the chain's `Slots` words (a variant's own checks)."""
    from . import simx

    backend = backend or simx.compiled_backend()
    if backend is None:
        raise RuntimeError("the real-decoder chain needs a compiled backend (simx.compiled_backend)")
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
    host = host_records(doc)
    for i in range(n):
        if i in host:
            recs += host[i]
            gaps += [0] * len(host[i])
        if i in losses:
            recs.append((C_LOSS, losses[i], 0, 0))
            gaps.append(0)
        recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0))
        acc += 40e6 / 33.7561e6 - 1
        gaps.append(int(acc))
        acc -= int(acc)
    b = RealChainBench(errors=doc.get("bch_errors", ()), tmcc_errors=doc.get("tmcc_errors", ()))
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
    if words is not None:
        words.extend(got)
    import json

    with open(os.path.join(os.path.dirname(__file__), "..", "..", "build", f"chain-real-got-{n}.json"), "w") as f:
        json.dump(got, f)
    diag = {k: e.get(k) for k in ("tdone", "port_lost", "late", "overrun", "frames_done", "in_lost",
                                  "dec_overruns", "queue_overflow", "fifo_max")}
    cyc = e.cycles()
    dt = time.time() - t0
    bad = ([stuck] if stuck else []) + b.mon.report(e.get)
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


def run_chain_restarts(doc, taps, backend=None, cycles=4, pause=200_000):
    """Issue #28's recipe on the chain with the real decoder (chain-7-hdl):
    the input from its start to inside the first codeword of the first
    decoded frame (frame 2), then the host's stop (no input for `pause`
    symbol clocks: what was committed drains) and its START (C_START),
    `cycles` times; then the whole input once more. At the end of each
    stop, before START: the chain's stop restarted it (`stops`, an abort
    beat), no bank held (the open codeword's freed by that abort), a bank
    free (`load_ready`), the bridge's FIFOs and tag queue and the result
    FIFO empty; and the same after START. The last pass: the slot and TMCC blocks of
    its three frames against the model's (below: the decoded bits and
    BCH results; not the frame counter, which runs on across a restart,
    nor the iteration counts), so frame 2 decodes every slot as without a
    stop. Before the abort each stop kept a decoder bank (one of three):
    the last pass had none; before the stop's restart each stop kept the
    open codeword's bank until START (build 9 on the board: "1 of 3 banks
    held" at every open). Returns (what, bad)."""
    from . import simx
    from . import slots as S

    backend = backend or simx.compiled_backend()
    if backend is None:
        raise RuntimeError("the real-decoder chain needs a compiled backend (simx.compiled_backend)")
    x = doc["input"]
    n = len(x) // 2
    decoded = [f for f in doc["frames"] if f["decoded"]]
    # inside the frame's first codeword in the demapper (it starts at the
    # frame's symbol 56; stage 3 releases a slot once the next slot's head
    # is in: a slot and more behind the input)
    stop = decoded[0]["at"] + SLOT + 6000

    def symbols(m):
        recs, gaps, acc = [], [], 0.0
        for i in range(m):
            recs.append((C_SYM, x[2 * i], x[2 * i + 1], 0))
            acc += 40e6 / 33.7561e6 - 1
            gaps.append(int(acc))
            acc -= int(acc)
        return recs, gaps

    b = RealChainBench(errors=doc.get("bch_errors", ()), tmcc_errors=doc.get("tmcc_errors", ()))
    t0 = time.time()
    e = engine(b, b.ports(), backend, tag="chainreal")
    e.stall_limit = 3_000_000
    g = e.group("ov", ["ow"])
    bad, starts, stops = [], [], []
    violated = False
    keys = ("held", "load_ready", "in_level", "tag_level", "out_level", "rf_level", "aborted", "aborts",
            "reused", "dec_overruns", "stops")

    def drained(st):
        return (st["held"], st["load_ready"], st["in_level"], st["tag_level"], st["out_level"],
                st["rf_level"]) == (0, 1, 0, 0, 0, 0)
    stop_recs, stop_gaps = symbols(stop)
    for r in range(cycles + 1):
        last = r == cycles
        recs, gaps = (symbols(n) if last else (list(stop_recs), list(stop_gaps)))
        if r == 0 and taps is not None:
            head = [(C_TAP, i, taps[2 * i], taps[2 * i + 1]) for i in range(11)] + [(C_TAPNOW, 0, 0, 0)]
            recs, gaps = head + recs, [0] * len(head) + gaps
        try:
            # the stop: `pause` idle clocks after the last symbol, then START
            before = e.get("stops")
            e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], recs, tail=1_500_000 if last else pause,
                  gaps=gaps)
            if not last:
                st = {k: e.get(k) for k in keys}
                stops.append(st)
                if not drained(st) or st["stops"] != before + 1:
                    bad.append(f"after stop {r + 1}, before START: {st}")
                e.run("i_valid", "i_ready", ["kind", "a", "b", "c"], [(C_START, 0, 0, 0)], tail=3000)
        except RuntimeError as err:
            return "", bad + [f"pass {r}: {err}"] + b.mon.report(e.get)
        words = [w for (w,) in e.read(g)]
        viol = b.mon.report(e.get)  # sticky: reported after the pass that first violates
        if viol and not violated:
            violated = True
            bad += [f"pass {r}: {x}" for x in viol]
        if not last:
            st = {k: e.get(k) for k in keys}
            starts.append(st)
            if not drained(st):
                bad.append(f"after START {r + 1}: {st}")
    dt = time.time() - t0
    # the last pass against the model: its frames in order are the model's 0, 1, 2
    try:
        got = S.blocks(words)
    except AssertionError as err:
        return "", bad + [f"the last pass does not parse as Slots blocks: {err}"]
    exp = S.blocks(doc["slots_words"])
    frames = sorted({f for k, _, f, _ in got if k == S.TMCC})
    mframes = sorted({f for k, _, f, _ in exp if k == S.TMCC})
    if len(frames) != len(mframes):
        return "", bad + [f"the last pass has TMCC blocks of frames {frames}, the model of {mframes}"]
    to_model = dict(zip(frames, mframes))
    gk = {(to_model[f], k, p[0] & 0x7F if k == S.SLOT else 0): p for k, _, f, p in got if f in to_model}
    ek = {(f, k, p[0] & 0x7F if k == S.SLOT else 0): p for k, _, f, p in exp}
    # A restart keeps the NCO's phase (the model's `Chain::loss` does), so
    # the last pass's symbols are rotated by another phase than the model's
    # run from reset: the same decisions, LLRs that may differ by rounding,
    # so a slot's iteration count may too (7 of 96 here; with the phase
    # zeroed at START, a scratch experiment, none). Compared: every block's kind, every slot's mode, modulation,
    # rate, convergence, BCH result and decoded bits, and the TMCC's bits
    # and BCH result; the iteration counts that differ are reported.
    dropped = compared = 0
    iters = []
    for key in sorted(ek):
        f, k, _s = key
        if k == S.STATUS:
            continue  # its counters run on across the restarts
        if key not in gk:
            bad.append(f"block {key} missing in the last pass")
            continue
        if k == S.SUMS:
            # present (above); the sums of the replay's symbols, which the
            # kept NCO phase rounds otherwise (as the iteration counts)
            continue
        gp, ep = list(gk[key]), list(ek[key])
        compared += 1
        if k == S.SLOT:
            gf_, ef_ = S.slot_fields(gp), S.slot_fields(ep)
            dropped += int(not gf_["dummy"] and not gf_["converged"] and gf_["iterations"] == 0)
            diff = [n_ for n_ in ef_ if n_ != "iterations" and gf_[n_] != ef_[n_]]
            if gf_["iterations"] != ef_["iterations"]:
                iters.append((key[2], gf_["iterations"], ef_["iterations"]))
            if diff:
                bad.append(f"block {key}: {diff} differ in the last pass")
        else:
            if gp[0] != ep[0] or gp[4:] != ep[4:]:  # BCH, the bits (1: iterations, 2-3: the frame)
                bad.append(f"block {key}: the TMCC's BCH or bits differ in the last pass")
        if len(bad) > 12:
            break
    if dropped:
        bad.append(f"{dropped} dropped slots in the last pass")
    what = (f"{cycles} stop/START cycles (stop at symbol {stop}, {pause} clocks), then the whole input: "
            f"{compared} slot and TMCC blocks of the model's compared, {dropped} dropped, iterations "
            f"(slot, got, model's) differing in {iters}; after each stop {stops}; after each START {starts}; "
            f"{e.cycles()} cycles in {dt:.0f} s")
    return what, bad
