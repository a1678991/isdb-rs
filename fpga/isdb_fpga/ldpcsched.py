"""The LDPC decoder's static schedule, computed at elaboration from the
address tables: the plain-Python half of ldpc.py (its docstring has the
design), shared by the HDL, the estimate scripts and the tests.

Storage. A code of the B44 form has n = 374 * NG bits: NG "groups" of 374
bits, the information groups 0 .. G-1 (bit m = 374 g + j) and q = NG - G
parity groups: group G + l holds the parity bits p_c with c = l (mod q),
bit i of it p_{l + i q}. Bit j of a group is in lane j mod S, word j div S
(W = 374 / S words per group): S must divide 374 (1, 2, 11, 17, 22, 34,
187, 374).

The layer address generation. Layer l (the checks c = l + i q, i = 0 ..
373: check i of the layer) has one "entry" per table entry x of a row g
with x = l (mod q), at shift s = x div q, and two parity entries: p_c
(group G + l, shift 0) and p_{c-1} (group G + l - 1, shift 0, for l > 0;
group G + q - 1, shift 1, for l = 0, whose edge of check 0 is absent).
Check i of layer l has, per entry (g, s), bit j = (i - s) mod 374 of
group g:

  bit j of group g is in check c = (x + j q) mod P with x = s q + l, and
  since P = 374 q, c = l + ((s + j) mod 374) q: c is check i = (s + j)
  mod 374 of layer l, so j = (i - s) mod 374.

For p_c (c = l + i q): parity group G + l, bit i (shift 0); for p_{c-1}:
c - 1 = (l - 1) + i q for l > 0 (group G + l - 1, bit i), and for l = 0,
c - 1 = (q - 1) + (i - 1) q (group G + q - 1, bit i - 1: shift 1; check 0
has no p_{c-1}). `check_layout` verifies all of it against the code's
checks as `isdb_fec::ldpc::Code::new` builds them.

A step is S consecutive checks i = st S .. st S + S - 1 of a layer, unit
u taking check i = st S + u. With s = a S + b (0 <= b < S), unit u reads
bit j = (st - a) S + (u - b): lane (u - b) mod S, in word (st - a) mod W
when u >= b, else word (st - a - 1) mod W. In lane terms: lane lam holds
unit (lam + b) mod S's bit, at word (st - a - [lam >= S - b]) mod W. So
an entry's reads in a step are every lane once, at one of two words, and
the lane-to-unit map is a rotation by b.

Edge order. Within a check the edges may be taken in any order: min1,
min2 and the parity are functions of the multiset of t values, and the
"first edge on ties" never shows (a tie makes min2 = min1, so every edge
gets the same magnitude). The layer's entries are ordered by group, the
entries of one group in a layer (a "chain": entries congruent mod q, the
contract's conflicting pairs and triples) consecutive by shift, then
p_{c-1}, then p_c. Every layer of a B44 code has the same degree d (the
decoder relies on it: its write phase is its read phase delayed by a
constant).

Chains and the delta write-back. Entries x and y of one group in a layer
read the same bit j at checks i_x = (s_x + j) mod 374 and i_y = i_x + E
(mod 374), E = (s_y - s_x) mod 374. Both are in one step when E < S and
unit u_y >= E (x first in check order), or when 374 - E < S and u_x <
S - (374 - E) (y first); for S <= 187 at most one of the two holds. The
contract's sum sat(t_1 + r_1,new + sum (r_i,new - r_i,old)) is a sum, so
the order the lane adds the contributions in does not matter, only that
the memory's last write of the bit in the step carries the whole sum: an
edge contributes A = t + r_new when no partner of the step is earlier in
check order (unit u < td, td = the smallest E < S of its partners), else
D = r_new - r_old; and it adds onto the pending sum of the entry one back
(or two back) in ROM order when that entry shares its bit in the step
(u >= lo or u < hi, per partner). Chains of three can be cyclic in
check-order precedence (93/120 at S = 187: shifts 69, 154, 326), which is
why the lane accumulates in write order rather than in check order.

First reads (the syndrome's snapshot). The first read of a bit in a pass
is by the first layer, in pass order, that has an entry of its group,
except: a chain partner in that layer that reads the bit in an earlier
step (partner p with E_p = (s_k - s_p) mod 374: i_k >= E_p and u < E_p),
and bit 373 of group G + q - 1, which layer 0 does not read (the absent
edge of check 0), first read by layer q - 1's p_c entry at its last check.
"""

