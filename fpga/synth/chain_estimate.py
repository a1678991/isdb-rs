"""Yosys `synth_xilinx` resource estimates of the ISDB-S3 chain's stages
(isdb_fpga/chain), for the budget of docs/plans/isdbs3-fpga-rx.md ("Budget
for the whole chain"). Before place and route; Vivado will differ.

python synth/chain_estimate.py [STAGE ...]     # default: every stage

Stages: sync (SyncFront: NCO, slot sync, carrier loop), phase (SlotPhase),
eq (Equaliser + FrameCtl), tmcc (TmccSoft), demap (Demap), finish (Finish:
TmccResult, the BCH unit, the PRBS, the packer, the status source); chain
(SlotsChain, all of them) only when named: abc9 in YoWASP's 32-bit
WebAssembly runs out of memory on it. Writes
build/chain_<stage>.il / .stat and prints one line per stage.
"""

import os
import subprocess
import sys

from amaranth.back import rtlil

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

BUDGET = {
    # plan rows: (LUT, BRAM36, DSP)
    "sync + phase": (3000, 9, 8),
    "eq": (2500, 1, 44),
    "tmcc + demap": (3500, 2, 64),
    "finish + packer": (3000, 4, 0),
}


def blocks():
    from amaranth.hdl import Elaboratable, Module

    from isdb_fpga.chain.eq import Equaliser
    from isdb_fpga.chain.frame import FrameCtl
    from isdb_fpga.chain.phase import SlotPhase
    from isdb_fpga.chain.stages import wire_eq_frame
    from isdb_fpga.chain.sync import SyncFront

    class EqFrame(Elaboratable):
        def __init__(self):
            self.eq = Equaliser()
            self.fc = FrameCtl()

        def elaborate(self, platform):
            m = Module()
            m.submodules.eq = self.eq
            m.submodules.fc = self.fc
            wire_eq_frame(m, self.eq, self.fc)
            return m

        def ports(self):
            e, f = self.eq, self.fc
            return [e.i_valid, *e.i_y, e.i_meta, e.restart, e.o_valid, *e.o_z, e.o_meta,
                    e.tap_we, e.tap_idx, *e.tap_val, e.tap_now, e.tap_arm, e.tap_at, e.tap_from_in,
                    e.rd_req, e.rd_addr, e.rd_data, e.rd_ack, e.sums_frame, e.sums_count,
                    f.lw_en, f.lw_slot, f.lw_data, f.lw_commit, f.lw_frame, f.lw_from, f.restart,
                    f.o_done, f.o_dropped, f.o_frame, f.active, f.is_main, f.is_tmcc, f.mi, f.ti,
                    f.s_mod, f.s_rate, f.s_start, f.s_symbols, f.s_dummy, f.s_mode]

    def sync():
        d = SyncFront(6, detail=False)
        s, n = d.sync, d.nco
        ports = [d.i_valid, d.i_ready, *d.i_x, d.i_loss, d.i_lost, n.o_valid, *n.o_r, n.o_phase,
                 n.o_at, n.f, n.o_f_valid, d.o_restart, s.o_head_valid, s.o_head_late,
                 s.o_head_at, s.o_head_word, *s.o_head_corr, s.o_head_theta, s.o_head_amp,
                 s.o_head_numbered, s.o_head_number, s.o_head_frame, s.st_peak, s.st_pos,
                 s.st_sum, s.st_locked, s.st_offset, s.st_parity, s.st_stray, s.st_missed]
        return d, ports

    def phase():
        d = SlotPhase(6)
        ports = [d.i_valid, d.i_at, *d.i_r, d.full, d.h_valid, d.h_late, d.h_at, d.h_word,
                 d.h_theta, d.h_amp, d.h_numbered, d.h_number, d.h_frame, d.restart, d.o_ready,
                 d.o_valid, d.o_at, d.o_j, *d.o_y, d.o_word, d.o_numbered, d.o_number, d.o_frame,
                 d.o_restart]
        return d, ports

    def eq():
        d = EqFrame()
        return d, d.ports()

    def tmcc():
        from isdb_fpga.chain.tmcc import TmccSoft

        d = TmccSoft()
        return d, [d.t_valid, *d.t_z, d.t_odd, d.t_idx, d.frame_done, d.frame, d.b_valid, d.b_op, d.b_pos,
                   d.b_llr, d.b_tag, d.grant, d.overrun]

    def demap():
        from isdb_fpga.chain.demap import Demap

        d = Demap()
        return d, [d.valid, *d.z, d.is_main, d.is_pilot, d.k, d.t, d.slot_mod, d.frame, d.f_start, d.f_swap,
                   d.f_end, d.f_drop, d.lay_addr, d.lay_data, d.o_valid, d.o_n, *d.o_pos, *d.o_llr, d.o_first,
                   d.o_last, d.o_tag, d.o_rate, d.n0, d.kk, d.learned_from, d.learned_valid, d.st_prev,
                   d.st_addr, d.st_data, d.late]

    def finish():
        from isdb_fpga.chain.finish import Finish

        d = Finish()
        st = d.status
        return d, [d.r_valid, d.r_ready, d.r_head, d.r_tag, d.r_iterations, d.r_converged, d.r_overrun,
                   d.r_bits, d.r_last, d.f_valid, d.f_frame, d.f_decoded, d.o_valid, d.o_word,
                   st.pt_addr, st.pt_data, *st.snap.values(),
                   *[getattr(d.tmcc.layout, n) for n in ("lw_open", "lw_en", "lw_slot", "lw_data", "lw_commit",
                                                        "lw_frame", "lw_from")]]

    def chain():
        from isdb_fpga.chain.top import SlotsChain

        d = SlotsChain()
        p = d.port
        return d, [d.i_valid, d.i_ready, *d.i_x, d.i_loss, d.i_lost, d.agc, d.r_overrun, d.o_valid, d.o_word,
                   p.in_valid, p.in_ready, p.in_op, p.in_ctx, p.in_n, *p.in_pos, *p.in_llr, p.in_first,
                   p.in_last, p.in_rate, p.in_tag, p.out_valid, p.out_ready, p.out_head, p.out_tag,
                   p.out_iterations, p.out_converged, p.out_bits, p.out_last]

    return {"sync": sync, "phase": phase, "eq": eq, "tmcc": tmcc, "demap": demap, "finish": finish,
            "chain": chain}


