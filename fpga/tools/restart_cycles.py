"""Issue #28's stop/START recipe on the chain with the real decoder
(`stages.run_chain_restarts`, chain-7-hdl): the input to inside frame 2's
first codeword, the host's stop and START, CYCLES times (default 4), then
the whole input; prints each START's decoder and bridge state (banks held,
FIFO levels, aborts) and the last pass against the model. The opt-in test
is `RealDecoderChainTest.test_stop_and_start_free_the_decoder`.

python tools/restart_cycles.py [CYCLES]      # from fpga/, after `mise run fpga:vectors`
"""

import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from isdb_fpga.chain.sim import load  # noqa: E402
from isdb_fpga.chain.stages import run_chain_restarts  # noqa: E402

BUILD = os.path.join(os.path.dirname(__file__), "..", "build")


def main(argv):
    cycles = int(argv[1]) if len(argv) > 1 else 4
    doc = load(os.path.join(BUILD, "chain-7-hdl.json"))
    taps = load(os.path.join(BUILD, "chain-3-eq.json"))["taps"]
    t0 = time.time()
    what, bad = run_chain_restarts(doc, taps, None, cycles=cycles)
    print(what)
    for b in bad:
        print("  " + b)
    print(f"{'ok' if not bad else f'{len(bad)} problems'} in {time.time() - t0:.0f} s")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
