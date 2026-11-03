"""The ISDB-S3 chain after the front end, wired (plan "The chain in the
FPGA"): `SlotsChain` (stages 1-6 and 8-9 on the chain's decoder port,
`ldpc_if.LdpcPort`) and `SlotsRx`, the sketch of the bitstream block: the
front end's `SymbolFrontend` output into the chain, the decoder bridge,
the `Slots` words out, and the command protocol of `control.py` on the
same GPIO pair with the chain's registers.

Data path in the symbol clock domain (`util_ad9361_divclk`, 40 MHz), with
the time-multiplexed datapaths (the demapper's metrics, the equaliser's
FIR) in its 4x (`fast`, l_clk, phase-aligned) and the decoder in its own
200 MHz (`ldpc`, behind `LdpcBridge`'s asynchronous FIFOs);
fpga/tezuka/isdb_slots.xdc has the constraints:

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
WRITE / WRITE_S, READ, CTRL on `gpio_out`, `gpio_in` the ID word or the
readback): the host's map, `isdb_frontend::control::chain` (fixed there
from this HDL, 2026-11-02), in the front end's address space at 0x400 -
0x6FF; a READ of any other address shows the front end's readback.

| Address | Register |
|---|---|
| 0x400 | VERSION (ro): the `Slots` format, 1 |
| 0x401 | FEATURES (ro): bit 0 TAPS, bit 1 SUMS (no host constellation or n0 at START: bits 2, 3 clear), bit 4 SUMS_CAPTURE; bits 15-8 MAX_ITER, bits 27-16 the decoder's step (34) |
| 0x402 | CARRIER: written, loaded into the carrier register at START; read, the register in use (2^-32 turn per symbol) |
| 0x403 | LAYOUT_LAG (ro): 2 (lag 1 is not built) |
| 0x404 | INIT: bit 0, START puts the written taps in use (bits 1, 2 ignored: FEATURES) |
| 0x405 | STATUS (ro): bit 0 locked, 1 a frame head since the (re)start, 2 the last frame decoded |
| 0x406 / 0x407 | FRAME (the last whole frame's counter, 0xFFFF_FFFF none) / SEQ (the next block's sequence number) (ro) |
| 0x408 / 0x409 / 0x40A / 0x40B | N0, LLR_SCALE, LEARNED_FROM, CONST_MASK (ro): n0, K, the frame they were learned on, the learned modulations, in force |
| 0x40C | MAX_ITER (8 bits, reset 14, `ldpc.DEFAULT_MAX_ITER`): the LDPC iteration limit each codeword is committed with |
| 0x40D | AGC: the input AGC gain reported in the status (Q6.10) |
| 0x410 + k | TAPS, k < 11: written (re + (im << 16), Q2.14), the pending taps; read, the taps in use |
| 0x41B | TAPS_FROM: written after TAPS, the frame N they were solved from: in use from frame N + 2 (the model's EQ_APPLY_FRAMES); read, the frame of the taps in use |
| 0x420 / 0x421 | SUMS_FRAME / SUMS_COUNT (ro): of the latched sums |
| 0x422 | SUMS_CAPTURE: written (any value), the next frame end latches the training sums and holds them (later frames are summed but not latched) until the next write; read, bit 0 VALID (latched and held since the write), bit 1 ARMED (waiting for a frame end). Before the first write every frame end latches |
| 0x430 .. 0x437 | COUNTERS (ro): frames, undecoded, TMCC failed, slots, LDPC failed, BCH failed, iterations low, high |
| 0x438 | TMCC_OVERRUNS (ro): TMCC codewords the decoder dropped (also counted as TMCC failed) |
| 0x500 + 4i, 0x610 + 4i | SUMS_R (i < 66), SUMS_P (i < 11): re low 32, re high 16 sign-extended, im low, im high (the read waits for the MAC unit) |
| 0x640 .. | CONST: not built (reads 0) |
| CTRL | the front end's bits (RUN, START, SYMBOLS) and bit 3 SLOTS (the DMA carries the `Slots` words); START also restarts the chain (the model's `Chain::loss` of 0 symbols: everything but the carrier register, the taps, the constellation and n0), loads CARRIER and, with INIT bit 0, the taps, and restarts the blocks' sequence numbers |

The ID word gains the chain's `Slots` format version in bits 11-8
(`ID_CHAIN`: 1 << 8), 0 on every front end so far; the front end's STATUS
(0x10) reads bit 2 while the `Slots` words are selected.
"""

