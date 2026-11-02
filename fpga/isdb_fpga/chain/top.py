"""The ISDB-S3 chain after the front end, wired (plan "The chain in the
FPGA"): `SlotsChain` (stages 1-6 and 8-9 on the chain's decoder port,
`ldpc_if.LdpcPort`) and `SlotsRx`, the sketch of the bitstream block: the
front end's `SymbolFrontend` output into the chain, the decoder bridge,
the `Slots` words out, and the command protocol of `control.py` on the
same GPIO pair with the chain's registers.

Data path, all in the symbol clock domain (`util_ad9361_divclk`, 40 MHz)
but the decoder (`ldpc` domain, 200 MHz, behind `LdpcBridge`'s FIFOs):

  symbols (i8, one per clock at most) -> SyncFront (NCO, slot sync,
  carrier loop; holds the input around decisions and while stage 3's ring
  is full) -> SlotPhase (one slot of delay) -> Back (equaliser + FrameCtl,
  TMCC soft bits and burst, demapper) -> LDPC port -> Finish (dispersal,
  BCH, TMCC layout into FrameCtl, slot / TMCC / status blocks) -> Packer ->
  `Slots` words.

The status snapshot: SyncFront latches the sync statistics and the
carrier register at each decision's end; SlotPhase keeps the one of the
head that completes each slot and gives the frame-final slot's; at a
frame's end the back end adds the taps in use, their frame, the frame's
sums (decoded frames), the demapper's n0, K, learned-from and learned
modulations in force (before the frame's swap) and the AGC register; the
finish stage adds the counters when it sends the status block.

The FIFO in front (`SlotsRx`): the front end delivers at most one symbol
per sample and holds no back-pressure; the chain holds its input about
0.7 % of the clocks (stage 1's decisions) and the front end's symbol rate
is 84 % of the clocks: a 1 024-entry FIFO (2 RAMB18) absorbs the holds.

Register map (`SlotsRx`, the command protocol of `control.py`: ADDR, HI,
WRITE, READ, CTRL on `gpio_out`, `gpio_in` the ID word or the readback):

| Address | Register |
|---|---|
| 0x100 | CARRIER (read only): the carrier register in use (its initial value is the build's; no write port yet) |
| 0x101 | MAX_ITER (8 bits, reset 16): the LDPC iteration limit each codeword is committed with |
| 0x102 | AGC: the input AGC gain reported in the status (Q6.10) |
| other | the front end's (`control.py`): a READ of an address outside 0x100 .. 0x14F and 0x200 .. 0x333 shows the front end's readback |
| 0x103 | FEATURES (read only): bit 0 the chain (SlotsRx), bit 1 layout lag 2 |
| 0x110 + 2k, + 2k + 1 | tap k (re, im) Q2.14 into the pending set |
| 0x126 | TAP_CTRL: write 1: the pending taps in use now; 2: armed for frame TAP_AT |
| 0x127 | TAP_AT, 0x128 TAP_FROM (the frame the taps were solved from) |
| 0x130 | SUMS_FRAME, 0x131 SUMS_COUNT (read only) |
| 0x200 + w | the latched training sums, word w (0 .. 307; the read waits for the MAC unit) |
| 0x140 .. 0x146 | the counters (frames, undecoded, TMCC failed, slots, LDPC failed, BCH failed, iterations low) |
| 0x147 | TMCC_OVERRUNS: TMCC codewords the decoder dropped (also counted as TMCC failed) |
| CTRL | bit 0 RUN, bit 1 START (the chain restarts; the packer's sequence restarts), bit 3 SLOTS (the DMA carries the `Slots` words) |

The ID word gains the chain's version: 0x15DB, format 4, taps, and bit 8
(`ID_CHAIN`) when the chain is in the image (the front end's ID word had
bits 8-11 zero), so a host tells the images apart from one register read.
"""

from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal, signed

from .back import Back
from .finish import Finish
from .ldpc_if import OP_DONE
from .packer import status_layout
from .phase import SlotPhase
from .sync import SNAP_FIELDS, SyncFront

SNAP_BITS = sum(w for _, w in SNAP_FIELDS)
ID_CHAIN = 1 << 8


def split(sig, fields):
    out, o = {}, 0
    for n, w in fields:
        out[n] = sig[o:o + w]
        o += w
    return out