GROUP = 374


class Layout:
    """The layers of one code for S units. `layers[l]`: the entries of
    layer l, each a dict: g, s, a, b, kind (0 information, 1 p_{c-1}, 2
    p_c), absent (the p_{c-1} entry of layer 0), first (the layer is the
    group's first in pass order), first_end (layer q-1's p_c entry: its
    read at check 373 is the first of that bit), nf (the E_p of every
    chain partner, for the first-read exclusion), t1, t2 (the delta
    thresholds for the partner one and two entries back)."""

    def __init__(self, rows, ngroups, s_units):
        self.rows, self.ngroups, self.S = [list(r) for r in rows], ngroups, s_units
        S = s_units
        assert GROUP % S == 0, f"S = {S} must divide 374"
        assert S <= GROUP // 2, "S > 187: a chain pair could share a bit both ways"
        self.G = len(rows)
        self.q = q = ngroups - self.G
        assert q >= 2, "at least two parity groups"
        self.W = GROUP // S
        per_layer = [[] for _ in range(q)]
        for g, row in enumerate(rows):
            for x in row:
                per_layer[x % q].append((g, x // q))
        first_layer = {}
        self.layers = []
        for layer in range(q):
            # groups the previous layer also has last (their words were just
            # written: reading them late shortens the hazard gaps), a
            # group's entries consecutive
            prev_groups = {g for g, _ in per_layer[layer - 1]} if layer else set()
            ents = [{"g": g, "s": s, "kind": 0, "absent": False}
                    for g, s in sorted(per_layer[layer],
                                       key=lambda e: (e[0] in prev_groups, e[0], e[1]))]
            pc1 = (self.G + layer - 1, 0) if layer else (self.G + q - 1, 1)
            ents.append({"g": pc1[0], "s": pc1[1], "kind": 1, "absent": layer == 0})
            ents.append({"g": self.G + layer, "s": 0, "kind": 2, "absent": False})
            for k, e in enumerate(ents):
                e["a"], e["b"] = divmod(e["s"], S)
                first_layer.setdefault(e["g"], layer)
                e["first"] = first_layer[e["g"]] == layer
                e["first_end"] = layer == q - 1 and e["kind"] == 2
                partners = [p for p in ents if p is not e and p["g"] == e["g"]]
                assert len(partners) <= 2, f"a chain of {len(partners) + 1} in layer {layer}"
                e["nf"] = [(e["s"] - p["s"]) % GROUP for p in partners]
                e["td"] = min([E for E in e["nf"] if E < S], default=S)
                for name, back in (("1", 1), ("2", 2)):
                    lo, hi = S, 0
                    if k >= back and ents[k - back]["g"] == e["g"]:
                        E = (e["s"] - ents[k - back]["s"]) % GROUP
                        if E < S:
                            lo = E
                        if GROUP - E < S:
                            hi = S - (GROUP - E)
                    e["lo" + name], e["hi" + name] = lo, hi
                e["chain"] = bool(partners)
            self.layers.append(ents)
        self.degree = [len(e) for e in self.layers]
        assert len(set(self.degree)) == 1, f"layer degrees differ: {sorted(set(self.degree))}"
        self.d = self.degree[0]
        self.edges = sum(self.degree) * self.W  # per unit and pass, the absent edge included

    def steps(self):
        """(layer, st) of every step of a pass, in order."""
        return [(layer, st) for layer in range(self.q) for st in range(self.W)]

    def unit_of_lane(self, b, lane):
        return (lane + b) % self.S

    def word(self, e, st, lane):
        """The word lane `lane` reads for entry e in step st."""
        S = self.S
        return (st - e["a"] - (lane >= S - e["b"])) % self.W

    def reads(self, layer, st, k):
        """Per unit u: (group, word, lane) of the bit entry k of step (layer,
        st) reads, None for the absent edge."""
        e = self.layers[layer][k]
        S = self.S
        out = []
        for u in range(S):
            if e["absent"] and st == 0 and u == 0:
                out.append(None)
                continue
            lane = (u - e["b"]) % S
            out.append((e["g"], self.word(e, st, lane), lane))
        return out

    def bit(self, loc):
        """The natural codeword index of a stored (group, word, lane)."""
        g, w, lane = loc
        j = w * self.S + lane
        if g < self.G:
            return g * GROUP + j
        return self.G * GROUP + (g - self.G) + j * self.q

    def first_read(self, e, st, u):
        """Whether unit u's read of entry e in step st is the bit's first in
        the pass."""
        S = self.S
        if e["first_end"] and st == self.W - 1 and u == S - 1:
            return True
        if not e["first"]:
            return False
        return not any(u < E and st * S + u >= E for E in e["nf"])

    def contribution(self, e, u):
        """Unit u's edge of entry e: "A" (t + r_new) or "D" (r_new - r_old)."""
        return "D" if u >= e["td"] else "A"

    def acc(self, e, u):
        """0: unit u's edge of entry e starts its lane's pending sum; 1 / 2:
        adds onto the sum of the entry one / two back (the same bit)."""
        if u >= e["lo1"] or u < e["hi1"]:
            return 1
        if u >= e["lo2"] or u < e["hi2"]:
            return 2
        return 0


def check_layout(layout, checks, k):
    """The layout's reads against the code's checks (`ldpcmodel.Code.checks`,
    k information bits): every step, unit and entry. Raises on a
    difference; returns the number of edges checked."""
    q, S = layout.q, layout.S
    n = 0
    for layer in range(q):
        ents = layout.layers[layer]
        for st in range(layout.W):
            per_unit = [layout.reads(layer, st, kk) for kk in range(len(ents))]
            for u in range(S):
                c = layer + (st * S + u) * q
                got = sorted(layout.bit(r[u]) for r in per_unit if r[u] is not None)
                assert got == sorted(checks[c]), f"check {c}: {got} != {sorted(checks[c])}"
                n += len(got)
    return n


def gaps(layout, wlat, dec, max_passes=8):
    """Idle cycles before each step (in `layout.steps()` order) so that
    every read of a stored word's lane comes after the last write of it,
    with the HDL's pipeline: entry k of step n is read (address presented)
    in cycle R_n + k and written in cycle R_n + d + k + wlat (d the
    layer degree, the same in every layer); a pass's first step starts at
    least dec cycles after the previous pass's last reads end (the
    syndrome decision). Steps start at R_n = R_{n-1} + d + gap_n. Passes
    are computed until the gaps repeat (the steady state: with those gaps
    in every pass, the first included, the schedule is hazard-free; the
    first pass, with no earlier writes, can only need less). The absent
    edge is neither read nor written. Returns (gaps, cycles per pass: from
    a pass's first read to the next pass's first read)."""
    S, d = layout.S, layout.d
    last = {}  # (g, word) -> the cycle of the last write, per lane
    steps = layout.steps()
    locs = {st: [layout.reads(*st, k) for k in range(d)] for st in steps}
    r_prev = None
    before = None
    for _ in range(max_passes):
        out = []
        t0 = None
        for i, step in enumerate(steps):
            base = 0 if r_prev is None else r_prev + d
            need = base + (dec if i == 0 and r_prev is not None else 0)
            for k, ls in enumerate(locs[step]):
                for loc in ls:
                    if loc is None:
                        continue
                    t = last.get(loc[:2])
                    if t is not None and t[loc[2]] + 1 - k > need:
                        need = t[loc[2]] + 1 - k
            out.append(need - base)
            for k, ls in enumerate(locs[step]):
                wt = need + d + k + wlat
                for loc in ls:
                    if loc is not None:
                        last.setdefault(loc[:2], [-(1 << 30)] * S)[loc[2]] = wt
            if t0 is None:
                t0 = need
            r_prev = need
        if out == before:
            return out, r_prev + d - t0 + out[0]
        before = out
    raise AssertionError("the step gaps reach no steady state")


def lane_decode(layout, llr, max_iter, offset):
    """The HDL's way of computing the contract, in plain Python: storage in
    (group, word, lane) with a hard-decision bit H per location; a step's
    checks evaluated from the values at its start; its writes entry by
    entry in ROM order, each lane keeping the pending sums of the last two
    entries (`Layout.acc`, `Layout.contribution`); H written with every
    write as the edge's old hard decision (the sign of L at the bit's first
    read in the pass, else H); and pass p's syndrome computed from those
    old decisions: the syndrome of iteration p - 1. A codeword whose
    iteration it satisfies every check ends after pass it + 1 with H as
    its bits. Returns (bits in natural order, iterations, converged,
    unsatisfied); it must equal `ldpcmodel.decode`."""
    S, W = layout.S, layout.W
    lv, hv = {}, {}
    for g in range(layout.ngroups):
        for w in range(W):
            for lane in range(S):
                loc = (g, w, lane)
                lv[loc] = llr[layout.bit(loc)]
                hv[loc] = 0
    rec, sign = {}, {}
    p = 0
    while True:
        p += 1
        unsat = 0
        for (layer, st) in layout.steps():
            ents = layout.layers[layer]
            d = len(ents)
            locs = [layout.reads(layer, st, k) for k in range(d)]
            ts, rolds, hold = [], [], []
            for u in range(S):
                t_u, r_u, h_u, par_h = [], [], [], 0
                for k, e in enumerate(ents):
                    loc = locs[k][u]
                    if loc is None:
                        t_u.append(127)
                        r_u.append(0)
                        h_u.append(0)
                        continue
                    r_old = 0
                    if p > 1:
                        m1, m2, im = rec[(layer, st)][u]
                        mag = m2 if k == im else m1
                        r_old = -mag if sign[(layer, st, k)][u] else mag
                    lval = lv[loc]
                    h = int(lval < 0) if layout.first_read(e, st, u) else hv[loc]
                    par_h ^= h
                    t_u.append(max(-128, min(127, lval - r_old)))
                    r_u.append(r_old)
                    h_u.append(h)
                unsat += par_h
                ts.append(t_u)
                rolds.append(r_u)
                hold.append(h_u)
            new_rec, rnew = [], []
            for u in range(S):
                min1 = min2 = 127
                imin = par = 0
                for k, x in enumerate(ts[u]):
                    a = 127 if x == -128 else abs(x)
                    par ^= x < 0
                    if a < min1:
                        min2, min1, imin = min1, a, k
                    elif a < min2:
                        min2 = a
                m1, m2 = max(min1 - offset, 0), max(min2 - offset, 0)
                new_rec.append((m1, m2, imin))
                rnew.append([(-1 if par ^ (x < 0) else 1) * (m2 if k == imin else m1)
                             for k, x in enumerate(ts[u])])
            pend = [[None, None] for _ in range(S)]  # per lane: (loc, sum) one and two back
            for k, e in enumerate(ents):
                wrote = [None] * S
                for u in range(S):
                    loc = locs[k][u]
                    if loc is None:
                        continue
                    rn = rnew[u][k]
                    v = rn - rolds[u][k] if layout.contribution(e, u) == "D" else ts[u][k] + rn
                    mode = layout.acc(e, u)
                    lane = loc[2]
                    if mode:
                        base = pend[lane][mode - 1]
                        assert base is not None and base[0] == loc, (layer, st, k, u, mode)
                        v += base[1]
                    lv[loc] = max(-128, min(127, v))
                    hv[loc] = hold[u][k]
                    wrote[lane] = (loc, v)
                for lane in range(S):
                    pend[lane] = [wrote[lane], pend[lane][0]]
            rec[(layer, st)] = new_rec
            for k in range(d):
                sign[(layer, st, k)] = [int(rnew[u][k] < 0) for u in range(S)]
        it = p - 1
        if unsat == 0 or it == max_iter:
            bits = [0] * (layout.ngroups * GROUP)
            for loc, h in hv.items():
                bits[layout.bit(loc)] = h
            return bits, it, unsat == 0, unsat
