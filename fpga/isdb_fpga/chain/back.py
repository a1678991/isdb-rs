"""The chain's back end before the decoder: stage 4 (`Equaliser`) with the
frame bookkeeping (`FrameCtl`) at its centre, and on its output stage 5's
input side (`TmccSoft`) and stage 6 (`Demap`), both writing the LDPC
decoder's input port (`ldpc_if.py`) through one arbiter.

Routing: FrameCtl decides at the FIR's centre cycle (it needs the centre's
meta, and the training needs its answer then); its decision for the
symbol (active, layout, part, indices, the slot's modulation, the frame
events) travels with the symbol through the FIR's pipeline (`aux`) and
reaches TMCC and the demapper with its z. Frame start, end and drop are
events in the symbol stream: start with the frame's first symbol, end and
drop after the last (the demapper delays its end by 12 cycles, so the
noise samples in its pipeline are in).

The decoder port: the demapper's beat (one per main symbol of a valid
codeword) has priority; the TMCC burst takes the other cycles (`grant`).
Both wait for the decoder's `in_ready` (back-pressure from its input FIFO;
the demapper cannot wait: a full FIFO there is counted as `port_lost`).

The abort (issue #28): a frame dropped or a restart (`f_drop` of the
demapper) gives up the slot codeword it was loading, whose last beat never
comes; the decoder would keep its bank. After every `f_drop` the port
carries one OP_ABORT beat on context 0 (`ldpc.py` "Abandoned codewords":
the bank is freed), once the demapper's pipeline has emptied (ABORT_WAIT
cycles: the beats of symbols that arrived before the drop are written
first) and on a cycle neither the demapper nor the TMCC burst uses (the
burst never waits for it). A new codeword's first beat after that wait
and before the abort cancels it: that `begin` takes the old bank back
instead. No beat, codeword or
result of the chain's moves: the abort only frees a bank.
"""

from amaranth.hdl import Cat, Elaboratable, Module, Mux, Signal

from .demap import Demap
from .eq import Equaliser
from .frame import FrameCtl
from .ldpc_if import OP_ABORT, OP_WRITE, LdpcPort
from .nominal import NominalRom
from .tmcc import TmccSoft

AUX = [("active", 1), ("lay", 1), ("main", 1), ("pilot", 1), ("tmcc", 1), ("t", 5), ("ti", 15),
       ("k", 7), ("smod", 4), ("start", 1), ("done", 1), ("drop", 1), ("frame", 32), ("odd", 1)]
AUX_BITS = sum(w for _, w in AUX)
# cycles from the demapper's `f_drop` to its abort beat: the demapper's
# beat leaves 10 cycles after its symbol's arrival (A + 9, registered), and
# no symbol arrives after the drop
ABORT_WAIT = 12


def aux_field(sig, name):
    o = 0
    for n, w in AUX:
        if n == name:
            return sig[o:o + w]
        o += w
    raise KeyError(name)