def stat(dut, ports, tag):
    out_dir = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out_dir, exist_ok=True)
    il = os.path.join(out_dir, f"chain_{tag}.il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name=f"chain_{tag}", ports=ports))
    st = os.path.join(out_dir, f"chain_{tag}.stat")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top chain_{tag}; tee -q -o {st} stat"
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode != 0:
        print(res.stdout[-2000:] + res.stderr[-2000:])
        return None
    with open(st) as f:
        log = f.read()
    total = log.split("design hierarchy")[-1]
    counts = {}
    for line in total.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0].isdigit():
            counts[parts[1]] = int(parts[0])
    luts = sum(v for k, v in counts.items() if k.startswith("LUT"))
    lram = sum(v for k, v in counts.items() if k.startswith("RAM") and not k.startswith("RAMB"))
    ff = sum(v for k, v in counts.items() if k.startswith("FD"))
    b36 = counts.get("RAMB36E1", 0) + counts.get("RAMB18E1", 0) / 2
    return {"LUT": luts, "FF": ff, "BRAM36": b36, "DSP": counts.get("DSP48E1", 0),
            "LUTRAM": lram, "CARRY4": counts.get("CARRY4", 0)}


def main(argv):
    # `chain` (the whole SlotsChain) is not in the default list: YoWASP
    # Yosys's abc9 runs out of the 4 GB WebAssembly memory on it
    names = argv[1:] or [n for n in blocks() if n != "chain"]
    bl = blocks()
    for n in names:
        dut, ports = bl[n]()
        r = stat(dut, ports, n)
        if r is None:
            print(f"{n}: synthesis failed")
            continue
        print(f"{n}: {r['LUT']} LUT, {r['FF']} FF, {r['BRAM36']:g} BRAM36 (RAMB18 as 1/2), "
              f"{r['DSP']} DSP48E1, {r['LUTRAM']} LUTRAM cells, {r['CARRY4']} CARRY4", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