from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal, signed

from .. import ldpc
from .back import Back
from .finish import Finish
from .ldpc_if import OP_DONE
from .packer import status_layout
from .phase import SlotPhase
from .results import ResultFifo
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
        self.results = ResultFifo()
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
        # the decoder's results, through the result FIFO (results.py)
        p = self.port
        m.submodules.results = rf = self.results
        m.d.comb += [
            rf.i_valid.eq(p.out_valid), rf.i_head.eq(p.out_head), rf.i_tag.eq(p.out_tag),
            rf.i_iterations.eq(p.out_iterations), rf.i_converged.eq(p.out_converged),
            rf.i_overrun.eq(self.r_overrun), rf.i_bits.eq(p.out_bits), rf.i_last.eq(p.out_last),
            p.out_ready.eq(rf.i_ready),
            fin.r_valid.eq(rf.o_valid), fin.r_head.eq(rf.o_head), fin.r_tag.eq(rf.o_tag),
            fin.r_iterations.eq(rf.o_iterations), fin.r_converged.eq(rf.o_converged),
            fin.r_overrun.eq(rf.o_overrun), fin.r_bits.eq(rf.o_bits), fin.r_last.eq(rf.o_last),
            rf.o_ready.eq(fin.r_ready),
        ]
        with m.If(p.in_valid & p.in_ready & p.in_ctx & (p.in_op == OP_DONE)):
            m.d.sync += self.tmcc_done.eq(self.tmcc_done + 1)
        return m


# The chain's registers (isdb_frontend::control::chain, the host's map; the
# module docstring): the front end's command address space at 0x400-0x6FF.
REG_BASE, REG_END = 0x400, 0x700
REG_VERSION = 0x400
REG_FEATURES = 0x401
REG_CARRIER = 0x402
REG_LAYOUT_LAG = 0x403
REG_INIT = 0x404
REG_STATUS = 0x405
REG_FRAME = 0x406
REG_SEQ = 0x407
REG_N0 = 0x408
REG_LLR_SCALE = 0x409
REG_LEARNED_FROM = 0x40A
REG_CONST_MASK = 0x40B
REG_MAX_ITER = 0x40C
REG_AGC = 0x40D
REG_TAPS = 0x410
REG_TAPS_FROM = 0x41B
REG_SUMS_FRAME = 0x420
REG_SUMS_COUNT = 0x421
REG_SUMS_CAPTURE = 0x422
REG_COUNTERS = 0x430
REG_TMCC_OVERRUNS = 0x438
REG_SUMS_R = 0x500
REG_SUMS_P = 0x610
SLOTS_VERSION = 1  # the Slots format (isdb_frontend::slots::SLOTS_VERSION)
# TAPS (the host writes taps), SUMS (the training sums readable), bit 4
# SUMS_CAPTURE (the sums held from a host-armed frame end)
FEATURES = 0b10011
LAYOUT_LAG = 2
EQ_APPLY_FRAMES = 2
NONE = 0xFFFFFFFF


