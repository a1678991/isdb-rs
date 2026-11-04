"""The formal harness of `LdpcDecoder`'s bank allocator (issue #28's
class; `ldpc.py` docstring, "The codeword queue" and "Abandoned
codewords").

The block under test is the decoder itself, unchanged (the toy-a code:
S = 17, NG = 6, the build's K = 3 banks), driven on `begin`, `in_ctx`,
`start` and `abort` by free inputs. The allocator (`alloc`, the contexts'
`ctx*_open`, `ctx*_drop`, `ctx*_bank`) is a few registers inside it; its
only input from the rest of the decoder is `free_mask`, the bank whose
output stream ends. The `.sby` script makes `free_mask` a free input
(Yosys `cutpoint`), after which nothing of the engine, the load
pipeline or the memories is in the cone of a property and Yosys removes
it: the check is of the allocator alone, against every timing of the
frees the assumption below allows (a superset of the engine's: a commit
reaches the queue 12 cycles after `start`, a decode takes thousands). The
harness drives the decoder's `o_ready` from a free input (tied to 0, the
engine's `free_mask` would be a constant Yosys folds before the cut);
`Cover(freed)`, unreachable from reset without the cut, shows the cut
took effect.

The spec model (the shadow, written from the docstring, not from the
code): per context, whether it holds a bank of an uncommitted codeword
and which; the banks committed and not yet freed, in commit order. A
`begin` on a context that holds a bank (and no `start` in the cycle)
keeps that bank; any other `begin` takes the lowest free bank or, with
none, is dropped; `start` commits the context's codeword (a held bank
joins the committed list; a dropped or aborted one frees nothing);
`abort` frees the context's held bank at once.

Assumptions (the port contract, docstring "Ports"):

- no reset: the decoder's domain is reset only at configuration, which
  is the initial state (every register at its reset value);
- `abort` never in the cycle of `begin` or `start`;
- `start` commits the codeword begun since the context's last `start`: no
  second `start` of a committed codeword without a `begin` between (it
  would commit its bank twice; the monitor `commit_without_begin`); a
  `start` after an `abort`, or before any `begin`, is allowed (a dropped
  codeword's overrun record);
- `free_mask` (cut): zero, or the oldest committed bank not yet freed
  (the result queue is in order, one output stream at a time). Not
  modelled: a commit lost to a full result queue (`queue_overflow`, a
  separate sticky flag of the decoder).

Properties (Assert; every cycle unless said otherwise):

- `alloc` is exactly the banks the contexts hold plus the committed ones
  not yet freed (no bank allocated without a codeword that will be
  committed or is queued: the `bank_leak` monitor's condition, as a set);
- a context holds a bank (`ctx_open & ~ctx_drop`) exactly when the model
  says so, and then `ctx_bank` is the model's bank (the lowest free one
  at its `begin`, or the bank it kept);
- the cycle after an `abort` of context c, c is closed and dropped (holds
  no bank) and the bank it held is free;
- `held` = popcount(`alloc`) of the cycle before (it is registered);
- `load_ready` = `alloc` not full, and `load_ready` of the cycle before
  = (`held` < K) (the monitor's form);
- helper invariants (consistency of the model; they make the properties
  inductive): every bank index < K, the held and committed banks
  pairwise distinct.

Covers: an allocation, a commit of a held bank, an abort that frees one,
a `begin` that keeps its context's bank, a free by an output stream, a
drop (every bank held), and a bank allocated again after it was freed.
"""

from amaranth.hdl import Array, Assert, Assume, Cover, Elaboratable, Module, Mux, ResetSignal, Signal

from .. import ldpcmodel
from ..ldpc import LdpcDecoder
from . import elaborate

K = 3
CTX = 2