class Back(Elaboratable):
    def __init__(self, max_iter=50, offset=1):
        # one nominal-point ROM for the equaliser's references and the
        # demapper's points (two read ports of one RAMB18)
        self.nom = NominalRom(2)
        self.eq = Equaliser(aux=AUX_BITS, nominal=self.nom.ports[0])
        self.fc = FrameCtl()
        self.tmcc = TmccSoft(max_iter, offset)
        self.demap = Demap(nominal=self.nom.ports[1])
        self.port = LdpcPort()
        # the decoder's per-codeword settings (registers: SlotsRx MAX_ITER)
        self.max_iter = Signal(8, init=max_iter)
        self.offset = Signal(3, init=offset)
        self.restart = Signal()
        self.port_lost = Signal(16)
        self.aborts = Signal(16)  # abort beats sent (verification)

    def elaborate(self, platform):
        m = Module()
        m.submodules.nom = self.nom
        m.submodules.eq = eq = self.eq
        m.submodules.fc = fc = self.fc
        m.submodules.tmcc = tm = self.tmcc
        m.submodules.demap = dm = self.demap
        from .stages import wire_eq_frame

        wire_eq_frame(m, eq, fc)
        cm = eq.c_meta
        m.d.comb += [
            eq.restart.eq(self.restart), fc.restart.eq(self.restart),
            eq.c_aux.eq(Cat(
                fc.active, fc.has_layout, fc.is_main, fc.is_pilot, fc.is_tmcc, fc.t, fc.ti,
                fc.c_number, fc.s_mod, fc.start & ~self.restart, fc.done_now, fc.drop_now,
                fc.c_frame, eq.meta_field(cm, "j")[0],
            )),
        ]
        a = eq.o_aux
        f = lambda n: aux_field(a, n)  # noqa: E731
        ov = eq.o_valid
        act = f("active")
        lay = f("lay")
        # the frame events at the output
        end_d = Signal(12)
        m.d.sync += end_d.eq(Cat(ov & f("done") & lay, end_d[:-1]))
        m.d.comb += [
            tm.t_valid.eq(ov & act & f("tmcc")),
            tm.t_z[0].eq(eq.o_z[0]), tm.t_z[1].eq(eq.o_z[1]),
            tm.t_odd.eq(f("odd")), tm.t_idx.eq(f("ti")),
            tm.frame_done.eq(ov & f("done")), tm.frame.eq(f("frame")),
            dm.valid.eq(ov), dm.z[0].eq(eq.o_z[0]), dm.z[1].eq(eq.o_z[1]),
            dm.is_main.eq(act & lay & f("main")), dm.is_pilot.eq(act & lay & f("pilot")),
            dm.k.eq(f("k")), dm.t.eq(f("t")), dm.slot_mod.eq(f("smod")), dm.frame.eq(f("frame")),
            dm.f_start.eq(ov & f("start") & lay),
            dm.f_swap.eq(ov & f("done") & lay),
            dm.f_end.eq(end_d[-1]),
            dm.f_drop.eq((ov & f("drop")) | self.restart),
            fc.r2_addr.eq(dm.lay_addr), dm.lay_data.eq(fc.r2_data),
        ]
        # the decoder port
        p = self.port
        use_dm = dm.o_valid
        # the abort beat after a drop (docstring)
        ab_pend = Signal()
        ab_wait = Signal(range(ABORT_WAIT + 1))
        ab_go = Signal()
        m.d.comb += ab_go.eq(ab_pend & (ab_wait == 0) & ~use_dm & ~tm.b_valid & p.in_ready)
        with m.If(dm.f_drop):
            m.d.sync += [ab_pend.eq(1), ab_wait.eq(ABORT_WAIT)]
        with m.Elif(ab_wait != 0):
            # the dropped codeword's last beats (a first one among them too)
            m.d.sync += ab_wait.eq(ab_wait - 1)
        with m.Elif(use_dm & dm.o_first):
            # a new codeword began first: its `begin` takes the bank back
            m.d.sync += ab_pend.eq(0)
        with m.Elif(ab_go):
            m.d.sync += [ab_pend.eq(0), self.aborts.eq(self.aborts + 1)]
        m.d.comb += [
            p.in_valid.eq(use_dm | tm.b_valid | ab_go),
            tm.grant.eq(~use_dm & tm.b_valid & p.in_ready),
            p.in_ctx.eq(~use_dm & ~ab_go),
            p.in_op.eq(Mux(use_dm, OP_WRITE, Mux(ab_go, OP_ABORT, tm.b_op))),
            p.in_n.eq(Mux(use_dm, dm.o_n, tm.b_n)),
            p.in_first.eq(use_dm & dm.o_first),
            p.in_last.eq(use_dm & dm.o_last),
            p.in_tag.eq(Mux(use_dm, dm.o_tag, tm.b_tag)),
            p.in_rate.eq(Mux(use_dm, dm.o_rate, 3)),
            p.in_max_iter.eq(self.max_iter),
            p.in_offset.eq(self.offset),
            p.in_shortened.eq(~use_dm),
        ]
        for k in range(5):
            m.d.comb += [
                p.in_pos[k].eq(Mux(use_dm, dm.o_pos[k], tm.b_pos[k] if k < len(tm.b_pos) else 0)),
                p.in_llr[k].eq(Mux(use_dm, dm.o_llr[k], tm.b_llr[k] if k < len(tm.b_llr) else 0)),
            ]
        with m.If(use_dm & ~p.in_ready):
            m.d.sync += self.port_lost.eq(self.port_lost + 1)
        return m
