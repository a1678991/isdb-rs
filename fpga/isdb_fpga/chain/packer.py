"""Stage 9 of the ISDB-S3 chain: the `Slots` blocks (format version 1,
`isdb_frontend::slots`).

Contract (`isdb_frontend::slots` module docs): a block is the sync words
0x1ACF 0xFC1D, header word 2 = 0xF (the `Slots` marker in the `Symbols`
version nibble) | type << 4 | 1 << 8 (format version 1), the 32-bit
sequence number (one counter for every block type, low word first), the
payload length P, the 32-bit frame counter, the header check (words 2 .. 8
sum to 0xFFFF), the P payload words, and the CRC-32C trailer over the
sequence number (4 bytes, little endian) and the payload words (2 bytes
each, little endian), low word first: the front end's packer's network
(`frontend.crc32c_word`, one 16-bit word per clock, seeded with the
sequence number's two words from all ones, output inverted).

`Packer`: `b_valid` with `b_kind`, `b_frame`, `b_len` starts a block
(`b_ready` when idle); the 9 header words leave on the next 9 clocks, then
the payload: `w_ready` asks for the next word (`w_valid`, `w_data`, one per
clock at most), then the 2 trailer words. `o_valid`, `o_word`: the output
(at most one word per clock: the `Symbols` packer's port, the ADC DMA's
channel 0). The sequence number counts from 0 at reset / `start`.

`status_words(snap)`: the status block's payload (`slots::Status`, field
order of its definition) from a snapshot record, as a list of 16-bit
expressions indexed by word (the learned constellation's points through a
read port: `StatusSource`).
"""

from amaranth.hdl import Array, Cat, Const, Elaboratable, Module, Mux, Signal

from ..frontend import crc32c_word

SYNC = (0x1ACF, 0xFC1D)


class Packer(Elaboratable):
    def __init__(self):
        self.start = Signal()
        self.b_valid = Signal()
        self.b_ready = Signal()
        self.b_kind = Signal(4)
        self.b_frame = Signal(32)
        self.b_len = Signal(16)
        self.w_valid = Signal()
        self.w_ready = Signal()
        self.w_data = Signal(16)
        self.o_valid = Signal()
        self.o_word = Signal(16)
        self.seq = Signal(32)

    def elaborate(self, platform):
        m = Module()
        kind = Signal(4)
        frame = Signal(32)
        ln = Signal(16)
        idx = Signal(16)
        crc = Signal(32)
        seq = self.seq
        w2 = 0xF | (kind << 4) | (1 << 8)
        hw = [Const(SYNC[0], 16), Const(SYNC[1], 16), w2, seq[:16], seq[16:], ln, frame[:16], frame[16:]]
        s = (hw[2] + hw[3] + hw[4] + hw[5] + hw[6] + hw[7])[:16]
        hw.append((0xFFFF - s)[:16])
        hwa = Array(Signal(16, name=f"hw{i}") for i in range(9))
        for i in range(9):
            m.d.comb += hwa[i].eq(hw[i])
        m.d.sync += self.o_valid.eq(0)
        # one CRC-32C network: the sequence number's two words go through it
        # in the header's first two cycles, the payload words after
        c_in = Signal(32)
        d_in = Signal(16)
        c_out = Signal(32)
        m.d.comb += [c_in.eq(crc), d_in.eq(self.w_data), c_out.eq(crc32c_word(c_in, d_in))]
        with m.FSM(name="packer"):
            with m.State("IDLE"):
                m.d.comb += self.b_ready.eq(1)
                with m.If(self.start):
                    m.d.sync += seq.eq(0)
                with m.Elif(self.b_valid):
                    m.d.sync += [kind.eq(self.b_kind), frame.eq(self.b_frame), ln.eq(self.b_len), idx.eq(0)]
                    m.next = "HEADER"
            with m.State("HEADER"):
                m.d.sync += [self.o_valid.eq(1), self.o_word.eq(hwa[idx[:4]]), idx.eq(idx + 1)]
                with m.If(idx == 0):
                    m.d.comb += [c_in.eq(0xFFFFFFFF), d_in.eq(seq[:16])]
                    m.d.sync += crc.eq(c_out)
                with m.If(idx == 1):
                    m.d.comb += d_in.eq(seq[16:])
                    m.d.sync += crc.eq(c_out)
                with m.If(idx == 8):
                    m.d.sync += idx.eq(0)
                    m.next = "PAYLOAD"
            with m.State("PAYLOAD"):
                with m.If(idx == ln):
                    m.d.sync += idx.eq(0)
                    m.next = "TRAILER"
                with m.Else():
                    m.d.comb += self.w_ready.eq(1)
                    with m.If(self.w_valid):
                        m.d.sync += [self.o_valid.eq(1), self.o_word.eq(self.w_data), idx.eq(idx + 1),
                                     crc.eq(c_out)]
            with m.State("TRAILER"):
                out = ~crc
                m.d.sync += [self.o_valid.eq(1), self.o_word.eq(Mux(idx == 0, out[:16], out[16:])),
                             idx.eq(idx + 1)]
                with m.If(idx == 1):
                    m.d.sync += seq.eq(seq + 1)
                    m.next = "IDLE"
        return m


