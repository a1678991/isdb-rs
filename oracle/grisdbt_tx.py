#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# This script imports GNU Radio and gr-isdbt (both GPL-3.0) at run time; the
# combination that runs is therefore covered by GPL-3.0. Neither is part of
# isdb-rs. See oracle/README.md.
"""Independent-oracle ISDB-T modulator: gr-isdbt turns per-layer TS into cf32 IQ.

Oracle only (README "How it is verified", rung 4): used to cross-check the
Rust demodulator.  Layer parameters as in grisdbt_rx.py (segs,const,cr_idx,I),
layers A, B, C (--a/--a-ts, --b/--b-ts, --c/--c-ts).  Run inside the oracle
container (oracle/Dockerfile).
The output length is set by the shortest layer input (no looping).

The skiphead of 2 data symbols after the frequency interleaver is the one in
gr-isdbt's own examples/tx_demo.grc, whose output a consumer USB tuner
decoded with PER 0 (VERIFICATION.md); it makes the OFDM frame start 2
symbols after the bit-interleaver output start (cf. B31 Part 2 Fig. 3-8).
"""
import argparse

from gnuradio import blocks, digital, dtv, fft, gr
from gnuradio.fft import window
import gnuradio.isdbt as isdbt

CR = {0: dtv.C1_2, 1: dtv.C2_3, 2: dtv.C3_4, 3: dtv.C5_6, 4: dtv.C7_8}
MOD = {4: dtv.MOD_QPSK, 16: dtv.MOD_16QAM, 64: dtv.MOD_64QAM}


def layer(s):
    return tuple(int(x) for x in s.split(",")) if s else (0, 4, 0, 0)


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--out", required=True)
    p.add_argument("--mode", type=int, default=3)
    p.add_argument("--guard", type=int, default=8)
    p.add_argument("--oneseg", action="store_true")
    p.add_argument("--a", required=True)
    p.add_argument("--b", default="")
    p.add_argument("--c", default="")
    p.add_argument("--a-ts", required=True)
    p.add_argument("--b-ts", default="")
    p.add_argument("--c-ts", default="")
    a = p.parse_args()
    la, lb, lc = layer(a.a), layer(a.b), layer(a.c)
    mode = a.mode
    tb = gr.top_block()
    hc = isdbt.hierarchical_combinator(mode, la[0], lb[0], lc[0])
    for i, (lay, path) in enumerate(((la, a.a_ts), (lb, a.b_ts), (lc, a.c_ts))):
        if not lay[0]:
            continue
        segs, const, cr, _ = lay
        src = blocks.file_source(gr.sizeof_char, path, False)
        ic = dtv.dvbt_inner_coder(1, 1512 * 4, MOD[const], dtv.ALPHA4, CR[cr])
        cm = isdbt.carrier_modulation(mode, segs, const)
        tb.connect(src, blocks.stream_to_vector(gr.sizeof_char, 188),
                   dtv.dvbt_reed_solomon_enc(2, 8, 0x11d, 255, 239, 8, 51, 1),
                   isdbt.energy_dispersal(mode, const, cr, segs),
                   isdbt.byte_interleaver(mode, const, cr, segs), ic,
                   blocks.vector_to_stream(gr.sizeof_char, 1512 * 4), cm, (hc, i))
    ti = isdbt.time_interleaver(mode, la[0], la[3], lb[0], lb[3], lc[0], lc[3])
    nfft = 2048 * 2 ** (mode - 1)
    data_carriers = 13 * 96 * 2 ** (mode - 1)
    tm = isdbt.tmcc_encoder(mode, a.oneseg, la[1], lb[1] or 4, lc[1] or 4,
                            la[2], lb[2], lc[2], la[3], lb[3], lc[3], la[0], lb[0], lc[0])
    tb.connect(hc, ti, isdbt.frequency_interleaver(a.oneseg, mode),
               blocks.skiphead(gr.sizeof_gr_complex * data_carriers, 2),
               isdbt.pilot_signals(mode), tm,
               fft.fft_vcc(nfft, False, window.rectangular(nfft), True, 1),
               digital.ofdm_cyclic_prefixer(nfft, nfft + nfft // a.guard, 0, ""),
               blocks.multiply_const_cc(1.0 / 64),
               blocks.file_sink(gr.sizeof_gr_complex, a.out, False))
    tb.run()
    print(f"oracle tx -> {a.out}")


if __name__ == "__main__":
    main()