class SlotsChain(Elaboratable):
    def __init__(self, in_bits=6, carrier=0, max_iter=50, offset=1):
        self.front = SyncFront(in_bits, carrier, detail=False)
        self.phase = SlotPhase(in_bits, snap_bits=SNAP_BITS)
        self.back = Back(max_iter, offset)
        self.finish = Finish()
        self.port = self.back.port
        self.i_valid = self.front.i_valid
        self.i_ready = self.front.i_ready
        self.i_x = self.front.i_x
        self.i_loss = self.front.i_loss
        self.i_lost = self.front.i_lost
        self.agc = Signal(16)
        self.r_overrun = Signal()  # from the bridge (a dropped codeword's record)
        self.o_valid = self.finish.o_valid
        self.o_word = self.finish.o_word
        self.tmcc_done = Signal(16)  # TMCC codewords committed (verification)

    def elaborate(self, platform):
        m = Module()
        m.submodules.front = sf = self.front
        m.submodules.phase = ph = self.phase
        m.submodules.back = bk = self.back
        m.submodules.finish = fin = self.finish
        sy = sf.sync
        eq, fc, dm = bk.eq, bk.fc, bk.demap
        m.d.comb += [
            sf.ext_hold.eq(ph.full),
            ph.i_valid.eq(sf.o_r_valid), ph.i_at.eq(sf.o_at),
            ph.i_x[0].eq(sf.o_x[0]), ph.i_x[1].eq(sf.o_x[1]), ph.i_phi.eq(sf.o_phase),
            ph.h_f.eq(sf.nco.f),
            ph.h_valid.eq(sy.o_head_valid), ph.h_late.eq(sy.o_head_late), ph.h_at.eq(sy.o_head_at),
            ph.h_word.eq(sy.o_head_word), ph.h_theta.eq(sy.o_head_theta), ph.h_amp.eq(sy.o_head_amp),
            ph.h_numbered.eq(sy.o_head_numbered), ph.h_number.eq(sy.o_head_number),
            ph.h_frame.eq(sy.o_head_frame), ph.restart.eq(sf.o_restart), ph.h_snap.eq(sf.snap),
            eq.i_valid.eq(ph.o_valid), eq.i_y[0].eq(ph.o_y[0]), eq.i_y[1].eq(ph.o_y[1]),
            eq.i_meta.eq(Cat(ph.o_j, ph.o_number, ph.o_numbered, ph.o_frame, ph.o_word, ph.o_at)),
            bk.restart.eq(ph.o_restart),
        ]
        # the frame's status snapshot at its end (FrameCtl's o_done)
        sn = split(ph.o_snap, SNAP_FIELDS)
        snap = {n: Signal(w, name=f"fs_{n}") for n, w in status_layout()}
        with m.If(fc.o_done):
            m.d.sync += [snap[n].eq(sn[n]) for n, _ in SNAP_FIELDS]
            for k in range(11):
                for c in range(2):
                    m.d.sync += snap[f"tap{k}_{c}"].eq(eq.taps[k][c])
            m.d.sync += [
                snap["agc"].eq(self.agc),
                snap["taps_from"].eq(eq.taps_from),
                snap["sums_frame"].eq(Mux(fc.o_has_layout, fc.o_frame, 0xFFFFFFFF)),
                snap["n0"].eq(dm.n0), snap["k"].eq(dm.kk), snap["learned_from"].eq(dm.learned_from),
                snap["learned"].eq(dm.learned_valid),
            ]
        st = fin.status
        for n, _ in status_layout():
            if n not in fin.counters and n != "iterations":
                m.d.comb += st.snap[n].eq(snap[n])
        fr_dec = Signal()
        with m.If(fc.o_done):
            m.d.sync += fr_dec.eq(fc.o_has_layout)
        m.d.comb += [
            fin.f_valid.eq(fc.o_done), fin.f_frame.eq(fc.o_frame), fin.f_decoded.eq(fc.o_has_layout),
            dm.st_addr.eq(st.pt_addr), st.pt_data.eq(dm.st_data), dm.st_prev.eq(fr_dec),
        ]
        # the TMCC's layout into FrameCtl
        lay = fin.tmcc.layout
        m.d.comb += [
            fc.lw_open.eq(lay.lw_open), fc.lw_en.eq(lay.lw_en), fc.lw_slot.eq(lay.lw_slot),
            fc.lw_data.eq(lay.lw_data), fc.lw_commit.eq(lay.lw_commit), fc.lw_frame.eq(lay.lw_frame),
            fc.lw_from.eq(lay.lw_from),
        ]
        # the decoder's results
        p = self.port
        m.d.comb += [
            fin.r_valid.eq(p.out_valid), fin.r_head.eq(p.out_head), fin.r_tag.eq(p.out_tag),
            fin.r_iterations.eq(p.out_iterations), fin.r_converged.eq(p.out_converged),
            fin.r_overrun.eq(self.r_overrun), fin.r_bits.eq(p.out_bits), fin.r_last.eq(p.out_last),
            p.out_ready.eq(fin.r_ready),
        ]
        with m.If(p.in_valid & p.in_ready & p.in_ctx & (p.in_op == OP_DONE)):
            m.d.sync += self.tmcc_done.eq(self.tmcc_done + 1)
        return m


