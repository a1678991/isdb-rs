#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# This script imports GNU Radio and gr-isdbt (both GPL-3.0) at run time; the
# combination that runs is therefore covered by GPL-3.0. Neither is part of
# isdb-rs. See oracle/README.md.
"""Independent-oracle ISDB-T receiver: gr-isdbt decodes a cf32 IQ file to TS.

Oracle only (README "How it is verified", rung 4): used to cross-check the
Rust modulator.  A disagreement is settled by reading ARIB STD-B31, never by
adopting this chain's behaviour.  Run inside the oracle container
(oracle/Dockerfile):

  python3 grisdbt_rx.py --iq x.cf32 --layer b --out b.ts \
      --mode 3 --guard 8 --a 1,4,1,2 --b 12,64,2,2      # segs,const,cr_idx,I

const: 4 (QPSK) / 16 / 64; cr_idx: 0=1/2 1=2/3 2=3/4 3=5/6 4=7/8; I = the
time-interleaving length (not the TMCC index).
"""
import argparse
import os

from gnuradio import blocks, gr
from gnuradio import filter as grfilter
from gnuradio.fft import window
from gnuradio.filter import firdes
import gnuradio.isdbt as isdbt


def layer(s):
    if not s:
        return (0, 4, 0, 0)
    return tuple(int(x) for x in s.split(","))


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--iq", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--layer", choices="abc", required=True)
    p.add_argument("--mode", type=int, default=3)
    p.add_argument("--guard", type=int, default=8, help="GI denominator")
    p.add_argument("--oneseg", action="store_true", help="partial reception")
    p.add_argument("--a", default="", help="segs,const,cr,I")
    p.add_argument("--b", default="")
    p.add_argument("--c", default="")
    p.add_argument("--print-tmcc", action="store_true")
    p.add_argument("--no-lpf", action="store_true")
    a = p.parse_args()
    la, lb, lc = layer(a.a), layer(a.b), layer(a.c)
    fs = 8e6 * 64 / 63

    tb = gr.top_block()
    src = blocks.file_source(gr.sizeof_gr_complex, a.iq, False)
    sync = isdbt.ofdm_synchronization(a.mode, 1.0 / a.guard, True)
    if a.no_lpf:
        tb.connect(src, sync)
    else:
        lpf = grfilter.fir_filter_ccf(1, firdes.low_pass(1, fs, 5.8e6 / 2, 0.5e6, window.WIN_HAMMING, 6.76))
        tb.connect(src, lpf, sync)
    tmd = isdbt.tmcc_decoder(a.mode, a.print_tmcc)
    fdi = isdbt.frequency_deinterleaver(a.oneseg, a.mode)
    tdi = isdbt.time_deinterleaver(a.mode, la[0], la[3], lb[0], lb[3], lc[0], lc[3])
    sdm = isdbt.symbol_demapper(a.mode, la[0], la[1], lb[0], lb[1], lc[0], lc[1])
    tb.connect(sync, tmd, fdi, tdi, sdm)
    sel = {"a": (0, la), "b": (1, lb), "c": (2, lc)}[a.layer]
    carriers = 96 * 2 ** (a.mode - 1)
    for idx, lay in ((0, la), (1, lb), (2, lc)):
        if idx != sel[0] and lay[0]:
            tb.connect((sdm, idx), blocks.null_sink(gr.sizeof_char * lay[0] * carriers))
    segs, const, cr, _ = sel[1]
    bdi = isdbt.bit_deinterleaver(a.mode, segs, const)
    vit = isdbt.viterbi_decoder(const, cr)
    sink = blocks.file_sink(gr.sizeof_char, a.out, False)
    tb.connect((sdm, sel[0]), bdi, vit, isdbt.byte_deinterleaver(), isdbt.energy_descrambler(),
               isdbt.reed_solomon_dec_isdbt(), blocks.vector_to_stream(gr.sizeof_char, 188), sink)
    tb.run()
    size = os.path.getsize(a.out) if os.path.exists(a.out) else 0
    print(f"oracle rx: layer {a.layer} -> {a.out}: {size} bytes ({size // 188} TSPs)")


if __name__ == "__main__":
    main()
