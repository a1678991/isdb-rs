#!/usr/bin/env python3
"""Byte-exact comparison of a recorded TS with the TS it should be (the
transmitter's placed TS), aligned on PCR values, with the receiver's own
changes accounted for; and the recording's PCRs checked against it.

Usage: ts-align-compare.py [OPTIONS] RECORDING.ts REFERENCE.ts
  --receiver-drops PIDS  comma list of PIDs (0x-hex or decimal), e.g.
                         0x0001: packets of these PIDs in the reference
                         that are absent from the recording were dropped
                         by the receiver without a null in their place;
                         they are skipped, the alignment kept, and
                         counted per PID
  --receiver-learns      the receiver passes the PMT PIDs (of the
                         recording's PAT) and the PCR and elementary PIDs
                         (of its PMTs) only once it has learnt them:
                         packets of such a PID before its first packet in
                         the recording are skipped like receiver drops
                         and counted apart (start-up drops)
  --ignore-af-only-cc    ignore the continuity_counter (byte 3, low
                         nibble) of adaptation-only packets, which a
                         receiver may rewrite (H.222.0 §2.4.3.3: it does
                         not increment there and is not checked)
  --ignore-nulls         drop null packets (PID 0x1FFF) from both first,
                         for a receiver that does not output them
  --fit-max-ns N         also fail when the corrected CBR fit's max
                         residual exceeds N ns
  --json FILE            write the result as JSON (for
                         tools/isdbs-bench-check.sh)

Both files are 188-byte packets (a recording that starts mid-packet is
re-synchronised on the first offset with 0x47 at three successive packet
starts); the reference may be a file still being written, it is read as
it is when opened.

Alignment. The PCRs of the recording's PCR PID (the first PID carrying
a PCR) are looked up by value in the whole reference. A value found
exactly once and in order with the previous anchor (the reference
segment between them at least as long as the recording's) is an anchor.
The placed TS restamps its PCRs on every loop pass, so a PCR value
names one packet of one pass, whereas a section packet repeats
unchanged from pass to pass: aligning on the first packet whose bytes
occur once in the reference, as this tool did before, could pair
packets of different passes. Between anchors the two are walked packet
by packet: equal (with --ignore-af-only-cc, but for the CC of an
adaptation-only packet), a reference packet the receiver may have
dropped skipped, or a mismatch; each segment must end with both sides
used up. Before the first anchor and after the last the walk runs
backward and forward to the ends of the recording. No anchor, no
comparison.

PCR check. Every recording PCR must be found in the reference: absent,
ambiguous (more than once) and out-of-order values are counted and must
all be 0. Three CBR fits (PCR = a + b·index, tools/ts-pcr-residuals.py):
the recording's against its own packet index (what `isdb ts inspect`
reports: a receiver that drops packets leaves a sawtooth of one packet
time, ±14.4 µs at 52.17 Mbit/s, with the drops' period); the recording's
against its index plus the number of receiver-dropped packets before it
(the index on air); and the reference's over the same span. With
--ignore-nulls the recording's index is not the air's and the corrected
fit is not made.

Written for tools/isdbs-bench-check.sh --placed: the reference is the
transmitter's placed TS (`isdb tx --placed-ts`), the recording the
tuner's output (docs/plans/isdbs-next/pcr-pacing.md §7.8, §8 bench
acceptance; the PIX-DT295's raw output and its PID filter:
VERIFICATION.md, "ISDB-S PCR-paced placement and BS SI into the
PIX-DT295").

Exit status: 0 identical over a non-empty overlap (modulo the receiver
drops and CC rewrites allowed) and every PCR found (with --fit-max-ns,
the corrected fit within it too); 1 otherwise or no alignment; 2 usage
or unreadable input.
"""
import argparse
import bisect
import importlib.util
import json
import mmap
import os
import sys

_spec = importlib.util.spec_from_file_location(
    "ts_pcr_residuals",
    os.path.join(os.path.dirname(os.path.abspath(__file__)), "ts-pcr-residuals.py"))
R = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(R)

SIZE = R.SIZE
NULL = R.NULL_PID
SHOW = 5
SLACK = 1 << 16  # reference packets read beyond the anchors' span
pid = R.pid


class Usage(Exception):
    pass