class BankHarness(Elaboratable):
    # the signals a counter-example's table shows (tools/formal.py)
    TRACE = ["begin", "in_ctx", "start", "abort", "free_mask", "alloc", "s_live", "ctx0_open", "ctx0_drop",
             "ctx0_bank", "ctx1_open", "ctx1_drop", "ctx1_bank", "held", "load_ready"]

    def __init__(self):
        self.begin = Signal()
        self.in_ctx = Signal()
        self.start = Signal()
        self.abort = Signal()
        # the output's ready: free, so that the engine's `free_mask` is not
        # a constant 0 that Yosys folds before the cut
        self.o_ready = Signal()

    def elaborate(self, platform):
        m = Module()
        dec = LdpcDecoder(17, {2: ldpcmodel.TOYS["toy-a"]}, ngroups=6, banks=K)
        names = ["alloc", "free_mask"] + [f"ctx{c}_{x}" for c in range(CTX) for x in ("open", "drop", "bank")]
        frag, t = elaborate(dec, names)
        m.submodules.dec = frag
        alloc, free_mask = t["alloc"], t["free_mask"]
        c_open = Array(t[f"ctx{c}_open"] for c in range(CTX))
        c_drop = Array(t[f"ctx{c}_drop"] for c in range(CTX))
        c_bank = Array(t[f"ctx{c}_bank"] for c in range(CTX))
        begin, ctx, start, abort = self.begin, self.in_ctx, self.start, self.abort
        m.d.comb += [dec.begin.eq(begin), dec.in_ctx.eq(ctx), dec.start.eq(start), dec.abort.eq(abort),
                     dec.o_ready.eq(self.o_ready)]

        # --- the spec model ---------------------------------------------------
        s_hold = Array(Signal(name=f"s_hold{c}") for c in range(CTX))
        s_bank = Array(Signal(range(K), name=f"s_bank{c}") for c in range(CTX))
        s_spent = Array(Signal(name=f"s_spent{c}") for c in range(CTX))  # committed, no begin since
        fifo = Array(Signal(range(K), name=f"s_fifo{i}") for i in range(K))  # committed, in order
        n = Signal(range(K + 1), name="s_n")

        def bit(b):
            return Mux(b < K, (1 << b)[:K], 0)

        live = Signal(K, name="s_live")
        m.d.comb += live.eq(
            Mux(s_hold[0], bit(s_bank[0]), 0) | Mux(s_hold[1], bit(s_bank[1]), 0)
            | Mux(n > 0, bit(fifo[0]), 0) | Mux(n > 1, bit(fifo[1]), 0) | Mux(n > 2, bit(fifo[2]), 0))
        lowest = Signal(range(K), name="s_lowest")
        for b in reversed(range(K)):
            with m.If(~live[b]):
                m.d.comb += lowest.eq(b)
        full = live.all()
        hold = s_hold[ctx]
        freed = free_mask.any()
        commit = start & hold  # a held bank joins the committed list
        keep = begin & hold & ~start  # the context keeps its bank
        fresh = begin & ~keep

        # the committed list: pop (a free) then push (a commit)
        n_pop = Signal.like(n)
        m.d.comb += n_pop.eq(n - freed)
        for i in range(K):
            src = Mux(freed, fifo[i + 1] if i + 1 < K else 0, fifo[i])
            with m.If(commit & (n_pop == i)):
                m.d.sync += fifo[i].eq(s_bank[ctx])
            with m.Else():
                m.d.sync += fifo[i].eq(src)
        m.d.sync += n.eq(n_pop + commit)
        with m.If(begin):
            m.d.sync += s_spent[ctx].eq(0)
            with m.If(fresh):
                m.d.sync += [s_hold[ctx].eq(~full), s_bank[ctx].eq(Mux(full, 0, lowest))]
        with m.Elif(start):
            m.d.sync += [s_hold[ctx].eq(0), s_spent[ctx].eq(hold | s_spent[ctx])]
        with m.Elif(abort):
            m.d.sync += [s_hold[ctx].eq(0), s_spent[ctx].eq(0)]

        # --- assumptions ------------------------------------------------------
        m.d.comb += [
            Assume(~ResetSignal()),
            Assume(~(abort & (begin | start))),
            Assume(~(start & s_spent[ctx])),
            Assume((free_mask == 0) | ((n > 0) & (free_mask == bit(fifo[0])))),
        ]

        # --- properties -------------------------------------------------------
        m.d.comb += Assert(alloc == live, "alloc = the banks held by contexts + committed not freed")
        for c in range(CTX):
            m.d.comb += [
                Assert((c_open[c] & ~c_drop[c]) == s_hold[c], f"context {c} holds a bank as the model"),
                Assert(~s_hold[c] | (c_bank[c] == s_bank[c]), f"context {c}'s bank is the model's"),
            ]
        ab = Signal(name="p_abort")
        ab_ctx = Signal(name="p_abort_ctx")
        ab_held = Signal(name="p_abort_held")
        ab_bank = Signal(range(K), name="p_abort_bank")
        m.d.sync += [ab.eq(abort), ab_ctx.eq(ctx), ab_held.eq(hold), ab_bank.eq(s_bank[ctx])]
        m.d.comb += Assert(~ab | (~c_open[ab_ctx] & c_drop[ab_ctx] & ~(ab_held & (alloc >> ab_bank)[0])),
                           "after abort: the context holds no bank and its bank is free")
        pop = sum(alloc[b] for b in range(K))
        alloc_d = Signal(K, name="p_alloc_d")
        lr_d = Signal(init=1, name="p_load_ready_d")
        m.d.sync += [alloc_d.eq(alloc), lr_d.eq(dec.load_ready)]
        m.d.comb += [
            Assert(dec.held == sum(alloc_d[b] for b in range(K)), "held = popcount(alloc) a cycle before"),
            Assert(dec.load_ready == (pop < K), "load_ready = a bank is free"),
            Assert(lr_d == (dec.held < K), "load_ready a cycle before = held < K"),
        ]
        # helper invariants of the model
        for c in range(CTX):
            m.d.comb += Assert(s_bank[c] < K, "model: bank index")
        for i in range(K):
            m.d.comb += Assert(fifo[i] < K, "model: committed bank index")
        m.d.comb += Assert(n <= K, "model: committed count")
        ids = [(s_hold[0], s_bank[0]), (s_hold[1], s_bank[1])] + [(n > i, fifo[i]) for i in range(K)]
        for i in range(len(ids)):
            for j in range(i + 1, len(ids)):
                m.d.comb += Assert(~(ids[i][0] & ids[j][0] & (ids[i][1] == ids[j][1])),
                                   "model: live banks distinct")

        # --- covers -----------------------------------------------------------
        was_freed = Signal(K, name="c_was_freed")
        m.d.sync += was_freed.eq(was_freed | free_mask | Mux(abort & hold, bit(s_bank[ctx]), 0))
        m.d.comb += [
            Cover(fresh & ~full),
            Cover(commit),
            Cover(abort & hold),
            Cover(keep),
            Cover(freed),
            Cover(fresh & full),
            Cover(fresh & ~full & (was_freed >> lowest)[0]),
        ]
        return m
