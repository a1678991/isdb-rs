"""Throwaway sketch (not merged): the ISDB-S transmit interpolator's datapath
for a Yosys `synth_xilinx` estimate. 4-bit symbol codes in (bit 3: zero
symbol, bits 2..0: angle index k, the point e^{jk*pi/4}), one 12-bit I/Q
sample per clock out. No DMA, no control beyond a coefficient write port.

python synth/tx_sketch.py MODE PHASES SPAN [linear]
  MODE mux: one ROM per tap pair (symmetry c[q][t] = c[P-1-q][2S-1-t]),
            2P entries (A rows for even k, B = A/sqrt2 rows for odd k),
            the point product a sign/zero mux, no multiplier;
  MODE mul: one ROM per tap (P entries of A), points as 16-bit integers,
            two multipliers per tap.
  linear:  (mux only) two rows q, q+1 per sample and mu*(y1 - y0) at the
            end (2 multipliers); rows 0..P (P+1) per tap, no symmetry.

Pipeline (one sample per clock, `frontend.py` style), mux:
  D0  acc += step (Q0.24); on carry the delay line shifts the next code in
  D1  phase q = acc[24-log2P:24], line snapshot, ROM address {k&1, q}
  D2  ROM data (BRAM, synchronous read)
  D3  terms: I, Q = (+-v | 0) by k        D4.. adder tree (ceil log2 2S)
  then round(>>S), saturate to 12 bits; output registered.
"""

import math
import os
import subprocess
import sys

from amaranth.back import rtlil
from amaranth.hdl import Cat, Const, Elaboratable, Module, Mux, Signal, signed, unsigned
from amaranth.lib.memory import Memory

NO_DSP = {"use_dsp": "no"}
ALPHA = 0.35
SPS = 40e6 / 28.86e6
FRAC = 24
BACKOFF_DB = 10.0


def rrc(t, a):
    if abs(t) < 1e-12:
        return 1 - a + 4 * a / math.pi
    x = 4 * a * t
    if abs(abs(x) - 1) < 1e-9:
        b = math.pi / (4 * a)
        return a / math.sqrt(2) * ((1 + 2 / math.pi) * math.sin(b) + (1 - 2 / math.pi) * math.cos(b))
    return (math.sin(math.pi * t * (1 - a)) + x * math.cos(math.pi * t * (1 + a))) / (math.pi * t * (1 - x * x))


def bank(P, span, half):
    """Integer rows (A, B): P (+1 for linear) rows of 2*span taps."""
    w = 2 * span
    pulse = lambda x: rrc(x, ALPHA) if abs(x) < span else 0.0  # noqa: E731
    rows = P + (0 if half else 1)
    pos = (lambda r: (r + 0.5) / P) if half else (lambda r: r / P)
    cf = [[pulse(pos(r) + t - span) for t in range(w)] for r in range(rows)]
    e = sum(pulse(r / P + t - span) ** 2 for r in range(P) for t in range(w)) / P
    g = e ** -0.5
    d = 2048 * 10 ** (-BACKOFF_DB / 20)
    maxc = max(abs(c) for r in cf for c in r) * g
    s = int(math.floor(math.log2(((1 << 17) - 1) / (maxc * d))))
    k = g * d * (1 << s)
    a = [[round(c * k) for c in r] for r in cf]
    b = [[round(c * k / math.sqrt(2)) for c in r] for r in cf]
    return a, b, s