class SlotsRx(Elaboratable):
    """The bitstream block (a sketch: elaborated, estimated, not simulated
    as a whole): the front end (`top.SymbolsRx`'s core and control), a
    1 024-symbol FIFO, `SlotsChain`, the decoder bridge (`ldpc_if.LdpcBridge`)
    and the decoder (`ldpc_cls`, the other branch's `isdb_fpga.ldpc` decoder;
    None: its ports are left as the block's ports), the output selection
    (raw IQ, `Symbols`, `Slots`). See the module docstring for the map."""

    def __init__(self, taps, cos_table, h=None, g=None, k=None, ldpc_cls=None):
        from ..frontend import SymbolFrontend
        from .ldpc_if import LdpcBridge

        self.core = SymbolFrontend(taps, cos_table, h, g, k)
        self.chain = SlotsChain(max_iter=16)  # MAX_ITER's reset: the decoder's default
        self.bridge = LdpcBridge(self.chain.port)
        self.ldpc_cls = ldpc_cls
        self.taps = taps
        self.adc_valid = Signal()
        self.adc_i = Signal(signed(16))
        self.adc_q = Signal(signed(16))
        self.pass_valid = Signal()
        self.pass_data0 = Signal(16)
        self.pass_data1 = Signal(16)
        self.pack_valid = Signal()
        self.pack_data0 = Signal(16)
        self.pack_data1 = Signal(16)
        self.gpio_out = Signal(32)
        self.gpio_in = Signal(32)

    def ports(self):
        b = self.bridge
        return [self.adc_valid, self.adc_i, self.adc_q, self.pass_valid, self.pass_data0, self.pass_data1,
                self.pack_valid, self.pack_data0, self.pack_data1, self.gpio_out, self.gpio_in,
                b.begin, b.d_ctx, b.d_valid, b.d_pos, b.d_llr, b.d_rate, b.start, b.st_rate, b.st_max_iter,
                b.st_offset, b.st_short, b.o_valid, b.o_data, b.o_last, b.o_ready, b.res_valid,
                b.res_iterations, b.res_converged, b.res_unsat, b.res_overrun]

    def elaborate(self, platform):
        from amaranth.lib.fifo import SyncFIFOBuffered

        from ..control import OP_ADDR, OP_CTRL, OP_HI, OP_READ, OP_WRITE, Control

        m = Module()
        m.submodules.core = core = self.core
        m.submodules.control = ctl = Control(core)
        m.submodules.chain = ch = self.chain
        m.submodules.bridge = br = self.bridge
        m.submodules.fifo = fifo = SyncFIFOBuffered(width=12, depth=1024)
        if self.ldpc_cls is not None:
            m.submodules.ldpc = self.ldpc_cls(br)
        # the front end's symbols (the quantiser's 6-bit k output) into the chain
        slots_on = Signal()
        m.d.comb += [
            core.i_valid.eq(self.adc_valid & ctl.run),
            core.i_i.eq(self.adc_i), core.i_q.eq(self.adc_q),
            fifo.w_en.eq(core.s_valid & slots_on),
            fifo.w_data.eq(Cat(core.s_q[0][:6], core.s_q[1][:6])),
            ch.i_valid.eq(fifo.r_rdy), fifo.r_en.eq(ch.i_ready),
            ch.i_x[0].eq(fifo.r_data[:6]), ch.i_x[1].eq(fifo.r_data[6:]),
            ch.r_overrun.eq(br.out_overrun),
        ]
        # the chain's registers on the front end's command protocol (the
        # same synchronised gpio_out the front end's Control decodes)
        s1 = Signal(32)
        s2 = Signal(32)
        m.d.sync += [s1.eq(self.gpio_out), s2.eq(s1)]
        prev = Signal()
        edge = Signal()
        go = Signal()
        m.d.sync += [prev.eq(s2[31]), edge.eq(s2[31] != prev), go.eq(edge)]
        op = s2[28:31]
        arg = s2[1:28]
        addr = Signal(16)
        staged = Signal(16)
        value = Cat(arg[:16], staged)
        readback = Signal(32)
        show = Signal()
        agc = ch.agc
        eqm = ch.back.eq
        fin = ch.finish
        m.d.comb += [eqm.tap_we.eq(0), eqm.tap_now.eq(0), eqm.tap_arm.eq(0)]
        tap_at = Signal(32)
        tap_from = Signal(32)
        m.d.comb += [eqm.tap_at.eq(tap_at), eqm.tap_from_in.eq(tap_from)]
        # the chain's own addresses; a READ of any other goes to the front
        # end's Control (which decodes the same command words) and its
        # readback is what gpio_in shows
        chain_addr = Signal()
        m.d.comb += chain_addr.eq(((addr >= 0x100) & (addr < 0x150)) | ((addr >= 0x200) & (addr < 0x200 + 308)))
        bk = ch.back
        with m.If(go):
            # every command but a READ of a chain address hands gpio_in back
            m.d.sync += show.eq((op == OP_READ) & chain_addr)
            with m.Switch(op):
                with m.Case(OP_ADDR):
                    m.d.sync += addr.eq(arg[:16])
                with m.Case(OP_HI):
                    m.d.sync += staged.eq(arg[:16])
                with m.Case(OP_WRITE):
                    m.d.sync += addr.eq(addr + 1)
                    with m.If(addr == 0x101):
                        m.d.sync += bk.max_iter.eq(value[:8])
                    with m.If(addr == 0x102):
                        m.d.sync += agc.eq(value[:16])
                    with m.If((addr >= 0x110) & (addr < 0x126)):
                        m.d.comb += [eqm.tap_we.eq(addr[0] == 1), eqm.tap_idx.eq((addr - 0x110)[1:5])]
                    with m.If(addr == 0x126):
                        m.d.comb += [eqm.tap_now.eq(value[0]), eqm.tap_arm.eq(value[1])]
                    with m.If(addr == 0x127):
                        m.d.sync += tap_at.eq(value)
                    with m.If(addr == 0x128):
                        m.d.sync += tap_from.eq(value)
                with m.Case(OP_READ):
                    with m.Switch(addr):
                        with m.Case(0x100):
                            m.d.sync += readback.eq(ch.front.nco.f)
                        with m.Case(0x101):
                            m.d.sync += readback.eq(bk.max_iter)
                        with m.Case(0x102):
                            m.d.sync += readback.eq(agc)
                        with m.Case(0x103):
                            m.d.sync += readback.eq(0b11)
                        with m.Case(0x130):
                            m.d.sync += readback.eq(eqm.sums_frame)
                        with m.Case(0x131):
                            m.d.sync += readback.eq(eqm.sums_count)
                        for i, n in enumerate(("frames", "undecoded", "tmcc_failed", "slots", "ldpc_failed",
                                               "bch_failed")):
                            with m.Case(0x140 + i):
                                m.d.sync += readback.eq(fin.counters[n])
                        with m.Case(0x146):
                            m.d.sync += readback.eq(fin.iterations[:32])
                        with m.Case(0x147):
                            m.d.sync += readback.eq(fin.tmcc_overruns)
                        with m.Default():
                            m.d.sync += readback.eq(0)
                with m.Case(OP_CTRL):
                    m.d.sync += slots_on.eq(arg[3])
        # the low tap word holds re, the next im (the pending set takes both
        # at the im write)
        re_hold = Signal(16)
        with m.If(go & (op == OP_WRITE) & (addr >= 0x110) & (addr < 0x126) & (addr[0] == 0)):
            m.d.sync += re_hold.eq(value[:16])
        m.d.comb += [eqm.tap_val[0].eq(re_hold), eqm.tap_val[1].eq(value[:16])]
        # the training sums' readback (0x200 + w): requested at READ, shown when done
        m.d.comb += [eqm.rd_req.eq(go & (op == OP_READ) & (addr >= 0x200) & (addr < 0x200 + 308)),
                     eqm.rd_addr.eq((addr - 0x200)[:9])]
        with m.If(eqm.rd_ack):
            m.d.sync += readback.eq(eqm.rd_data)
        # the chain's readback, else the front end's (its own READs), whose
        # ID word gains ID_CHAIN
        from ..control import ID_MAGIC
        from ..frontend import FORMAT_VERSION

        ident = ID_MAGIC << 16 | FORMAT_VERSION << 12 | (self.taps & 0xFF)
        fe = ctl.gpio_in
        m.d.comb += self.gpio_in.eq(Mux(show, readback, Mux(fe == ident, ident | ID_CHAIN, fe)))
        # the output selection: the chain's Slots words, else the front end's block
        m.d.comb += [
            ctl.gpio_out.eq(self.gpio_out),
            self.pack_valid.eq(Mux(slots_on, ch.o_valid, Mux(ctl.symbols, core.o_valid, self.pass_valid))),
            self.pack_data0.eq(Mux(slots_on, ch.o_word, Mux(ctl.symbols, core.o_word, self.pass_data0))),
            self.pack_data1.eq(Mux(slots_on | ctl.symbols, 0, self.pass_data1)),
        ]
        return m
