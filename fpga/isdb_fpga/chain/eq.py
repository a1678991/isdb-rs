"""Stage 4 of the ISDB-S3 chain: the 11-tap equaliser FIR, its taps and its
training sums.

Contract: `isdbs3_phy::chain::eq::Equaliser`, `frame::Back::push` and the
module docs of `isdbs3_phy::chain`, "Stage 4":

- `z_n = sat16(round_shift(sum_k w_k y_{n+5-k}, 14))`, complex, taps Q2.14,
  output aligned with the input (delay 5): the output for index n when
  y_{n+5} enters; the line holds zeros until filled (outputs from the 6th
  input on); identity taps (w_5 = 16 384) until taps are written;
- taps written by the host are in use either at once (`tap_now`, the
  model's `initial_taps`) or from the first output whose frame >= the armed
  frame (`tap_arm`, `tap_at`: the model's EQ_APPLY_FRAMES rule, the
  host's slow loop), with `tap_from` reported as the frame they were
  solved from;
- training at the FIR's centre over the frame's 24 sync and 32 pilot
  symbols of each slot while a layout is in force and the line holds 11
  inputs: `R[r][c] += conj(x_r) x_c` (66 entries r <= c, row by row),
  `p[r] += conj(x_r) d` with x_k = y_{n+5-k}; references d: sync symbol t
  `+-w_t * 724` (- on !FSync), pilot t the nominal point of label t mod
  2^bits of the slot's mode times the pilot PRBS sign of (slot, t); 48-bit
  sums, latched at the end of a frame with a layout for the host (the
  slow loop: read, solve, write taps).

Implementation:

- **FIR**: a line of 11 input registers; the 44 real products (16 x 16
  bits) on 11 DSP48E1 in the 4x clock (`fast`): in the four fast cycles of
  the centre cycle C (C = the cycle after the input's shift) phase p
  computes kind p of every tap (w_r x_r, w_i x_i, w_r x_i, w_i x_r), one
  11-leaf adder tree (4 registered levels) sums each kind, re = S0 - S1
  and im = S2 + S3 settle in phase 0 of C+2; the symbol clock takes them
  then and delays them, rounding and saturation to the 44-DSP design's
  schedule: `o_valid`, `o_z` and the centre's meta at C+6. The taps for the
  centre symbol are chosen at C (the pending set replaces the in-use set
  at the first centre of the armed frame) and the products of C use them.
- **Training** (time-multiplexed, the plan's slow loop): at the centre of
  position 0 of a slot the line (y_{n0+5} .. y_{n0-5}) is copied into a
  66-entry buffer, then the newest input at each centre of positions 1 ..
  55, with each training symbol's enable and reference (`d`, a cycle
  later: the nominal-point ROM is a RAM). After position 55 a MAC unit
  (4 real products, one complex MAC per clock) runs 56 x 77 = 4 312 MACs
  over the 77 accumulators (66 R + 11 p, 96 bits, a read-modify-write
  pipeline of 3 cycles in a 154-entry RAM: two banks), done long before
  the next slot's position 0 (9 240 symbols later). A frame with a layout
  clears its bank (77 cycles at position 0 of slot 0, before its first
  MAC) and latches it at its end (`frame_close`): the host reads the
  latched bank (`rd_*`, 4 words per entry: re low 32, re high 16 sign-
  extended, im low, im high; the read waits for the MAC unit to be idle).

Ports: `i_valid`, `i_y`, `i_meta` (the stage-3 record's at, j, number,
numbered, frame, word); `restart`; outputs at C: `cv` and the centre's
`c_*` meta (for `FrameCtl`), `t_en` (FrameCtl: train this centre symbol:
active, a layout in force, sync or pilot), `t_pilot`, `t_t`, `t_mod`,
`t_rate`, `frame_open`, `frame_close`; outputs at C+6: `o_valid`, `o_z`,
`o_*` meta.
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal, signed
from amaranth.lib.memory import Memory

from .common import FSYNC, NO_DSP, Phase4, PILOTS, SLOTS, SSYNC, SYNC, SYNC_REF, TAP_FRAC, TAPS, W_NFSYNC, W_SSYNC
from .common import round_shift, sat16, word_symbols
from .nominal import NominalRom
from .tables import MOD_BITS, pilot_sign_rom

CENTRE = TAPS // 2
R_ENTRIES = TAPS * (TAPS + 1) // 2
ENTRIES = R_ENTRIES + TAPS  # 77
TRAIN = SYNC + PILOTS  # 56
BUF = TRAIN + TAPS - 1  # 66
META = [("j", 14), ("number", 7), ("numbered", 1), ("frame", 32), ("word", 2), ("at", 48)]
META_SMALL = 56  # the fields before `at`
META_BITS = sum(w for _, w in META)


def entries():
    """(r, c) of the 77 accumulators: R row by row (r <= c), then p[r] as (r, None)."""
    return [(r, c) for r in range(TAPS) for c in range(r, TAPS)] + [(r, None) for r in range(TAPS)]


class Equaliser(Elaboratable):
    def __init__(self, aux=0, nominal=None):
        """`aux`: bits of a record the caller attaches to the centre symbol
        (`c_aux`, at the centre cycle) and gets back with its output
        (`o_aux`): the back end's routing of the symbol. `nominal`: a
        `NominalPort` of a shared `NominalRom` (back.py), or None for one of
        its own."""
        self.aux = aux
        self.nominal = nominal
        self.c_aux = Signal(max(aux, 1))
        self.i_valid = Signal()
        self.i_y = [Signal(signed(16), name=f"i_y{k}") for k in range(2)]
        self.i_meta = Signal(META_BITS)
        self.restart = Signal()
        # centre cycle
        self.cv = Signal()
        self.c_meta = Signal(META_BITS)
        self.c_full = Signal()
        self.t_en = Signal()
        self.t_pilot = Signal()
        self.t_t = Signal(range(32))
        self.t_mod = Signal(4)
        self.t_rate = Signal(4)
        self.frame_open = Signal()  # in: a frame with a layout starts (at its centre cycle)
        self.frame_close = Signal()  # in: it ended whole
        self.close_frame = Signal(32)
        # taps
        self.tap_we = Signal()
        self.tap_idx = Signal(range(TAPS))
        self.tap_val = [Signal(signed(16), name=f"tap_val{k}") for k in range(2)]
        self.tap_now = Signal()
        self.tap_arm = Signal()
        self.tap_at = Signal(32)
        self.tap_from_in = Signal(32)
        self.taps = [[Signal(signed(16), name=f"w{k}_{c}", init=(1 << TAP_FRAC if k == CENTRE and c == 0 else 0))
                      for c in range(2)] for k in range(TAPS)]
        self.taps_from = Signal(32, init=0xFFFFFFFF)
        # sums readback
        self.sums_frame = Signal(32, init=0xFFFFFFFF)
        self.sums_count = Signal(16)
        self.rd_req = Signal()
        self.rd_addr = Signal(range(4 * ENTRIES))
        self.rd_data = Signal(32)
        self.rd_ack = Signal()
        self.rd_busy = Signal()
        # training internals (verification)
        self.d_valid = Signal()
        self.d_at = Signal(48)
        self.d_ref = [Signal(signed(16), name=f"d_ref{k}") for k in range(2)]
        # output
        self.o_valid = Signal()
        self.o_z = [Signal(signed(16), name=f"o_z{k}") for k in range(2)]
        self.o_meta = Signal(META_BITS)
        self.o_aux = Signal(max(aux, 1))

    def meta_field(self, sig, name):
        o = 0
        for n, w in META:
            if n == name:
                return sig[o:o + w]
            o += w
        raise KeyError(name)

    def elaborate(self, platform):
        m = Module()
        mf = self.meta_field

        # ---- line ----------------------------------------------------------
        line = [[Signal(signed(16), name=f"line{k}_{c}") for c in range(2)] for k in range(TAPS)]
        mline = [Signal(META_BITS, name=f"mline{k}") for k in range(CENTRE + 1)]
        filled = Signal(range(TAPS + 1))
        cv = self.cv
        m.d.sync += [cv.eq(0), self.c_full.eq(0)]
        with m.If(self.restart):
            m.d.sync += [filled.eq(0)]
            m.d.sync += [line[k][c].eq(0) for k in range(TAPS) for c in range(2)]
        with m.Elif(self.i_valid):
            m.d.sync += [line[0][c].eq(self.i_y[c]) for c in range(2)]
            m.d.sync += [line[k][c].eq(line[k - 1][c]) for k in range(1, TAPS) for c in range(2)]
            m.d.sync += mline[0].eq(self.i_meta)
            m.d.sync += [mline[k].eq(mline[k - 1]) for k in range(1, CENTRE + 1)]
            m.d.sync += [filled.eq(Mux(filled == TAPS, TAPS, filled + 1)),
                         cv.eq(filled >= CENTRE), self.c_full.eq(filled >= TAPS - 1)]
        m.d.comb += self.c_meta.eq(mline[CENTRE])
        cframe = mf(self.c_meta, "frame")

        # ---- taps ----------------------------------------------------------
        pend = [[Signal(signed(16), name=f"pend{k}_{c}") for c in range(2)] for k in range(TAPS)]
        pend_valid = Signal()
        pend_at = Signal(32)
        pend_from = Signal(32)
        with m.If(self.tap_we):
            for k in range(TAPS):
                with m.If(self.tap_idx == k):
                    m.d.sync += [pend[k][c].eq(self.tap_val[c]) for c in range(2)]
        with m.If(self.tap_now):
            m.d.sync += [self.taps[k][c].eq(pend[k][c]) for k in range(TAPS) for c in range(2)]
            m.d.sync += [self.taps_from.eq(0xFFFFFFFF), pend_valid.eq(0)]
        with m.If(self.tap_arm):
            m.d.sync += [pend_valid.eq(1), pend_at.eq(self.tap_at), pend_from.eq(self.tap_from_in)]
        switch = Signal()
        m.d.comb += switch.eq(cv & pend_valid & mf(self.c_meta, "numbered") & (cframe >= pend_at))
        with m.If(switch):
            m.d.sync += [self.taps[k][c].eq(pend[k][c]) for k in range(TAPS) for c in range(2)]
            m.d.sync += [self.taps_from.eq(pend_from), pend_valid.eq(0)]
        w = [[Mux(switch, pend[k][c], self.taps[k][c]) for c in range(2)] for k in range(TAPS)]

        # ---- FIR (4x clock) ------------------------------------------------------
        # In the centre cycle C the line and the chosen taps are stable; the
        # fast domain's phase p computes product kind p of all 11 taps on 11
        # multipliers (0: w_r x_r, 1: w_i x_i, 2: w_r x_i, 3: w_i x_r), an
        # 11-leaf tree (4 levels) sums each kind, re = S0 - S1 and im = S2 + S3
        # land in fast registers by phase 0 of C+2, and the symbol clock takes
        # them at C+2 and delays them to the 44-product design's schedule
        # (o_* at C+7). Exact integer sums in any order: the same z.
        # The crossing: the chosen taps and the line through fast registers
        # (`wq`, `lq`, every fast cycle: C's values in its fast cycles 1-3 and
        # C+1's 0), then the products' operands registered (`a_r`, `b_r`,
        # kind = the phase one fast cycle late: kinds 0-3 at the ends of
        # those four cycles), the products a cycle later: two fast cycles
        # after the single-stage schedule, re / im in phase 2 of C+2, still
        # inside C+2.
        m.submodules.ph = ph4 = Phase4()
        kd = ph4.late
        fast = m.d.fast
        kind_i = kd[0]  # w_i (kinds 1, 3)
        kind_x = kd[0] ^ kd[1]  # x_i (kinds 1, 2)
        lv = []
        a_k = Signal(2)
        fast += a_k.eq(kd)
        for k in range(TAPS):
            wq = [Signal(signed(16), name=f"wq{k}_{c}") for c in range(2)]
            lq = [Signal(signed(16), name=f"lq{k}_{c}") for c in range(2)]
            fast += [wq[c].eq(w[k][c]) for c in range(2)]
            fast += [lq[c].eq(line[k][c]) for c in range(2)]
            a_r = Signal(signed(16), name=f"fa{k}")
            b_r = Signal(signed(16), name=f"fb{k}")
            fast += [a_r.eq(Mux(kind_i, wq[1], wq[0])), b_r.eq(Mux(kind_x, lq[1], lq[0]))]
            s = Signal(signed(32), name=f"fp{k}")
            fast += s.eq(a_r * b_r)
            lv.append(s)
        fk = Signal(2)
        fast += fk.eq(a_k)
        kinds = [fk]
        cur = lv
        lvl = 0
        while len(cur) > 1:
            nxt = []
            for i in range(0, len(cur), 2):
                if i + 1 == len(cur):
                    r = Signal(signed(len(cur[i])), name=f"ft_l{lvl}_{i // 2}")
                    fast += r.eq(cur[i])
                else:
                    r = Signal(signed(max(len(cur[i]), len(cur[i + 1])) + 1), name=f"ft_l{lvl}_{i // 2}",
                               attrs=NO_DSP)
                    fast += r.eq(cur[i] + cur[i + 1])
                nxt.append(r)
            kk_ = Signal(2, name=f"ft_kind{lvl}")
            fast += kk_.eq(kinds[-1])
            kinds.append(kk_)
            cur = nxt
            lvl += 1
        ssum, skind = cur[0], kinds[-1]
        held = [Signal(signed(len(ssum)), name=f"fs{p}") for p in range(3)]
        for p in range(3):
            with m.If(skind == p):
                fast += held[p].eq(ssum)
        f_re = Signal(signed(len(ssum) + 1))
        f_im = Signal(signed(len(ssum) + 1))
        with m.If(skind == 3):
            fast += [f_re.eq(held[0] - held[1]), f_im.eq(held[2] + ssum)]
        # the symbol clock: f_re / f_im settle in phase 0 of C+2, taken at its
        # end, then delayed to C+6
        zd = [(f_re, f_im)]
        for d in range(3, 7):
            r = Signal(signed(len(f_re)), name=f"zre{d}")
            i_ = Signal(signed(len(f_im)), name=f"zim{d}")
            m.d.sync += [r.eq(zd[-1][0]), i_.eq(zd[-1][1])]
            zd.append((r, i_))
        sre, sim_ = zd[-1]
        depth = 5
        vpipe = Signal(depth + 1)
        mpipe = [Signal(META_BITS + self.aux, name=f"mpipe{k}") for k in range(depth + 1)]
        m.d.sync += vpipe.eq(Cat(cv, vpipe[:-1]))
        m.d.sync += mpipe[0].eq(Cat(self.c_meta, self.c_aux))
        m.d.sync += [mpipe[k].eq(mpipe[k - 1]) for k in range(1, depth + 1)]
        m.d.sync += [
            self.o_valid.eq(vpipe[-1]),
            self.o_meta.eq(mpipe[-1][:META_BITS]),
            self.o_aux.eq(mpipe[-1][META_BITS:]),
            self.o_z[0].eq(sat16(round_shift(sre, TAP_FRAC))),
            self.o_z[1].eq(sat16(round_shift(sim_, TAP_FRAC))),
        ]
        with m.If(self.restart):
            m.d.sync += [vpipe.eq(0), self.o_valid.eq(0)]

        # ---- training capture -------------------------------------------------
        buf = Memory(shape=32, depth=BUF, init=[], attrs={"ram_style": "distributed"})
        m.submodules.buf = buf
        bw = buf.write_port()
        cj = mf(self.c_meta, "j")
        ck = mf(self.c_meta, "number")
        tsel = Signal(range(TAPS + 1))  # copying the line at position 0
        copying = Signal()
        m.d.comb += [bw.en.eq(0)]
        with m.If(cv & (cj == 0)):
            # entries 10 .. 0 = line[0 .. 10]; line[0] now, the rest in the next cycles
            m.d.comb += [bw.addr.eq(TAPS - 1), bw.data.eq(Cat(line[0][0], line[0][1])), bw.en.eq(1)]
            m.d.sync += [tsel.eq(1), copying.eq(1)]
        with m.Elif(cv & (cj < TRAIN)):
            m.d.comb += [bw.addr.eq(TAPS - 1 + cj), bw.data.eq(Cat(line[0][0], line[0][1])), bw.en.eq(1)]
        # the copy of line[1 .. 10] runs in the 10 cycles after position 0;
        # the line may shift meanwhile (one input per cycle at most), so the
        # snapshot is taken at position 0
        snap = [Signal(32, name=f"snap{k}") for k in range(TAPS)]
        with m.If(cv & (cj == 0)):
            m.d.sync += [snap[k].eq(Cat(line[k][0], line[k][1])) for k in range(TAPS)]
        with m.If(copying & ~(cv & (cj < TRAIN) & (cj != 0))):
            m.d.comb += [bw.addr.eq(TAPS - 1 - tsel), bw.data.eq(Array(snap)[tsel]), bw.en.eq(1)]
            m.d.sync += tsel.eq(tsel + 1)
            with m.If(tsel == TAPS - 1):
                m.d.sync += copying.eq(0)

        # references, a cycle after the centre (the ROMs are RAMs)
        fs = word_symbols(FSYNC)
        ss = word_symbols(SSYNC)
        if self.nominal is None:
            m.submodules.nominal = nrom = NominalRom(1)
            nrp = nrom.ports[0]
        else:
            nrp = self.nominal
        # the pilot signs: a LUT ROM, registered (a cycle, as the nominal port)
        srom = Memory(shape=1, depth=4096, init=pilot_sign_rom())
        m.submodules.signs = srom
        srp = srom.read_port(domain="comb")
        tt = self.t_t
        bits = Array(Const(MOD_BITS.get(i, 1), 3) for i in range(8))[self.t_mod[:3]]
        label = tt & ((Const(1, 6) << bits) - 1)[:5]
        m.d.comb += [
            nrp.mod.eq(self.t_mod), nrp.rate.eq(self.t_rate), nrp.label.eq(label),
            srp.addr.eq(ck * PILOTS + tt),
        ]
        en_d = Signal()
        pil_d = Signal()
        idx_d = Signal(range(TRAIN))
        sref = [Signal(signed(16), name=f"sref{c}") for c in range(2)]
        at_d = Signal(48)
        word = mf(self.c_meta, "word")
        fsr = Array(Const(fs[k][0], signed(2)) for k in range(SYNC))
        fsi = Array(Const(fs[k][1], signed(2)) for k in range(SYNC))
        ssr = Array(Const(ss[k][0], signed(2)) for k in range(SYNC))
        ssi = Array(Const(ss[k][1], signed(2)) for k in range(SYNC))
        tsync = tt[:5]
        wr_ = Mux(word == W_SSYNC, ssr[tsync], fsr[tsync])
        wi_ = Mux(word == W_SSYNC, ssi[tsync], fsi[tsync])
        neg = word == W_NFSYNC
        m.d.sync += [
            en_d.eq(cv & (cj < TRAIN)),
            pil_d.eq(self.t_pilot),
            idx_d.eq(Mux(cj < TRAIN, cj, 0)),
            at_d.eq(mf(self.c_meta, "at")),
            # +-w * 724 (w = +-1): a choice of two constants
            sref[0].eq(Mux((wr_ < 0) ^ neg, -SYNC_REF, SYNC_REF)),
            sref[1].eq(Mux((wi_ < 0) ^ neg, -SYNC_REF, SYNC_REF)),
        ]
        neg_p = Signal()
        m.d.sync += neg_p.eq(srp.data[0])
        ten = Signal(TRAIN)
        ten_d = Signal()
        m.d.sync += ten_d.eq(self.t_en & self.c_full)
        nre = nrp.re
        nim = nrp.im
        d_re = Mux(pil_d, Mux(neg_p, -nre, nre), sref[0])
        d_im = Mux(pil_d, Mux(neg_p, -nim, nim), sref[1])
        dbuf = Memory(shape=32, depth=TRAIN, init=[])
        m.submodules.dbuf = dbuf
        dw = dbuf.write_port()
        m.d.comb += [dw.addr.eq(idx_d), dw.data.eq(Cat(d_re[:16], d_im[:16])), dw.en.eq(en_d)]
        with m.If(en_d):
            for i in range(TRAIN):
                with m.If(idx_d == i):
                    m.d.sync += ten[i].eq(ten_d)
        m.d.sync += [self.d_valid.eq(en_d & ten_d), self.d_at.eq(at_d),
                     self.d_ref[0].eq(d_re), self.d_ref[1].eq(d_im)]
        go_pend = Signal()
        with m.If(en_d & (idx_d == TRAIN - 1)):
            m.d.sync += go_pend.eq(1)

        # ---- MAC unit -----------------------------------------------------------
        acc = Memory(shape=96, depth=2 * 128, init=[])
        m.submodules.acc = acc
        arp = acc.read_port()
        awp = acc.write_port()
        brp1 = buf.read_port()
        brp2 = buf.read_port()
        drp = dbuf.read_port()
        ent = entries()
        er = Memory(shape=9, depth=128, init=[r | ((c if c is not None else 0) << 4) | ((c is None) << 8)
                                               for r, c in ent])
        m.submodules.ent = er
        erp = er.read_port(domain="comb")
        work = Signal()  # the bank being summed
        latched = Signal()
        opened = Signal()  # a sum is open (a frame with a layout)
        count = Signal(16)
        busy = Signal()
        clearing = Signal()
        ci = Signal(range(TRAIN))
        ce = Signal(range(128))
        # stage A: entry decode, buffer reads
        e_r = erp.data[:4]
        e_c = erp.data[4:8]
        e_p = erp.data[8]
        m.d.comb += [
            erp.addr.eq(ce),
            brp1.addr.eq(ci + TAPS - 1 - e_r),
            brp2.addr.eq(ci + TAPS - 1 - e_c),
            drp.addr.eq(ci),
            arp.addr.eq(Cat(ce[:7], work)),
        ]
        va = Signal()
        pa = Signal()
        ea = Signal(7)
        m.d.sync += [va.eq(0)]
        # stage B: products (data of the buffers and the accumulator ready)
        xr = brp1.data
        xc = Mux(pa, drp.data, brp2.data)
        ar_, ai_ = xr[:16].as_signed(), xr[16:].as_signed()
        br_, bi_ = xc[:16].as_signed(), xc[16:].as_signed()
        q = [Signal(signed(32), name=f"mq{k}") for k in range(4)]
        vb = Signal()
        eb = Signal(7)
        accb = Signal(96)
        m.d.sync += [q[0].eq(ar_ * br_), q[1].eq(ai_ * bi_), q[2].eq(ar_ * bi_), q[3].eq(ai_ * br_),
                     vb.eq(va), eb.eq(ea), accb.eq(arp.data)]
        # stage C: add, write
        nre_ = accb[:48].as_signed() + (q[0] + q[1])
        nim_ = accb[48:].as_signed() + (q[2] - q[3])
        m.d.comb += [awp.addr.eq(Cat(Mux(clearing, ce, eb)[:7], work)),
                     awp.data.eq(Mux(clearing, 0, Cat(nre_[:48], nim_[:48]))),
                     awp.en.eq(vb | clearing)]
        m.d.comb += [arp.en.eq(1), brp1.en.eq(1), brp2.en.eq(1), drp.en.eq(1)]

        rd_pend = self.rd_busy
        rd_a = Signal(range(4 * ENTRIES))
        with m.If(self.rd_req):
            m.d.sync += rd_a.eq(self.rd_addr)
        with m.If(self.rd_req):
            m.d.sync += rd_pend.eq(1)
        open_pend = Signal()
        with m.If(self.frame_open):
            m.d.sync += open_pend.eq(1)
        m.d.sync += self.rd_ack.eq(0)
        rd_stage = Signal(2)
        with m.FSM(name="mac"):
            with m.State("IDLE"):
                with m.If(open_pend):
                    m.d.sync += [clearing.eq(1), ce.eq(0), work.eq(~latched), opened.eq(1), count.eq(0),
                                 open_pend.eq(0)]
                    m.next = "CLEAR"
                with m.Elif(go_pend):
                    m.d.sync += [ci.eq(0), ce.eq(0), go_pend.eq(0)]
                    m.next = "SKIP"
                with m.Elif(rd_pend):
                    m.d.sync += [ce.eq(rd_a >> 2)]
                    m.next = "READ"
            with m.State("CLEAR"):
                m.d.sync += ce.eq(ce + 1)
                with m.If(ce == ENTRIES - 1):
                    m.d.sync += clearing.eq(0)
                    m.next = "IDLE"
            with m.State("SKIP"):
                # the next enabled training symbol
                with m.If(Array(ten)[ci] & opened):
                    m.d.sync += [ce.eq(0), count.eq(count + 1)]
                    m.next = "MAC"
                with m.Elif(ci == TRAIN - 1):
                    m.next = "IDLE"
                with m.Else():
                    m.d.sync += ci.eq(ci + 1)
            with m.State("MAC"):
                m.d.sync += [va.eq(1), ea.eq(ce), pa.eq(e_p), ce.eq(ce + 1)]
                with m.If(ce == ENTRIES - 1):
                    with m.If(ci == TRAIN - 1):
                        m.next = "DRAIN"
                    with m.Else():
                        m.d.sync += ci.eq(ci + 1)
                        m.next = "WAIT"
            with m.State("WAIT"):
                # let the last write land before entry 0 is read again
                m.next = "WAIT2"
            with m.State("WAIT2"):
                m.next = "SKIP"
            with m.State("DRAIN"):
                m.next = "DRAIN2"
            with m.State("DRAIN2"):
                m.next = "IDLE"
            with m.State("READ"):
                m.d.comb += arp.addr.eq(Cat(ce[:7], latched))
                m.next = "READ2"
            with m.State("READ2"):
                v = arp.data
                part = rd_a[:2]
                word_ = Array([v[0:32], v[32:48].as_signed(), v[48:80], v[80:96].as_signed()])[part]
                m.d.sync += [self.rd_data.eq(word_), self.rd_ack.eq(1), rd_pend.eq(0)]
                m.next = "IDLE"
        # frame end: latch the open sum (the last slot's MACs ended 9 000
        # symbols earlier)
        with m.If(self.frame_close & opened):
            m.d.sync += [latched.eq(work), opened.eq(0), self.sums_frame.eq(self.close_frame),
                         self.sums_count.eq(count)]
        with m.If(self.restart):
            m.d.sync += [opened.eq(0), go_pend.eq(0), open_pend.eq(0)]
        return m