class TxInterp(Elaboratable):
    def __init__(self, mode, P, span, linear=False):
        self.mode, self.P, self.span, self.linear = mode, P, span, linear
        self.pbits = P.bit_length() - 1
        self.step = Signal(FRAC)
        self.s_valid = Signal()  # a code is available (DMA FIFO not empty)
        self.s_code = Signal(4)
        self.s_ready = Signal()  # consumed this clock
        self.underrun = Signal()  # sticky
        self.o_i = Signal(signed(12))
        self.o_q = Signal(signed(12))
        self.c_we = Signal()
        self.c_addr = Signal(16)
        self.c_data = Signal(signed(18))
        self.c_sel = Signal(6)

    def ports(self):
        return [self.step, self.s_valid, self.s_code, self.s_ready, self.underrun,
                self.o_i, self.o_q, self.c_we, self.c_addr, self.c_data, self.c_sel]

    def elaborate(self, platform):
        m = Module()
        P, span, w, pb = self.P, self.span, 2 * self.span, self.pbits
        a, b, S = bank(P, span, half=not self.linear)

        # D0: phase accumulator and delay line of 4-bit codes
        acc = Signal(FRAC + 1, attrs=NO_DSP)
        line = [Signal(4, name=f"ln{t}", init=8) for t in range(w)]
        nxt = Signal(FRAC + 1, attrs=NO_DSP)
        m.d.comb += nxt.eq(acc[:FRAC] + self.step)
        m.d.sync += acc.eq(nxt)
        wrap = nxt[FRAC]
        m.d.comb += self.s_ready.eq(wrap & self.s_valid)
        with m.If(wrap):
            m.d.sync += [line[t].eq(line[t - 1]) for t in range(1, w)]
            m.d.sync += line[0].eq(Mux(self.s_valid, self.s_code, 8))
            with m.If(~self.s_valid):
                m.d.sync += self.underrun.eq(1)
        # D1: snapshot of phase, mu, codes
        q = Signal(pb)
        mu = Signal(8)
        snap = [Signal(4, name=f"sn{t}") for t in range(w)]
        m.d.sync += [q.eq(acc[FRAC - pb:FRAC]), mu.eq(acc[FRAC - pb - 8:FRAC - pb])]
        m.d.sync += [snap[t].eq(line[t]) for t in range(w)]

        def term(v, code):
            """(I, Q) of v * point(code) for the parity's value v (mux)."""
            k = code[:3]
            z = code[3]
            neg = Signal(signed(19))
            m.d.comb += neg.eq(-v)
            ti = Signal(signed(19))
            tq = Signal(signed(19))
            ci = [v, v, 0, neg, neg, neg, 0, v]   # sign of cos(k*45)
            cq = [0, v, v, v, 0, neg, neg, neg]   # sign of sin(k*45)
            with m.Switch(k):
                for kk in range(8):
                    with m.Case(kk):
                        m.d.comb += [ti.eq(ci[kk]), tq.eq(cq[kk])]
            ri = Signal(signed(19))
            rq = Signal(signed(19))
            m.d.sync += [ri.eq(Mux(z, 0, ti)), rq.eq(Mux(z, 0, tq))]
            return ri, rq

        def tree(leaves, name, width):
            cur = list(leaves)
            lvl = 0
            while len(cur) > 1:
                nx = []
                for i in range(0, len(cur), 2):
                    s = Signal(signed(width), name=f"{name}_l{lvl}_{i // 2}", attrs=NO_DSP)
                    m.d.sync += s.eq(cur[i] + (cur[i + 1] if i + 1 < len(cur) else 0))
                    nx.append(s)
                cur = nx
                lvl += 1
            return cur[0], lvl

        codes_d2 = [Signal(4, name=f"cd{t}") for t in range(w)]
        m.d.sync += [codes_d2[t].eq(snap[t]) for t in range(w)]
        leaves_i, leaves_q = [], []
        leaves1_i, leaves1_q = [], []
        shift = S
        if self.mode == "mux" and not self.linear:
            # one memory per tap pair (t, w-1-t): rows {B if odd, A} x q
            for t in range(span):
                u = w - 1 - t
                init = [x & 0x3FFFF for x in
                        [a[r][t] for r in range(P)] + [b[r][t] for r in range(P)]]
                mem = Memory(shape=unsigned(18), depth=2 * P, init=init)
                m.submodules[f"rom{t}"] = mem
                rp0 = mem.read_port()
                rp1 = mem.read_port()
                wp = mem.write_port()
                # port A is read/write: the load shares its address (a load
                # happens while the output is stopped)
                pa = Signal(pb + 1, name=f"pa{t}")
                m.d.comb += [
                    pa.eq(Mux(self.c_we, self.c_addr[:pb + 1], Cat(q, snap[t][0]))),
                    wp.en.eq(self.c_we & (self.c_sel == t)),
                    wp.addr.eq(pa), wp.data.eq(self.c_data),
                    rp0.addr.eq(pa),
                    rp1.addr.eq(Cat(~q, snap[u][0])),  # c[P-1-q][u] = c[q][t]
                ]
                for rp, tt in ((rp0, t), (rp1, u)):
                    ti, tq = term(rp.data.as_signed(), codes_d2[tt])
                    leaves_i.append(ti)
                    leaves_q.append(tq)
        elif self.mode == "mux":
            # linear: rows q and q+1 of P+1 rows per tap, one memory per tap
            # holding (row r, row r+1) side by side: 36-bit words, 2(P+1) deep
            for t in range(w):
                rowsA = a + [a[-1]]
                rowsB = b + [b[-1]]
                init = [((rowsA[r][t] & 0x3FFFF) | ((rowsA[r + 1][t] & 0x3FFFF) << 18)) for r in range(P)] + \
                       [((rowsB[r][t] & 0x3FFFF) | ((rowsB[r + 1][t] & 0x3FFFF) << 18)) for r in range(P)]
                mem = Memory(shape=unsigned(36), depth=2 * P, init=init)
                m.submodules[f"rom{t}"] = mem
                rp = mem.read_port()
                wp = mem.write_port()
                m.d.comb += [
                    wp.en.eq(self.c_we & (self.c_sel == t)),
                    wp.addr.eq(self.c_addr), wp.data.eq(Cat(self.c_data, self.c_data)),
                    rp.addr.eq(Cat(q, snap[t][0])),
                ]
                ti, tq = term(rp.data[:18].as_signed(), codes_d2[t])
                leaves_i.append(ti)
                leaves_q.append(tq)
                ti, tq = term(rp.data[18:].as_signed(), codes_d2[t])
                leaves1_i.append(ti)
                leaves1_q.append(tq)
        else:
            # multiply: one memory of P A-rows per tap, points as integers
            one, c = 16384, round(16384 / math.sqrt(2))
            pts = [(one, 0), (c, c), (0, one), (-c, c), (-one, 0), (-c, -c), (0, -one), (c, -c)]
            for t in range(w):
                init = [a[r][t] & 0x3FFFF for r in range(P)]
                mem = Memory(shape=unsigned(18), depth=P, init=init)
                m.submodules[f"rom{t}"] = mem
                rp = mem.read_port()
                wp = mem.write_port()
                m.d.comb += [
                    wp.en.eq(self.c_we & (self.c_sel == t)),
                    wp.addr.eq(self.c_addr), wp.data.eq(self.c_data),
                    rp.addr.eq(q),
                ]
                pi = Signal(signed(16), name=f"pi{t}")
                pq = Signal(signed(16), name=f"pq{t}")
                with m.Switch(codes_d2[t]):
                    for kk in range(8):
                        with m.Case(kk):
                            m.d.comb += [pi.eq(pts[kk][0]), pq.eq(pts[kk][1])]
                    with m.Default():
                        m.d.comb += [pi.eq(0), pq.eq(0)]
                ri = Signal(signed(34), name=f"mi{t}")
                rq = Signal(signed(34), name=f"mq{t}")
                m.d.sync += [ri.eq(rp.data.as_signed() * pi), rq.eq(rp.data.as_signed() * pq)]
                leaves_i.append(ri)
                leaves_q.append(rq)
            shift = S + 14
        width = 40 if self.mode == "mul" else 24
        si, lv = tree(leaves_i, "ti", width)
        sq, _ = tree(leaves_q, "tq", width)
        if self.linear:
            s1i, _ = tree(leaves1_i, "ui", width)
            s1q, _ = tree(leaves1_q, "uq", width)
            mu_d = Signal(8)
            # mu delayed to the tree's output (D1 -> D3 + levels)
            prev = mu
            for i in range(2 + lv):
                n = Signal(8, name=f"mu{i}")
                m.d.sync += n.eq(prev)
                prev = n
            m.d.comb += mu_d.eq(prev)
            mus = Cat(mu_d, Const(0, 1)).as_signed()
            outs = []
            for s0, s1, nm in ((si, s1i, "li"), (sq, s1q, "lq")):
                dif = Signal(signed(width), name=f"{nm}_d", attrs=NO_DSP)
                prod = Signal(signed(width + 9), name=f"{nm}_p")
                s0d = Signal(signed(width), name=f"{nm}_s0")
                s0dd = Signal(signed(width), name=f"{nm}_s0d")
                fin = Signal(signed(width + 1), name=f"{nm}_f", attrs=NO_DSP)
                m.d.sync += [dif.eq(s1 - s0), s0d.eq(s0), prod.eq(dif * mus), s0dd.eq(s0d),
                             fin.eq(s0dd + ((prod + 128) >> 8))]
                outs.append(fin)
            si, sq = outs
        for s, o in ((si, self.o_i), (sq, self.o_q)):
            r = Signal(signed(len(s)), attrs=NO_DSP)
            m.d.comb += r.eq((s + (1 << (shift - 1))) >> shift)
            m.d.sync += o.eq(Mux(r > 2047, 2047, Mux(r < -2048, -2048, r)))
        return m


