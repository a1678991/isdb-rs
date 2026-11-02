"""Stage 1 of the ISDB-S3 chain: slot and frame synchronisation, with the
NCO and the carrier loop of stage 2 wired in front of it (`SyncFront`).

Contract: `isdbs3_phy::chain::sync::SlotSync` and the module docs of
`isdbs3_phy::chain`, "Stage 1"; every rule below is the model's:

- per position n the correlations of r_n .. r_{n+23} with FSync and SSync
  (`C = sum r_k conj(w_k)`, w_k = (+-1 +- j)). With a_k = r_i + r_q and
  b_k = r_q - r_i (one adder each per symbol, shared by both words) a word
  symbol with w_i = w_q adds w_i a_k to re and w_i b_k to im, one with w_i
  = -w_q adds -w_i b_k to re and w_i a_k to im: four add/subtract trees of
  24 leaves (one adder per node, the signs folded into add or subtract),
  no multiplier;
- `m = max(M(C_F), M(C_S))`, M = max(|re|, |im|) + (min >> 1);
- `A[n mod 9 296] += (m - A) >> 3` (arithmetic shift) in a 9 296-entry
  RAM, read the cycle before it is written (positions arrive in order, so
  a read never meets the write of its own address);
- per period (9 296 positions from the (re)start): the first largest A and
  its position, the sum of A; the peak counts when peak > 0 and peak *
  9 296 >= 3 sum; lock after 4 counting periods at one position, loss
  after 8 periods without it (a restart);
- heads at the locked offset: head 0 held until head 1 decides the SSync
  parity (head 1 carries SSync when M(C_S) - M(C_F) of head 0 is less than
  head 1's), FSync-parity heads read against the previous SSync
  correlation (Re(C_F conj(C_S,prev)) >= 0: FSync, else !FSync and C
  negated), an FSync-parity head with no SSync head before it dropped;
- the sign-majority counter mu (+-8), the marks, the frame numbering and
  its flywheel (two missed marks in a row: a restart), the stray and
  missed counters;
- the frame counter: 0 at the stream's first frame head, then `+ max(1,
  round(D / 1 115 520))`, D the symbols since the previous frame head (kept
  across restarts);
- the head record: C (sign removed), its CORDIC angle and magnitude.

A restart clears the accumulator. Instead of writing 9 296 zeros, the
RAM's reads count as 0 during the first period after it: every position is
written in that period before the next period reads it, which is the same.

Schedule (one symbol per clock). The NCO (`nco.py`) has the rotated symbol
3 cycles after the accept (A); a/b at A+4, the 24-symbol a/b line at A+5,
the four trees registered per level (5 levels) at A+10, the magnitudes at
A+11 and their max at A+12 ("W", the window is ready): the RAM read is
issued; at W+1 A is updated and written, the period's peak and sum
updated, the window's internals out (`o_w_*`). A window that needs a
decision (a head: locked and the position is the offset; a period end:
position 9 295) holds the input from the accept of the symbol that
completes it (`hold`: known at the accept from the input-side position
counter and the lock state, which changes only at a period end, itself
such a window) until the decision is taken: the head's CORDIC (35 cycles;
twice for the first pair), the FSync sign product (bit-serial, 17
cycles), the frame counter's division at a frame head (48 cycles), the
carrier loop's answer (1 cycle, or 20 for the 60th step), the lock rule
(2 cycles). That is what the model does inside one `push`: the carrier
register a head changes rotates the very next symbol, and a loss restarts
before it. The hold costs about 15 + 35 + 17 cycles per slot (9 296
symbols), 0.7 % of the clocks; the PlutoSky's front end delivers
33.76 Msymbols/s at 40 MHz (16 % of the clocks idle), and a FIFO in front
of the chain (`top.py`) absorbs the bursts.

Ports of `SlotSync` (on the NCO's output, `i_accept` its accept strobe):
the window internals (`o_w_*`), the period records (`o_per_*`), the head
records (`o_head_*`, with `o_head_late` for the held head 0, which the
model emits only at head 1's time and stage 3 therefore never corrects),
`decided` (one pulse when a decision window is done: the status snapshot
point), `restart` (a lost sync, one pulse), the statistics of the last
period with the live marker counters (`st_*`, the status block's).
"""

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal, signed, unsigned
from amaranth.lib.memory import Memory