class SlotsRx(Elaboratable):
    """The bitstream block: the front end (`SymbolFrontend` and its
    `Control`), a 1 024-symbol FIFO, `SlotsChain`, the decoder bridge
    (`ldpc_if.LdpcBridge`), the LDPC decoder (`isdb_fpga.ldpc.LdpcDecoder`,
    S = 34, K = 3, every rate; `decoder=False` leaves its ports as the
    block's ports), the output selection (raw IQ, `Symbols`, `Slots`) and
    the chain's registers (the module docstring's map).

    Clocks (fpga/tezuka/isdb_slots.xdc): `sync` the symbol clock
    (`util_ad9361_divclk`, l_clk / 4), `fast` its 4x (`axi_ad9361` l_clk
    itself), `ldpc` the decoder's 200 MHz: with `mmcm`, an MMCME2_BASE on
    `ldpc_ref` (the PS's clk_fpga_0, 100 MHz) inside the block, its reset
    held until the MMCM locks; without (the simulations), the `ldpc`
    domain from outside."""

    def __init__(self, taps, cos_table, h=None, g=None, k=None, decoder=True, mmcm=False, units=34):
        from ..frontend import SymbolFrontend
        from .ldpc_if import LdpcBridge

        self.core = SymbolFrontend(taps, cos_table, h, g, k)
        # MAX_ITER's reset: the decoder's default
        self.chain = SlotsChain(max_iter=ldpc.DEFAULT_MAX_ITER)
        self.bridge = LdpcBridge(self.chain.port)
        self.decoder = None
        if decoder:
            from ..ldpc import LdpcDecoder

            self.decoder = LdpcDecoder(units)
        self.mmcm = mmcm
        self.units = units  # the decoder's step (the one built, or the one outside)
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
        self.ldpc_ref = Signal()  # with mmcm: the MMCM's input clock
        self.ldpc_locked = Signal()

    def ports(self):
        out = [self.adc_valid, self.adc_i, self.adc_q, self.pass_valid, self.pass_data0, self.pass_data1,
               self.pack_valid, self.pack_data0, self.pack_data1, self.gpio_out, self.gpio_in]
        if self.mmcm:
            out += [self.ldpc_ref, self.ldpc_locked]
        if self.decoder is None:
            b = self.bridge
            out += [b.begin, b.d_ctx, b.d_valid, b.d_pos, b.d_llr, b.d_rate, b.start, b.st_rate,
                    b.st_max_iter, b.st_offset, b.st_short, b.load_ready, b.o_valid, b.o_data, b.o_last,
                    b.o_ready, b.res_valid, b.res_iterations, b.res_converged, b.res_unsat, b.res_overrun]
        return out

    def elaborate(self, platform):
        from amaranth.hdl import ClockDomain, DomainRenamer, Instance
        from amaranth.lib.cdc import ResetSynchronizer
        from amaranth.lib.fifo import SyncFIFOBuffered

        from ..control import ID_MAGIC, OP_ADDR, OP_CTRL, OP_HI, OP_READ, OP_WRITE, OP_WRITE_S, STATUS, Control
        from ..frontend import FORMAT_VERSION
        from .ldpc_if import connect_decoder

        m = Module()
        m.submodules.core = core = self.core
        m.submodules.control = ctl = Control(core)
        m.submodules.chain = ch = self.chain
        m.submodules.bridge = br = self.bridge
        m.submodules.fifo = fifo = SyncFIFOBuffered(width=12, depth=1024)
        if self.decoder is not None:
            m.submodules.ldpc = DomainRenamer("ldpc")(self.decoder)
            connect_decoder(m, br, self.decoder)
        if self.mmcm:
            # 100 MHz x 10 / 5 = 200 MHz (VCO 1 GHz)
            m.domains.ldpc = cd = ClockDomain("ldpc")
            fb = Signal()
            c0 = Signal()
            m.submodules.mmcm = Instance(
                "MMCME2_BASE", p_CLKIN1_PERIOD=10.0, p_CLKFBOUT_MULT_F=10.0, p_DIVCLK_DIVIDE=1,
                p_CLKOUT0_DIVIDE_F=5.0, i_CLKIN1=self.ldpc_ref, i_CLKFBIN=fb, o_CLKFBOUT=fb,
                o_CLKOUT0=c0, o_LOCKED=self.ldpc_locked, i_RST=0, i_PWRDWN=0)
            m.submodules.ldpc_bufg = Instance("BUFG", i_I=c0, o_O=cd.clk)
            m.submodules.ldpc_rst = ResetSynchronizer(~self.ldpc_locked, domain="ldpc")
        # the front end's symbols (the quantiser's 6-bit k output) into the chain
        slots_on = Signal()
        restart = Signal()  # CTRL START: the chain restarts (`Chain::loss` with 0 symbols)
        m.d.comb += [
            core.i_valid.eq(self.adc_valid & ctl.run),
            core.i_i.eq(self.adc_i), core.i_q.eq(self.adc_q),
            fifo.w_en.eq(core.s_valid & slots_on),
            fifo.w_data.eq(Cat(core.s_q[0][:6], core.s_q[1][:6])),
            ch.i_valid.eq(Mux(restart, 1, fifo.r_rdy)), fifo.r_en.eq(ch.i_ready & ~restart),
            ch.i_loss.eq(restart), ch.i_lost.eq(0),
            ch.i_x[0].eq(fifo.r_data[:6]), ch.i_x[1].eq(fifo.r_data[6:]),
            ch.r_overrun.eq(br.out_overrun),
        ]
        with m.If(restart & ch.i_ready):
            m.d.sync += restart.eq(0)
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
        value = Signal(32)
        m.d.comb += value.eq(Mux(op == OP_WRITE_S, arg.as_signed(), Cat(arg[:16], staged)))
        readback = Signal(32)
        show = Signal()
        fe_status = Signal()  # the last READ was the front end's STATUS
        eqm = ch.back.eq
        fin = ch.finish
        dm = ch.back.demap
        nco = ch.front.nco
        carrier = Signal(32)
        init = Signal(3)
        m.d.comb += [eqm.tap_we.eq(0), eqm.tap_now.eq(0), eqm.tap_arm.eq(0), eqm.cap_strobe.eq(0), nco.f_load.eq(0),
                     nco.f_in.eq(carrier)]
        # TAPS_FROM's value arms the equaliser in the cycle it is written
        # (the model's rule: taps solved from frame N in use from N + 2)
        m.d.comb += [eqm.tap_at.eq(value + EQ_APPLY_FRAMES), eqm.tap_from_in.eq(value)]
        # STATUS: locked, a frame head seen since the (re)start, frames decoded
        framed = Signal()
        decoding = Signal()
        sy = ch.front.sync
        with m.If(ch.front.o_restart):
            m.d.sync += framed.eq(0)
        with m.Elif(sy.o_head_valid & sy.o_head_numbered):
            m.d.sync += framed.eq(1)
        fc = ch.back.fc
        with m.If(fc.o_done | fc.o_dropped):
            m.d.sync += decoding.eq(fc.o_done & fc.o_has_layout)
        last_frame = Signal(32, init=NONE)
        with m.If(fin.f_valid):
            m.d.sync += last_frame.eq(fin.f_frame)
        chain_addr = Signal()
        m.d.comb += chain_addr.eq((addr >= REG_BASE) & (addr < REG_END))
        sums_r = (addr >= REG_SUMS_R) & (addr < REG_SUMS_R + 4 * 66)
        sums_p = (addr >= REG_SUMS_P) & (addr < REG_SUMS_P + 4 * 11)
        sums_w = Mux(sums_r, addr - REG_SUMS_R, addr - REG_SUMS_P + 4 * 66)[:9]
        is_tap = (addr >= REG_TAPS) & (addr < REG_TAPS + 11)
        with m.If(go):
            m.d.sync += show.eq((op == OP_READ) & chain_addr)
            m.d.sync += fe_status.eq((op == OP_READ) & (addr == STATUS))
            with m.Switch(op):
                with m.Case(OP_ADDR):
                    m.d.sync += addr.eq(arg[:16])
                with m.Case(OP_HI):
                    m.d.sync += staged.eq(arg[:16])
                with m.Case(OP_WRITE, OP_WRITE_S):
                    m.d.sync += addr.eq(addr + 1)
                    with m.Switch(addr):
                        with m.Case(REG_CARRIER):
                            m.d.sync += carrier.eq(value)
                        with m.Case(REG_INIT):
                            m.d.sync += init.eq(value[:3])
                        with m.Case(REG_MAX_ITER):
                            m.d.sync += ch.back.max_iter.eq(value[:8])
                        with m.Case(REG_AGC):
                            m.d.sync += ch.agc.eq(value[:16])
                        with m.Case(REG_TAPS_FROM):
                            m.d.comb += eqm.tap_arm.eq(1)
                        with m.Case(REG_SUMS_CAPTURE):
                            m.d.comb += eqm.cap_strobe.eq(1)
                    with m.If(is_tap):
                        m.d.comb += eqm.tap_we.eq(1)
                with m.Case(OP_READ):
                    with m.Switch(addr):
                        regs = {
                            REG_VERSION: SLOTS_VERSION,
                            REG_FEATURES: Cat(Const(FEATURES, 8), ch.back.max_iter, Const(self.units, 12)),
                            REG_CARRIER: nco.f,
                            REG_LAYOUT_LAG: LAYOUT_LAG,
                            REG_INIT: init,
                            REG_STATUS: Cat(sy.st_locked, framed, decoding),
                            REG_FRAME: last_frame,
                            REG_SEQ: fin.packer.seq,
                            REG_N0: dm.n0,
                            REG_LLR_SCALE: dm.kk,
                            REG_LEARNED_FROM: dm.learned_from,
                            REG_CONST_MASK: dm.learned_valid,
                            REG_MAX_ITER: ch.back.max_iter,
                            REG_AGC: ch.agc,
                            REG_TAPS_FROM: eqm.taps_from,
                            REG_SUMS_FRAME: eqm.sums_frame,
                            REG_SUMS_COUNT: eqm.sums_count,
                            REG_SUMS_CAPTURE: Cat(eqm.cap_valid, eqm.cap_armed),
                            REG_TMCC_OVERRUNS: fin.tmcc_overruns,
                        }
                        for i, n in enumerate(("frames", "undecoded", "tmcc_failed", "slots", "ldpc_failed",
                                               "bch_failed")):
                            regs[REG_COUNTERS + i] = fin.counters[n]
                        regs[REG_COUNTERS + 6] = fin.iterations[:32]
                        regs[REG_COUNTERS + 7] = fin.iterations[32:]
                        for k in range(11):
                            regs[REG_TAPS + k] = Cat(eqm.taps[k][0], eqm.taps[k][1])
                        for a, v in regs.items():
                            with m.Case(a):
                                m.d.sync += readback.eq(v)
                        with m.Default():
                            m.d.sync += readback.eq(0)
                with m.Case(OP_CTRL):
                    m.d.sync += slots_on.eq(arg[3])
                    with m.If(arg[1]):
                        # START: the chain restarts, the host's carrier and
                        # (INIT bit 0) taps loaded, the Slots sequence from 0
                        m.d.sync += restart.eq(1)
                        m.d.comb += [nco.f_load.eq(1), eqm.tap_now.eq(init[0]), fin.packer.start.eq(1)]
        m.d.comb += [eqm.tap_idx.eq((addr - REG_TAPS)[:4]), eqm.tap_val[0].eq(value[:16]),
                     eqm.tap_val[1].eq(value[16:])]
        # the training sums' readback: requested at READ, shown when done
        m.d.comb += [eqm.rd_req.eq(go & (op == OP_READ) & (sums_r | sums_p)), eqm.rd_addr.eq(sums_w)]
        with m.If(eqm.rd_ack):
            m.d.sync += readback.eq(eqm.rd_data)
        # the chain's readback, else the front end's (its own READs), whose
        # ID word carries the Slots format version in bits 11-8 and whose
        # STATUS bit 2 says the Slots words go to the DMA
        ident = ID_MAGIC << 16 | FORMAT_VERSION << 12 | (self.taps & 0xFF)
        fe = ctl.gpio_in
        m.d.comb += self.gpio_in.eq(Mux(show, readback,
                                        Mux(fe == ident, ident | ID_CHAIN,
                                            Mux(fe_status, fe | (slots_on << 2), fe))))
        # the output selection: the chain's Slots words, else the front end's block
        m.d.comb += [
            ctl.gpio_out.eq(self.gpio_out),
            self.pack_valid.eq(Mux(slots_on, ch.o_valid, Mux(ctl.symbols, core.o_valid, self.pass_valid))),
            self.pack_data0.eq(Mux(slots_on, ch.o_word, Mux(ctl.symbols, core.o_word, self.pass_data0))),
            self.pack_data1.eq(Mux(slots_on | ctl.symbols, 0, self.pass_data1)),
        ]
        return m