def status_layout():
    """The snapshot record's fields (name, bits), as `StatusSnap` holds them."""
    return [("peak", 32), ("pos", 16), ("sum", 48), ("locked", 1), ("parity", 1), ("offset", 16),
            ("stray", 32), ("missed", 32), ("carrier", 32), ("agc", 16)] + \
        [(f"tap{k}_{c}", 16) for k in range(11) for c in range(2)] + \
        [("taps_from", 32), ("sums_frame", 32), ("n0", 32), ("k", 32), ("learned_from", 32),
         ("learned", 5)] + \
        [(n, 32) for n in ("frames", "undecoded", "tmcc_failed", "slots", "ldpc_failed", "bch_failed")] + \
        [("iterations", 64)]


def status_len(learned):
    """Payload words for `learned` (bit i: modulation i + 1 learned)."""
    n = 2 + 1 + 3 + 1 + 1 + 2 + 2 + 2 + 1 + 22 + 2 + 2 + 2 + 2 + 2 + 12 + 4
    for i in range(5):
        n += 1 + (2 * (2 << i) if learned >> i & 1 else 0)
    return n


class StatusSource(Elaboratable):
    """The status block's payload words from a snapshot (`snap`, a dict of
    signals by `status_layout`), in order: `next` advances, `word` is the
    current one, `last` on the last; the learned points through
    `pt_addr` / `pt_data` (the demapper's point RAM of the bank in force
    for the frame, a registered read: `word` of a point is valid a cycle
    after the sequencer moved, two after `next`; the finish stage reads
    each word two cycles after its `next`)."""

    def __init__(self):
        self.snap = {n: Signal(w, name=f"snap_{n}") for n, w in status_layout()}
        self.begin = Signal()
        self.next = Signal()
        self.word = Signal(16)
        self.last = Signal()
        self.length = Signal(16)
        self.pt_addr = Signal(8)
        self.pt_data = Signal(32)

    def elaborate(self, platform):
        m = Module()
        s = self.snap
        fixed = []

        def w32(v):
            fixed.extend([v[:16], v[16:32]])

        w32(s["peak"])
        fixed.append(s["pos"])
        fixed.extend([s["sum"][0:16], s["sum"][16:32], s["sum"][32:48]])
        fixed.append(Cat(s["locked"], s["parity"], Const(0, 14)))
        fixed.append(s["offset"])
        w32(s["stray"])
        w32(s["missed"])
        w32(s["carrier"])
        fixed.append(s["agc"])
        for k in range(11):
            for c in range(2):
                fixed.append(s[f"tap{k}_{c}"])
        for n in ("taps_from", "sums_frame", "n0", "k", "learned_from"):
            w32(s[n])
        head = fixed  # 41 words, then the constellation, then the counters
        tail = []
        for n in ("frames", "undecoded", "tmcc_failed", "slots", "ldpc_failed", "bch_failed"):
            tail.extend([s[n][:16], s[n][16:32]])
        tail.extend([s["iterations"][16 * k:16 * k + 16] for k in range(4)])
        lv = s["learned"]
        # the sequencer: part 0 the head, 1 a modulation's count, 2 its points, 3 the tail
        part = Signal(2)
        i = Signal(16)
        mod = Signal(3)
        lab = Signal(6)
        comp = Signal()
        n = Signal(16)
        npts = Array(Const(2 << k, 6) for k in range(8))[mod]
        learned_m = (lv >> mod)[0]
        m.d.comb += self.length.eq(
            len(head) + len(tail) + 5 + sum(Mux(lv[k], 2 * (2 << k), 0) for k in range(5)))
        wh = Array(head)[i[:6]]
        wt = Array(tail)[i[:4]]
        m.d.comb += self.pt_addr.eq(Cat(lab[:5], mod))
        m.d.comb += self.word.eq(Array([wh, Mux(learned_m, npts, 0), Mux(comp, self.pt_data[16:], self.pt_data[:16]), wt])[part])
        m.d.comb += self.last.eq((part == 3) & (i == len(tail) - 1))
        with m.If(self.begin):
            m.d.sync += [part.eq(0), i.eq(0), mod.eq(0), lab.eq(0), comp.eq(0), n.eq(0)]
        with m.Elif(self.next):
            with m.Switch(part):
                with m.Case(0):
                    m.d.sync += i.eq(i + 1)
                    with m.If(i == len(head) - 1):
                        m.d.sync += [part.eq(1), mod.eq(0)]
                with m.Case(1):
                    with m.If(learned_m):
                        m.d.sync += [part.eq(2), lab.eq(0), comp.eq(0)]
                    with m.Elif(mod == 4):
                        m.d.sync += [part.eq(3), i.eq(0)]
                    with m.Else():
                        m.d.sync += mod.eq(mod + 1)
                with m.Case(2):
                    m.d.sync += comp.eq(~comp)
                    with m.If(comp):
                        m.d.sync += lab.eq(lab + 1)
                        with m.If(lab + 1 == npts):
                            with m.If(mod == 4):
                                m.d.sync += [part.eq(3), i.eq(0)]
                            with m.Else():
                                m.d.sync += [part.eq(1), mod.eq(mod + 1)]
                with m.Case(3):
                    m.d.sync += i.eq(i + 1)
        return m