from .common import (
    FRAME, FRAME_MISS_LIMIT, FSYNC, LEAK_SHIFT, LOCK_PERIODS, LOCK_RATIO, LOSS_PERIODS,
    MAJORITY_MAX, MAJORITY_MIN, SLOT, SLOTS, SSYNC, SYNC, W_FSYNC, W_NFSYNC, W_SSYNC,
    Cordic, SeqDiv, SeqMul, mag_approx, r_bits, word_symbols,
)
from .nco import Nco

AT_BITS = 48  # symbol indices (2**48 symbols: 96 days at 33.76 Mbaud)


def ab_max(in_bits):
    """Largest |a_k|, |b_k|: (|x_i| + |x_q|) * 32 767 * sqrt(2) / 2**11 + 1."""
    return ((1 << in_bits) * 46341) // 2048 + 2


def corr_bits(in_bits):
    """Signed width of a correlation component (17 for 6-bit input, 19 for
    8: the model's "19 bits signed")."""
    return (SYNC * ab_max(in_bits)).bit_length() + 1


def acc_bits(in_bits):
    """Width of m and A: M <= 1.5 max|C| (16 bits for 6-bit input)."""
    c = SYNC * ab_max(in_bits)
    return (c + c // 2).bit_length()


class SlotSync(Elaboratable):
    def __init__(self, in_bits=6, detail=True):
        self.in_bits = in_bits
        self.rb = r_bits(in_bits)
        self.cb = corr_bits(in_bits)
        self.ab = acc_bits(in_bits)
        self.detail = detail
        # input side
        self.i_accept = Signal()
        self.hold = Signal()
        self.i_tot = Signal(AT_BITS)  # chain index of the next input symbol
        self.loss = Signal()  # restart now (an input loss; the index is i_tot)
        # the NCO's output
        self.r_valid = Signal()
        self.r = [Signal(signed(self.rb), name=f"r{k}") for k in range(2)]
        # carrier loop handshake (to the NCO)
        self.h_valid = Signal()
        self.h_ssync = Signal()
        self.h_theta = Signal(32)
        self.h_done = Signal()
        self.restart = Signal()
        self.decided = Signal()
        # window internals
        self.o_w_valid = Signal()
        self.o_w_cf = [Signal(signed(self.cb), name=f"o_w_cf{k}") for k in range(2)]
        self.o_w_cs = [Signal(signed(self.cb), name=f"o_w_cs{k}") for k in range(2)]
        self.o_w_a = Signal(self.ab)
        # period records
        self.o_per_valid = Signal()
        self.o_per_at = Signal(AT_BITS)
        self.o_per_peak = Signal(self.ab)
        self.o_per_pos = Signal(range(SLOT))
        self.o_per_sum = Signal(34)
        self.o_per_locked = Signal()
        self.o_per_offset = Signal(range(SLOT))
        self.o_per_parity = Signal()
        # the statistics of the last whole period, live marker counters
        self.st_peak = Signal(self.ab)
        self.st_pos = Signal(range(SLOT))
        self.st_sum = Signal(34)
        self.st_locked = Signal()
        self.st_offset = Signal(range(SLOT))
        self.st_parity = Signal()
        self.st_stray = Signal(32)
        self.st_missed = Signal(32)
        # head records
        self.o_head_valid = Signal()
        self.o_head_late = Signal()
        self.o_head_at = Signal(AT_BITS)
        self.o_head_word = Signal(2)
        self.o_head_corr = [Signal(signed(self.cb), name=f"o_head_corr{k}") for k in range(2)]
        self.o_head_theta = Signal(32)
        self.o_head_amp = Signal(signed(26))
        self.o_head_numbered = Signal()
        self.o_head_number = Signal(range(SLOTS))
        self.o_head_frame = Signal(32)

    def elaborate(self, platform):
        m = Module()
        rb, cb, ab = self.rb, self.cb, self.ab

        # ---- input side: which accepted symbol completes a decision window
        cnt_in = Signal(range(SYNC))
        ipos = Signal(range(SLOT))
        locked = Signal()
        offset = Signal(range(SLOT))
        release = Signal()
        do_restart = Signal()
        completes = Signal()
        m.d.comb += completes.eq(cnt_in == SYNC - 1)
        with m.If(self.i_accept):
            with m.If(~completes):
                m.d.sync += cnt_in.eq(cnt_in + 1)
            with m.Else():
                m.d.sync += ipos.eq(Mux(ipos == SLOT - 1, 0, ipos + 1))
                with m.If((locked & (ipos == offset)) | (ipos == SLOT - 1)):
                    m.d.sync += self.hold.eq(1)
        with m.If(release):
            m.d.sync += self.hold.eq(0)
        with m.If(do_restart):
            m.d.sync += [cnt_in.eq(0), ipos.eq(0), self.hold.eq(0)]

        # the window flag travels with the symbol through the NCO's 3 cycles
        flag = Signal(3)
        m.d.sync += flag.eq(Cat(self.i_accept & completes, flag[:2]))

        # ---- a/b line ----------------------------------------------------
        a = Signal(signed(rb + 1))
        b = Signal(signed(rb + 1))
        ab_v = Signal()
        ab_f = Signal()
        m.d.sync += [
            a.eq(self.r[0] + self.r[1]),
            b.eq(self.r[1] - self.r[0]),
            ab_v.eq(self.r_valid),
            ab_f.eq(self.r_valid & flag[2]),
        ]
        la = [Signal(signed(rb + 1), name=f"la{k}") for k in range(SYNC)]
        lb = [Signal(signed(rb + 1), name=f"lb{k}") for k in range(SYNC)]
        with m.If(ab_v):
            m.d.sync += [la[0].eq(a), lb[0].eq(b)]
            m.d.sync += [la[k].eq(la[k - 1]) for k in range(1, SYNC)]
            m.d.sync += [lb[k].eq(lb[k - 1]) for k in range(1, SYNC)]
        line_f = Signal()
        m.d.sync += line_f.eq(ab_f)

        # ---- the four trees ----------------------------------------------
        def leaves(word):
            re, im = [], []
            for k, (wi, wq) in enumerate(word_symbols(word)):
                x, y = la[SYNC - 1 - k], lb[SYNC - 1 - k]  # w_k pairs with r_{n+k}
                if wi == wq:
                    re.append((wi, x))
                    im.append((wi, y))
                else:
                    re.append((-wi, y))
                    im.append((wi, x))
            return re, im

        def tree(lv, name):
            cur = list(lv)
            lvl = 0
            while len(cur) > 1:
                nxt = []
                for i in range(0, len(cur), 2):
                    if i + 1 == len(cur):
                        s, v = cur[i]
                        r = Signal(signed(len(v)), name=f"{name}_l{lvl}_{i // 2}")
                        m.d.sync += r.eq(v)
                        nxt.append((s, r))
                        continue
                    (sa, va), (sb, vb) = cur[i], cur[i + 1]
                    r = Signal(signed(min(max(len(va), len(vb)) + 1, cb)),
                               name=f"{name}_l{lvl}_{i // 2}")
                    if sa == sb:
                        m.d.sync += r.eq(va + vb)
                        nxt.append((sa, r))
                    elif sa > 0:
                        m.d.sync += r.eq(va - vb)
                        nxt.append((1, r))
                    else:
                        m.d.sync += r.eq(vb - va)
                        nxt.append((1, r))
                cur = nxt
                lvl += 1
            s, v = cur[0]
            out = Signal(signed(cb), name=name)
            m.d.comb += out.eq(v if s > 0 else -v)
            return out, lvl

        fre, fim = leaves(FSYNC)
        sre, sim = leaves(SSYNC)
        cf_re, depth = tree(fre, "cf_re")
        cf_im, _ = tree(fim, "cf_im")
        cs_re, _ = tree(sre, "cs_re")
        cs_im, _ = tree(sim, "cs_im")
        tf = Signal(depth)
        m.d.sync += tf.eq(Cat(line_f, tf[:-1]))
        mf = Signal(ab)
        ms = Signal(ab)
        c1 = [Signal(signed(cb), name=f"c1_{k}") for k in range(4)]
        v1 = Signal()
        m.d.sync += [
            mf.eq(mag_approx(cf_re, cf_im)),
            ms.eq(mag_approx(cs_re, cs_im)),
            c1[0].eq(cf_re), c1[1].eq(cf_im), c1[2].eq(cs_re), c1[3].eq(cs_im),
            v1.eq(tf[-1]),
        ]
        wv = Signal()
        wm = Signal(ab)
        wc = [Signal(signed(cb), name=f"wc{k}") for k in range(4)]
        m.d.sync += [wv.eq(v1), wm.eq(Mux(mf > ms, mf, ms))]
        m.d.sync += [wc[k].eq(c1[k]) for k in range(4)]

        # ---- per window: the accumulator, the period ---------------------
        acc = Memory(shape=unsigned(ab), depth=SLOT, init=[])
        m.submodules.acc = acc
        rp = acc.read_port()
        wp = acc.write_port()
        pos_next = Signal(range(SLOT))
        wat = Signal(AT_BITS)
        first = Signal(init=1)
        p0_v = Signal()
        p0_pos = Signal(range(SLOT))
        p0_at = Signal(AT_BITS)
        p0_m = Signal(ab)
        p0_c = [Signal(signed(cb), name=f"p0_c{k}") for k in range(4)]
        m.d.comb += [rp.addr.eq(pos_next), rp.en.eq(wv)]
        m.d.sync += p0_v.eq(wv)
        with m.If(wv):
            m.d.sync += [
                p0_pos.eq(pos_next),
                p0_at.eq(wat),
                p0_m.eq(wm),
                pos_next.eq(Mux(pos_next == SLOT - 1, 0, pos_next + 1)),
                wat.eq(wat + 1),
            ]
            m.d.sync += [p0_c[k].eq(wc[k]) for k in range(4)]
        a_old = Signal(ab)
        a_new = Signal(ab)
        diff = Signal(signed(ab + 1))
        m.d.comb += [
            a_old.eq(Mux(first, 0, rp.data)),
            diff.eq(p0_m - a_old),
            a_new.eq(a_old + (diff >> LEAK_SHIFT)),
            wp.addr.eq(p0_pos),
            wp.data.eq(a_new),
            wp.en.eq(p0_v),
        ]
        peak = Signal(ab)
        peak_pos = Signal(range(SLOT))
        psum = Signal(34)
        cur_at = Signal(AT_BITS)
        cur_c = [Signal(signed(cb), name=f"cur_c{k}") for k in range(4)]
        go_head = Signal()
        go_end = Signal()
        m.d.sync += [go_head.eq(0), go_end.eq(0), self.o_w_valid.eq(0)]
        with m.If(p0_v):
            with m.If(p0_pos == 0):
                m.d.sync += [peak.eq(a_new), peak_pos.eq(0), psum.eq(a_new)]
            with m.Else():
                with m.If(a_new > peak):
                    m.d.sync += [peak.eq(a_new), peak_pos.eq(p0_pos)]
                m.d.sync += psum.eq(psum + a_new)
            with m.If(p0_pos == SLOT - 1):
                m.d.sync += first.eq(0)
            m.d.sync += [
                cur_at.eq(p0_at),
                go_head.eq(locked & (p0_pos == offset)),
                go_end.eq(p0_pos == SLOT - 1),
            ]
            m.d.sync += [cur_c[k].eq(p0_c[k]) for k in range(4)]
            if self.detail:
                m.d.sync += [
                    self.o_w_valid.eq(1),
                    self.o_w_cf[0].eq(p0_c[0]), self.o_w_cf[1].eq(p0_c[1]),
                    self.o_w_cs[0].eq(p0_c[2]), self.o_w_cs[1].eq(p0_c[3]),
                    self.o_w_a.eq(a_new),
                ]

        # ---- decisions ---------------------------------------------------
        cand = Signal(range(SLOT))
        count = Signal(range(LOCK_PERIODS + 1))
        misses = Signal(range(LOSS_PERIODS + 1))
        heads = Signal(2)  # 0: none since lock, 1: head 0 held, 2: parity known
        kpar = Signal()  # parity of the next head's k
        parity = Signal()
        f_at = Signal(AT_BITS)
        f_c = [Signal(signed(cb), name=f"f_c{k}") for k in range(4)]
        have_ss = Signal()
        last_ss = [Signal(signed(cb), name=f"last_ss{k}") for k in range(2)]
        majority = Signal(signed(5))
        numbered = Signal()
        number = Signal(range(SLOTS))
        frame = Signal(32)
        have_ff = Signal()
        last_ff_at = Signal(AT_BITS)
        last_ff = Signal(32)
        fmiss = Signal(range(FRAME_MISS_LIMIT + 1))
        wh_at = Signal(AT_BITS)
        wh_c = [Signal(signed(cb), name=f"wh_c{k}") for k in range(4)]
        wh_k = Signal()
        wh_second = Signal()  # the held head 0 is being done; head 1 follows
        wh_late = Signal()
        wh_word = Signal(2)
        wh_corr = [Signal(signed(cb), name=f"wh_corr{k}") for k in range(2)]
        end_pending = Signal()

        m.submodules.cordic = cordic = Cordic()
        m.submodules.mre = mre = SeqMul(cb, cb, b_signed=True)
        m.submodules.mim = mim = SeqMul(cb, cb, b_signed=True)
        m.submodules.fdiv = fdiv = SeqDiv(AT_BITS, 21)
        m.d.sync += [
            cordic.start.eq(0), mre.start.eq(0), mim.start.eq(0), fdiv.start.eq(0),
            self.h_valid.eq(0), self.o_head_valid.eq(0), self.o_per_valid.eq(0),
            release.eq(0), do_restart.eq(0), self.decided.eq(0),
        ]
        m.d.comb += self.restart.eq(do_restart)

        def m_diff(c):  # M(C_S) - M(C_F)
            return mag_approx(c[2], c[3]).as_signed() - mag_approx(c[0], c[1]).as_signed()

        lock_counts = Signal()
        m.d.comb += lock_counts.eq((peak > 0) & ((peak * SLOT) >= (psum * LOCK_RATIO)))

        def restart_all():
            m.d.sync += [
                do_restart.eq(1), locked.eq(0), offset.eq(0), cand.eq(0), count.eq(0),
                misses.eq(0), heads.eq(0), kpar.eq(0), parity.eq(0), have_ss.eq(0),
                majority.eq(0), numbered.eq(0), number.eq(0), fmiss.eq(0),
                first.eq(1), pos_next.eq(0), wat.eq(self.i_tot),
                self.st_peak.eq(0), self.st_pos.eq(0), self.st_sum.eq(0), self.st_locked.eq(0),
                self.st_offset.eq(0), self.st_parity.eq(0), self.st_stray.eq(0),
                self.st_missed.eq(0),
            ]

        with m.FSM(name="decide"):
            with m.State("IDLE"):
                with m.If(self.loss):
                    restart_all()
                with m.Elif(go_head):
                    m.d.sync += end_pending.eq(go_end)
                    m.next = "HEAD"
                with m.Elif(go_end):
                    m.next = "END"
            with m.State("HEAD"):
                with m.If(heads == 0):
                    m.d.sync += [f_at.eq(cur_at), heads.eq(1)]
                    m.d.sync += [f_c[k].eq(cur_c[k]) for k in range(4)]
                    m.next = "HEAD_DONE"
                with m.Elif(heads == 1):
                    p = m_diff(f_c) < m_diff(cur_c)
                    m.d.sync += [
                        parity.eq(p), self.st_parity.eq(p), heads.eq(2), wh_at.eq(f_at),
                        wh_k.eq(0), wh_second.eq(1), wh_late.eq(1), kpar.eq(0),
                    ]
                    m.d.sync += [wh_c[k].eq(f_c[k]) for k in range(4)]
                    m.next = "WORD"
                with m.Else():
                    m.d.sync += [
                        wh_at.eq(cur_at), wh_k.eq(kpar), wh_second.eq(0), wh_late.eq(0),
                        kpar.eq(~kpar),
                    ]
                    m.d.sync += [wh_c[k].eq(cur_c[k]) for k in range(4)]
                    m.next = "WORD"
            with m.State("WORD"):
                with m.If(wh_k == parity):
                    m.d.sync += [
                        last_ss[0].eq(wh_c[2]), last_ss[1].eq(wh_c[3]), have_ss.eq(1),
                        wh_word.eq(W_SSYNC), wh_corr[0].eq(wh_c[2]), wh_corr[1].eq(wh_c[3]),
                    ]
                    m.next = "NUMBER"
                with m.Elif(~have_ss):
                    m.next = "NEXT_WORD"  # no SSync head before it: dropped
                with m.Else():
                    m.d.sync += [
                        mre.a.eq(wh_c[0]), mre.b.eq(last_ss[0]), mre.start.eq(1),
                        mim.a.eq(wh_c[1]), mim.b.eq(last_ss[1]), mim.start.eq(1),
                    ]
                    m.next = "SIGN"
            with m.State("SIGN"):
                with m.If(mre.done):
                    neg = (mre.p + mim.p) < 0
                    m.d.sync += [
                        wh_word.eq(Mux(neg, W_NFSYNC, W_FSYNC)),
                        wh_corr[0].eq(Mux(neg, -wh_c[0], wh_c[0])),
                        wh_corr[1].eq(Mux(neg, -wh_c[1], wh_c[1])),
                    ]
                    m.next = "NUMBER"
            with m.State("NUMBER"):
                nnum = Mux(number == SLOTS - 1, 0, number + 1)
                m.d.sync += [cordic.re.eq(wh_corr[0]), cordic.im.eq(wh_corr[1])]
                with m.If(numbered):
                    m.d.sync += number.eq(nnum)
                cur_num = Mux(numbered, nnum, number)
                with m.If(wh_word == W_SSYNC):
                    m.d.sync += cordic.start.eq(1)
                    m.next = "CORDIC"
                with m.Else():
                    positive = wh_word == W_FSYNC
                    mag_ok = (majority >= MAJORITY_MIN) | (majority <= -MAJORITY_MIN)
                    marked = mag_ok & ((majority > 0) != positive)
                    nm = Mux(positive, majority + 1, majority - 1)
                    m.d.sync += majority.eq(Mux(nm > MAJORITY_MAX, MAJORITY_MAX,
                                                Mux(nm < -MAJORITY_MAX, -MAJORITY_MAX, nm)))
                    with m.If(~numbered & marked):
                        m.d.sync += [numbered.eq(1), number.eq(0)]
                        m.next = "NEW_FRAME"
                    with m.Elif(numbered & (cur_num == 0) & marked):
                        m.d.sync += fmiss.eq(0)
                        m.next = "NEW_FRAME"
                    with m.Elif(numbered & (cur_num == 0)):
                        m.d.sync += [missed.eq(missed + 1) for missed in [self.st_missed]]
                        m.d.sync += fmiss.eq(fmiss + 1)
                        with m.If(fmiss + 1 >= FRAME_MISS_LIMIT):
                            m.next = "LOST"
                        with m.Else():
                            m.next = "NEW_FRAME"
                    with m.Else():
                        with m.If(numbered & marked):
                            m.d.sync += self.st_stray.eq(self.st_stray + 1)
                        m.d.sync += cordic.start.eq(1)
                        m.next = "CORDIC"
            with m.State("NEW_FRAME"):
                with m.If(~have_ff):
                    m.d.sync += [frame.eq(0), last_ff.eq(0), last_ff_at.eq(wh_at), have_ff.eq(1),
                                 cordic.start.eq(1)]
                    m.next = "CORDIC"
                with m.Else():
                    m.d.sync += [
                        fdiv.n.eq(wh_at - last_ff_at + FRAME // 2), fdiv.d.eq(FRAME),
                        fdiv.start.eq(1),
                    ]
                    m.next = "FDIV"
            with m.State("FDIV"):
                with m.If(fdiv.done):
                    d = Mux(fdiv.q == 0, 1, fdiv.q)
                    nf = (last_ff + d)[:32]
                    m.d.sync += [frame.eq(nf), last_ff.eq(nf), last_ff_at.eq(wh_at),
                                 cordic.start.eq(1)]
                    m.next = "CORDIC"
            with m.State("CORDIC"):
                with m.If(cordic.done):
                    m.d.sync += [
                        self.h_valid.eq(1), self.h_ssync.eq(wh_word == W_SSYNC),
                        self.h_theta.eq(cordic.theta),
                    ]
                    m.next = "CARRIER"
            with m.State("CARRIER"):
                with m.If(self.h_done):
                    m.d.sync += [
                        self.o_head_valid.eq(1),
                        self.o_head_late.eq(wh_late),
                        self.o_head_at.eq(wh_at),
                        self.o_head_word.eq(wh_word),
                        self.o_head_corr[0].eq(wh_corr[0]),
                        self.o_head_corr[1].eq(wh_corr[1]),
                        self.o_head_theta.eq(cordic.theta),
                        self.o_head_amp.eq(cordic.mag),
                        self.o_head_numbered.eq(numbered),
                        self.o_head_number.eq(number),
                        self.o_head_frame.eq(frame),
                    ]
                    m.next = "NEXT_WORD"
            with m.State("NEXT_WORD"):
                with m.If(wh_second):
                    m.d.sync += [wh_second.eq(0), wh_late.eq(0), wh_at.eq(cur_at), wh_k.eq(1),
                                 kpar.eq(0)]
                    m.d.sync += [wh_c[k].eq(cur_c[k]) for k in range(4)]
                    m.next = "WORD"
                with m.Else():
                    m.next = "HEAD_DONE"
            with m.State("HEAD_DONE"):
                with m.If(end_pending):
                    m.next = "END"
                with m.Else():
                    m.next = "RELEASE"
            with m.State("END"):
                m.d.sync += [self.st_peak.eq(peak), self.st_pos.eq(peak_pos), self.st_sum.eq(psum),
                             self.st_parity.eq(parity), end_pending.eq(0)]
                with m.If(~locked):
                    nc = Mux(lock_counts, Mux(peak_pos == cand, count + 1, 1), 0)
                    lock_now = nc >= LOCK_PERIODS
                    m.d.sync += [
                        cand.eq(peak_pos), count.eq(Mux(lock_now, 0, nc)),
                        locked.eq(lock_now), offset.eq(Mux(lock_now, peak_pos, 0)), misses.eq(0),
                        self.st_locked.eq(lock_now), self.st_offset.eq(Mux(lock_now, peak_pos, 0)),
                    ]
                    m.next = "PERIOD"
                with m.Else():
                    nmiss = Mux(lock_counts & (peak_pos == offset), 0, misses + 1)
                    with m.If(nmiss >= LOSS_PERIODS):
                        m.next = "LOST"
                    with m.Else():
                        m.d.sync += [misses.eq(nmiss), self.st_locked.eq(1),
                                     self.st_offset.eq(offset)]
                        m.next = "PERIOD"
            with m.State("PERIOD"):
                m.d.sync += [
                    self.o_per_valid.eq(1),
                    self.o_per_at.eq(cur_at),
                    self.o_per_peak.eq(self.st_peak),
                    self.o_per_pos.eq(self.st_pos),
                    self.o_per_sum.eq(self.st_sum),
                    self.o_per_locked.eq(self.st_locked),
                    self.o_per_offset.eq(self.st_offset),
                    self.o_per_parity.eq(self.st_parity),
                ]
                m.next = "RELEASE"
            with m.State("RELEASE"):
                m.d.sync += [release.eq(1), self.decided.eq(1)]
                m.next = "IDLE"
            with m.State("LOST"):
                # Everything of stage 1 restarts at the next input symbol
                # (the frame counter's history stays). A frame-sync loss at
                # a head on the period's last position is followed, in the
                # model, by that period's end on the cleared state, which
                # changes nothing and reports an all-zero period: so here.
                restart_all()
                m.d.sync += self.decided.eq(1)
                with m.If(end_pending):
                    m.d.sync += end_pending.eq(0)
                    m.next = "PERIOD"
                with m.Else():
                    m.next = "IDLE"
        return m


class SyncFront(Elaboratable):
    """Stages 2a, 1 and 2b: the NCO, the slot sync and the carrier loop.

    Input: one record per `i_valid & i_ready` cycle: a symbol (`i_x`) or,
    with `i_loss`, `i_lost` symbols lost before the next one (the `Symbols`
    reader's gap: the index advances, everything restarts but the carrier
    register; `o_restart` and `o_restart_loss` pulse). Outputs: the NCO
    stream (`o_r_valid`, `o_r`, `o_phase`), the register updates
    (`o_f_valid`, `nco.f`, `o_f_at` = the index of the first symbol rotated
    with it) and the records of `SlotSync` (`self.sync.o_*`).
    """

    def __init__(self, in_bits=6, carrier=0, table=None, detail=True):
        self.nco = Nco(in_bits, carrier, table)
        self.sync = SlotSync(in_bits, detail)
        self.in_bits = in_bits
        self.i_valid = Signal()
        self.i_ready = Signal()
        self.i_x = self.nco.i_x
        self.i_loss = Signal()
        self.i_lost = Signal(32)
        self.o_r_valid = self.nco.o_valid
        self.o_r = self.nco.o_r
        self.o_phase = self.nco.o_phase
        self.o_at = self.nco.o_at
        self.o_f_valid = self.nco.o_f_valid
        self.o_f_at = Signal(AT_BITS)
        self.o_restart = Signal()
        self.o_restart_loss = Signal()

    def elaborate(self, platform):
        m = Module()
        m.submodules.nco = nco = self.nco
        m.submodules.sync = sy = self.sync
        tot = sy.i_tot
        drain = Signal(range(32))
        loss_go = Signal()
        m.d.comb += [
            self.i_ready.eq(~sy.hold & (drain == 0) & ~loss_go),
            nco.i_valid.eq(self.i_valid & ~self.i_loss),
            nco.i_ready.eq(self.i_ready),
            sy.i_accept.eq(nco.accept),
            sy.r_valid.eq(nco.o_valid),
            sy.r[0].eq(nco.o_r[0]),
            sy.r[1].eq(nco.o_r[1]),
            nco.h_valid.eq(sy.h_valid),
            nco.h_ssync.eq(sy.h_ssync),
            nco.h_theta.eq(sy.h_theta),
            sy.h_done.eq(nco.h_done),
            nco.restart.eq(sy.restart),
            sy.loss.eq(loss_go),
            self.o_f_at.eq(tot),
            nco.i_at.eq(tot),
        ]
        with m.If(nco.accept):
            m.d.sync += tot.eq(tot + 1)
        m.d.sync += [self.o_restart.eq(sy.restart), self.o_restart_loss.eq(loss_go), loss_go.eq(0)]
        # a loss: let the windows in flight through, then restart at the
        # index after the gap
        with m.If(self.i_valid & self.i_loss & self.i_ready):
            m.d.sync += [tot.eq(tot + self.i_lost), drain.eq(24)]
        with m.Elif(drain > 1):
            m.d.sync += drain.eq(drain - 1)
        with m.Elif(drain == 1):
            m.d.sync += [drain.eq(0), loss_go.eq(1)]
        return m