def main(argv):
    mode, P, span = argv[1], int(argv[2]), int(argv[3])
    linear = len(argv) > 4 and argv[4] == "linear"
    dut = TxInterp(mode, P, span, linear)
    out = os.path.join(os.path.dirname(__file__), "..", "build")
    os.makedirs(out, exist_ok=True)
    tag = f"tx_{mode}_p{P}_s{span}{'_lin' if linear else ''}"
    il = os.path.join(out, tag + ".il")
    with open(il, "w") as f:
        f.write(rtlil.convert(dut, name="tx_interp", ports=dut.ports()))
    stat = os.path.join(out, tag + ".stat")
    yosys = os.path.join(os.path.dirname(sys.executable), "yowasp-yosys")
    script = f"read_rtlil {il}; synth_xilinx -family xc7 -top tx_interp; tee -q -o {stat} stat"
    res = subprocess.run([yosys, "-q", "-p", script], capture_output=True, text=True)
    if res.returncode:
        print(res.stdout[-3000:] + res.stderr[-3000:])
        return res.returncode
    log = open(stat).read().split("design hierarchy")[-1]
    counts = {}
    for line in log.splitlines():
        p = line.split()
        if len(p) == 2 and p[0].isdigit():
            counts[p[1]] = int(p[0])
    luts = sum(v for k, v in counts.items() if k.startswith("LUT"))
    lram = {k: v for k, v in counts.items() if k.startswith("RAM") and not k.startswith("RAMB")}
    ff = sum(v for k, v in counts.items() if k.startswith("FD"))
    print(f"{tag}: {counts.get('DSP48E1', 0)} DSP48E1, {luts} LUT (+LUTRAM {lram}), {ff} FF, "
          f"{counts.get('RAMB18E1', 0)} RAMB18E1, {counts.get('RAMB36E1', 0)} RAMB36E1, "
          f"{counts.get('CARRY4', 0)} CARRY4; S = {bank(P, span, not linear)[2]}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