def parse_pids(text):
    out = set()
    for t in text.split(","):
        t = t.strip()
        if not t:
            continue
        try:
            v = int(t, 0)
        except ValueError:
            raise Usage(f"--receiver-drops: {t!r} is not a PID") from None
        if not 0 <= v <= 0x1FFF:
            raise Usage(f"--receiver-drops: {t} is not a 13-bit PID")
        out.add(v)
    return out


def open_ts(path):
    f = open(path, "rb")
    if os.fstat(f.fileno()).st_size == 0:
        raise ValueError(f"{path}: empty")
    buf = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
    sync = R.sync_offset(buf)
    if sync is None:
        raise ValueError(f"{path}: no 188-byte packet sync")
    n = (len(buf) - sync) // SIZE
    return buf, sync, n, len(buf) - sync - n * SIZE


def packet(buf, sync, k):
    return bytes(buf[sync + k * SIZE:sync + (k + 1) * SIZE])


def af_only(p):
    return p[3] & 0x30 == 0x20


def section(p):
    """The section starting in a PUSI packet (one packet's worth), or None."""
    if not p[1] & 0x40 or not p[3] & 0x10:
        return None
    o = 4 + (1 + p[4] if p[3] & 0x20 else 0)
    if o >= SIZE:
        return None
    o += 1 + p[o]  # pointer_field
    return p[o:] if o + 3 <= SIZE else None


def learnt_pids(rec):
    """PMT PIDs of the recording's PATs and PCR / elementary PIDs of its
    PMTs (sections within one packet; the bench's are)."""
    pmts, out = set(), set()
    for p in rec:
        if pid(p) == 0:
            s = section(p)
            if s and s[0] == 0x00:
                n = ((s[1] & 0x0F) << 8) | s[2]
                body = s[8:3 + n - 4]
                for k in range(0, len(body) - 3, 4):
                    if (body[k] << 8) | body[k + 1]:
                        pmts.add(((body[k + 2] & 0x1F) << 8) | body[k + 3])
    out |= pmts
    for p in rec:
        if pid(p) in pmts:
            s = section(p)
            if s and s[0] == 0x02:
                n = ((s[1] & 0x0F) << 8) | s[2]
                end = min(len(s), 3 + n - 4)
                out.add(((s[8] & 0x1F) << 8) | s[9])
                k = 12 + (((s[10] & 0x0F) << 8) | s[11])
                while k + 5 <= end:
                    out.add(((s[k + 1] & 0x1F) << 8) | s[k + 2])
                    k += 5 + (((s[k + 3] & 0x0F) << 8) | s[k + 4])
    out.discard(NULL)
    return out


class Walk:
    """Packet-by-packet comparison; drop_ok(pid, i) says whether the
    receiver may have dropped a packet of pid just before recording
    packet i, and of which kind ("drop" or "learn")."""

    def __init__(self, rec, ref, drop_ok, ignore_cc):
        self.rec, self.ref, self.drop_ok, self.ignore_cc = rec, ref, drop_ok, ignore_cc
        self.identical = 0
        self.cc = 0
        self.dropped = {"drop": {}, "learn": {}}
        self.drop_at = {"drop": [], "learn": []}  # recording index after each drop
        self.mismatch = []  # (recording index or None, reference index or None, what)

    def same(self, a, b):
        if a == b:
            self.identical += 1
            return True
        if (self.ignore_cc and af_only(a) and af_only(b) and a[:3] == b[:3]
                and a[4:] == b[4:] and (a[3] ^ b[3]) & 0xF0 == 0):
            self.cc += 1
            return True
        return False

    def try_drop(self, j, at):
        q = pid(self.ref[j])
        kind = self.drop_ok(q, at)
        if kind:
            self.dropped[kind][q] = self.dropped[kind].get(q, 0) + 1
            self.drop_at[kind].append(at)
        return bool(kind)

    def segment(self, i, i_end, j, j_end):
        """rec[i:i_end] against ref[j:j_end]; both must be used up."""
        rec, ref = self.rec, self.ref
        while i < i_end:
            if j >= j_end:
                self.mismatch.append((i, None, "extra in the recording"))
                i += 1
            elif self.same(rec[i], ref[j]):
                i += 1
                j += 1
            elif self.try_drop(j, i):
                j += 1
            else:
                self.mismatch.append((i, j, "differs"))
                i += 1
                j += 1
        while j < j_end:
            if not self.try_drop(j, i_end):
                self.mismatch.append((None, j, "missing from the recording"))
            j += 1

    def forward(self, i, j):
        """From rec[i] / ref[j] to the end of the recording or the
        reference; returns the recording packets left over."""
        rec, ref = self.rec, self.ref
        while i < len(rec) and j < len(ref):
            if self.same(rec[i], ref[j]):
                i += 1
                j += 1
            elif self.try_drop(j, i):
                j += 1
            else:
                self.mismatch.append((i, j, "differs"))
                i += 1
                j += 1
        return len(rec) - i

    def backward(self, i, j):
        """From rec[i - 1] / ref[j - 1] back to the start of the recording
        or the reference; returns the recording packets left over."""
        rec, ref = self.rec, self.ref
        i -= 1
        j -= 1
        while i >= 0 and j >= 0:
            if self.same(rec[i], ref[j]):
                i -= 1
                j -= 1
            elif self.try_drop(j, i + 1):
                j -= 1
            else:
                self.mismatch.append((i, j, "differs"))
                i -= 1
                j -= 1
        return i + 1


def fit_summary(xs, ys):
    s = R.shape(xs, ys)
    if s is None:
        return None
    s = dict(s)
    del s["residuals_ns"]
    return s


def per_pid(d):
    return ", ".join(f"pid {q:#06x} x{c}" for q, c in sorted(d.items())) or "none"


def compare(a):
    drops = parse_pids(a.receiver_drops) if a.receiver_drops else set()
    rbuf, rsync, rn, rtail = open_ts(a.recording)
    fbuf, fsync, fn, ftail = open_ts(a.reference)
    res = {"recording": a.recording, "reference": a.reference,
           "receiver_drops": [f"{q:#06x}" for q in sorted(drops)],
           "receiver_learns": a.receiver_learns,
           "ignore_af_only_cc": a.ignore_af_only_cc, "ignore_nulls": a.ignore_nulls}
    for name, s, t in ((a.recording, rsync, rtail), (a.reference, fsync, ftail)):
        if s or t:
            print(f"note: {name}: {s} bytes before the first packet, {t} after the last")
    rec_raw = [packet(rbuf, rsync, i) for i in range(rn)]

    # The recording's PCRs, then each value's places in the whole reference.
    pcr_pid, rec_pcrs = R.pcr_packets(rbuf, rsync)
    res["pcr_pid"] = None if pcr_pid is None else f"{pcr_pid:#06x}"
    where = {}
    if pcr_pid is not None:
        for j, v in R.pcr_packets(fbuf, fsync, pcr_pid)[1]:
            where.setdefault(v, []).append(j)
    absent = [i for i, v in rec_pcrs if v not in where]
    ambiguous = [i for i, v in rec_pcrs if len(where.get(v, ())) > 1]
    anchors, misplaced = [], []
    for i, j in ((i, where[v][0]) for i, v in rec_pcrs if len(where.get(v, ())) == 1):
        if anchors and not (j > anchors[-1][1] and (
                a.ignore_nulls or j - anchors[-1][1] >= i - anchors[-1][0])):
            misplaced.append(i)
            continue
        anchors.append((i, j))
    pcr = {"count": len(rec_pcrs), "matched": len(anchors), "absent": len(absent),
           "ambiguous": len(ambiguous), "misplaced": len(misplaced)}
    res["pcr"] = pcr
    nulls = " (null packets ignored)" if a.ignore_nulls else ""
    if not anchors:
        why = ("the recording carries no PCR" if pcr_pid is None
               else f"none of its {len(rec_pcrs)} PCRs (pid {pcr_pid:#06x}) occurs exactly once "
                    "in the reference")
        print(f"FAIL: no alignment{nulls}: {why}")
        res.update(aligned=False, content_ok=False, pcr_ok=False)
        return res

    # The reference around the anchors' span: raw packets lo..hi.
    (i0, j0), (i1, j1) = anchors[0], anchors[-1]
    lo = max(0, j0 - i0 - SLACK)
    hi = min(fn, j1 + (rn - i1) + SLACK)
    ref_raw = [packet(fbuf, fsync, k) for k in range(lo, hi)]
    if a.ignore_nulls:
        rec_ix = [i for i, p in enumerate(rec_raw) if pid(p) != NULL]
        ref_ix = [lo + k for k, p in enumerate(ref_raw) if pid(p) != NULL]
        rec = [rec_raw[i] for i in rec_ix]
        ref = [ref_raw[j - lo] for j in ref_ix]
        rpos = {i: k for k, i in enumerate(rec_ix)}
        fpos = {j: k for k, j in enumerate(ref_ix)}
        fa = [(rpos[i], fpos[j]) for i, j in anchors]
    else:
        rec_ix, ref_ix = range(rn), range(lo, hi)
        rec, ref = rec_raw, ref_raw
        fa = [(i, j - lo) for i, j in anchors]

    learn = learnt_pids(rec) - drops if a.receiver_learns else set()
    first = {}
    for k, p in enumerate(rec):
        first.setdefault(pid(p), k)
    res["learnt_pids"] = [f"{q:#06x}" for q in sorted(learn)]

    def drop_ok(q, at):
        if q in drops:
            return "drop"
        if q in learn and first.get(q, len(rec)) >= at:
            return "learn"
        return None

    w = Walk(rec, ref, drop_ok, a.ignore_af_only_cc)
    before = w.backward(*fa[0])
    for (ia, ja), (ib, jb) in zip(fa, fa[1:]):
        w.segment(ia, ib, ja, jb)
    beyond = w.forward(*fa[-1])
    if hi < fn and beyond:
        # the slack ran out, not the reference
        w.mismatch.append((len(rec) - beyond, None, f"{beyond} recording packets beyond the "
                                                    "reference read"))
        beyond = 0
    compared = len(rec) - before - beyond
    edges = []
    if before:
        edges.append(f"{before} before the reference starts")
    if beyond:
        edges.append(f"{beyond} after it ends")
    print(f"aligned{nulls} on {len(anchors)} PCR values (pid {pcr_pid:#06x}): recording packet "
          f"{rec_ix[fa[0][0]]} = reference packet {ref_ix[fa[0][1]]}; {compared} of {len(rec)} "
          f"recording packets compared" + (f" ({', '.join(edges)})" if edges else ""))
    bad = w.mismatch
    for i, j, what in bad[:SHOW]:
        where_ = (f"recording packet {'-' if i is None else rec_ix[i]} / reference "
                  f"{'-' if j is None else ref_ix[j]}")
        pids = " vs ".join(f"pid {pid(p):#06x}" for p in
                           ([] if i is None else [rec[i]]) + ([] if j is None else [ref[j]]))
        byte = ""
        if i is not None and j is not None:
            byte = f", first differing byte {next(k for k in range(SIZE) if rec[i][k] != ref[j][k])}"
        print(f"  mismatch at {where_}: {what}, {pids}{byte}")
    if len(bad) > SHOW:
        print(f"  ... {len(bad) - SHOW} more")
    dropped, learnt = w.dropped["drop"], w.dropped["learn"]
    print(f"packets: {w.identical} identical, {w.cc} identical but for the CC of an "
          f"adaptation-only packet, {len(bad)} mismatches; dropped by the receiver (no null in "
          f"their place): {per_pid(dropped)}"
          + (f"; before it had learnt the PID: {per_pid(learnt)}" if learn else ""))
    res.update(aligned=True, compared=compared, before_reference=before,
               beyond_reference=beyond, identical=w.identical, cc_rewrites=w.cc,
               receiver_dropped={f"{q:#06x}": c for q, c in sorted(dropped.items())},
               startup_dropped={f"{q:#06x}": c for q, c in sorted(learnt.items())},
               startup_last=(None if not w.drop_at["learn"]
                             else rec_ix[min(max(w.drop_at["learn"]), len(rec) - 1)]),
               mismatches=len(bad),
               recording_pids=sorted(f"{q:#06x}" for q in first))
    content_ok = not bad and compared > 0
    res["content_ok"] = content_ok
    modulo = [m for m, n in (("receiver drops", dropped or learnt), ("CC rewrites", w.cc)) if n]
    if content_ok:
        print(f"PASS: {compared} packets identical"
              + (f" modulo {' and '.join(modulo)}" if modulo else "") + nulls)
    else:
        print(f"FAIL: {len(bad)} of {compared} packets differ{nulls}")

    # PCRs: found, then the three fits.
    pcr_ok = not absent and not ambiguous and not misplaced
    print(f"{'PASS' if pcr_ok else 'FAIL'}: {len(anchors)} of {len(rec_pcrs)} PCR values of the "
          f"recording found in the reference ({len(absent)} absent, {len(ambiguous)} more than "
          f"once, {len(misplaced)} out of order)")
    xs = [i for i, _ in rec_pcrs]
    ys, _ = R.unwrap([v for _, v in rec_pcrs])
    raw = fit_summary(xs, ys)
    pcr["raw_fit"] = raw
    if raw:
        print(f"  recording, index-based:       {R.describe(raw)}")
    steady = sorted(w.drop_at["drop"])
    if len(steady) > 1 and not a.ignore_nulls:
        gaps = [b - c for c, b in zip(steady, steady[1:])]
        pcr["drop_spacing_packets"] = sum(gaps) / len(gaps)
        print(f"  receiver drops {len(steady)} packets, one every {min(gaps)}..{max(gaps)} "
              f"(mean {pcr['drop_spacing_packets']:.0f}) recording packets"
              + (f"; one packet time {SIZE * 8 / raw['rate_bps'] * 1e9:.0f} ns" if raw and raw["rate_bps"] else ""))
    if a.ignore_nulls:
        pcr["corrected_fit"] = None
        print("  recording, drops re-inserted: n/a without the null packets (--ignore-nulls)")
    else:
        drop_at = sorted(w.drop_at["drop"] + w.drop_at["learn"])
        at = dict(anchors)
        cx, cy, offs = [], [], set()
        for (i, _), y in zip(rec_pcrs, ys):
            if i in at:
                x = i + bisect.bisect_right(drop_at, i)
                cx.append(x)
                cy.append(y)
                offs.add(at[i] - x)
        cor = fit_summary(cx, cy)
        pcr["corrected_fit"] = cor
        # every segment accounted for: the corrected index is the reference's less a constant
        pcr["corrected_index_consistent"] = len(offs) == 1
        if cor:
            print(f"  recording, drops re-inserted: {R.describe(cor)}"
                  + ("" if len(offs) == 1 else
                     f" (WARNING: the corrected index is off the reference's by {len(offs)} different amounts)"))
        if a.fit_max_ns is not None:
            good = cor is not None and cor["max_ns"] <= a.fit_max_ns
            pcr_ok &= good
            print(f"{'PASS' if good else 'FAIL'}: corrected CBR-fit max "
                  f"{'-' if cor is None else format(cor['max_ns'], '.0f')} ns "
                  f"{'≤' if good else '>'} {a.fit_max_ns:g} ns")
    ref_ys, _ = R.unwrap([R.pcr(packet(fbuf, fsync, j)) for _, j in anchors])
    pl = fit_summary([j for _, j in anchors], ref_ys)
    pcr["reference_fit"] = pl
    if pl:
        print(f"  reference, same span:         {R.describe(pl)}")
    res["pcr_ok"] = pcr_ok
    return res


def main(argv):
    ap = argparse.ArgumentParser(add_help=False, allow_abbrev=False)
    ap.add_argument("--receiver-drops", default="")
    ap.add_argument("--receiver-learns", action="store_true")
    ap.add_argument("--ignore-af-only-cc", action="store_true")
    ap.add_argument("--ignore-nulls", action="store_true")
    ap.add_argument("--fit-max-ns", type=float)
    ap.add_argument("--json")
    ap.add_argument("-h", "--help", action="store_true")
    ap.add_argument("recording", nargs="?")
    ap.add_argument("reference", nargs="?")
    usage = __doc__.split("\n\n")[1]
    try:
        a = ap.parse_args(argv)
    except SystemExit:
        print(usage, file=sys.stderr)
        return 2
    if a.help:
        print(__doc__)
        return 0
    if not a.recording or not a.reference:
        print(usage, file=sys.stderr)
        return 2
    try:
        res = compare(a)
    except (Usage, OSError, ValueError) as e:
        print(f"ts-align-compare: {e}", file=sys.stderr)
        return 2
    if a.json:
        with open(a.json, "w") as f:
            json.dump(res, f, indent=1)
    return 0 if res.get("content_ok") and res.get("pcr_ok") else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
