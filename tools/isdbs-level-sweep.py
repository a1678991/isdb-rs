#!/usr/bin/env python3
"""Per-step parsing and the result table of tools/isdbs-level-sweep.sh.

The ISDB-S level-margin sweep (docs/plans/isdbs-next/media-and-margin.md
§6) runs `dt295 status --isdbs --watch N --json`, `dt295 tune`, `dt295
stream` and `isdb ts inspect --json` at each TX attenuation; this helper
turns their output into one CSV row per step and, at the end, into the
Markdown table of §6.3 (the VERIFICATION.md format) with a straight-line
fit of the raw C/N register against the attenuation. Run it with
`python3 -I`; it needs only the standard library.

Subcommands:

  peek  --status F.jsonl [--loss-rounds N]
        Lock figures of one `status --json` run, as `KEY VALUE` lines:
        ROUNDS, LOCKED, LOST (1 when N or more consecutive rounds were not
        locked: 0xC3 != 00), LOCKED_ALL.

  step  --csv sweep.csv --phase P --att-set A [options]
        Appends one row to the CSV (header written on first use) and
        prints the decision figures as `KEY VALUE` lines: ROUNDS, LOST,
        LOCKED_ALL, CN_LEFT_FLOOR (raw C/N mean above 3000, the floor of
        the tc90522 formula, in the locked rounds), ERR_ANY (a non-zero
        error counter in a locked round), TUNE_LOCKED, TUNE_FAILED (the
        tune printed no `lock:` line: a tool failure, not a lock result),
        TS_CLEAN.

  table sweep.csv [--md OUT.md] [--fit-max-att 80] [--est-at-45 -78:-68]
        The Markdown table and the summary lines (hold threshold,
        acquisition threshold, hysteresis, error onsets, fit).

The raw C/N is the TC90532 register 0xBC..0xBD as read; its dB figure is
the Linux tc90522 polynomial (INFERRED for the TC90532, no value below raw
3000). The error counters (0xEB / 0xF0), the packet counts and the BER are
INFERRED from tc90522 as well (PIX-DT295 docs/demod-investigation.md §4).
"""

import argparse
import csv
import json
import os
import re
import sys

CN_FORMULA_MIN = 3000

FIELDS = [
    "utc", "phase", "mode", "att_set", "att_read", "pad_db", "level_db",
    "rounds", "locked_rounds", "unlocked_max_run", "lost", "locked_all", "lock_states",
    "cn_raw_mean", "cn_raw_min", "cn_raw_max", "cn_raw_unlocked_mean",
    "cn_db_mean", "cn_db_min", "cn_db_max", "cn_out_of_range_rounds",
    "ba_min", "ba_max",
    "err_hi_max", "err_lo_max", "err_hi_last", "err_lo_last",
    "ber_hi_max", "ber_lo_max",
    "tune_rc", "tune_locked", "tune_lock_ms",
    "stream_rc", "ts_bytes", "ts_packets", "sync_losses", "tei", "cc",
    "tx_missed", "tx_underrun_lines", "run", "note",
]

# Continuity events that are errors on a received TS: everything but the
# signalled discontinuity and the one legal duplicate (isdb-analyze inspect).
CC_BENIGN = {"signalled discontinuity", "duplicate"}


def kv(d):
    for k, v in d.items():
        print(f"{k} {v}")


def fmt(x, nd=1):
    if x is None:
        return ""
    if isinstance(x, float):
        return f"{x:.{nd}f}"
    return str(x)


def read_status(path, loss_rounds):
    """Figures of one `dt295 status --json` run (rounds of either side)."""
    rounds = []
    if path and os.path.exists(path):
        with open(path) as f:
            for line in f:
                line = line.strip()
                if not line.startswith("{"):
                    continue
                try:
                    o = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if o.get("type") == "round":
                    rounds.append(o)
    r = {"rounds": len(rounds)}
    locked = [o for o in rounds if o.get("locked")]
    r["locked_rounds"] = len(locked)
    run = best = 0
    for o in rounds:
        run = 0 if o.get("locked") else run + 1
        best = max(best, run)
    r["unlocked_max_run"] = best
    r["lost"] = int(bool(rounds) and best >= loss_rounds)
    r["locked_all"] = int(bool(rounds) and len(locked) == len(rounds))
    states = {}
    for o in rounds:
        states[o.get("lock", "?")] = states.get(o.get("lock", "?"), 0) + 1
    r["lock_states"] = ";".join(f"{k}:{v}" for k, v in sorted(states.items()))

    def raw(o):
        if o.get("cn_raw") is not None:
            return o["cn_raw"]
        x = (o.get("extra") or {}).get("bc")
        return int(x, 16) if x else None

    lr = [v for v in (raw(o) for o in locked) if v is not None]
    ur = [v for v in (raw(o) for o in rounds if not o.get("locked")) if v is not None]
    r["cn_raw_mean"] = sum(lr) / len(lr) if lr else None
    r["cn_raw_min"] = min(lr) if lr else None
    r["cn_raw_max"] = max(lr) if lr else None
    r["cn_raw_unlocked_mean"] = sum(ur) / len(ur) if ur else None
    db = [o["cn_db"] for o in locked if o.get("cn_db") is not None]
    r["cn_db_mean"] = sum(db) / len(db) if db else None
    r["cn_db_min"] = min(db) if db else None
    r["cn_db_max"] = max(db) if db else None
    r["cn_out_of_range_rounds"] = sum(1 for o in locked if o.get("cn_below_formula"))
    ba = [o["ba"] for o in rounds if o.get("ba") is not None]
    r["ba_min"] = f"{min(ba):02x}" if ba else None
    r["ba_max"] = f"{max(ba):02x}" if ba else None
    errs = [o["errors"] for o in locked if o.get("errors")]
    r["err_hi_max"] = max(e[0] for e in errs) if errs else None
    r["err_lo_max"] = max(e[1] for e in errs) if errs else None
    r["err_hi_last"] = errs[-1][0] if errs else None
    r["err_lo_last"] = errs[-1][1] if errs else None
    bers = [o["ber"] for o in locked if o.get("ber")]
    for i, name in ((0, "hi"), (1, "lo")):
        v = [b[i] for b in bers if b[i] is not None]
        r[f"ber_{name}_max"] = f"{max(v):.3e}" if v else None
    r["err_any"] = int(any(any(x for x in e) for e in errs))
    r["cn_left_floor"] = int(r["cn_raw_mean"] is not None and r["cn_raw_mean"] > CN_FORMULA_MIN)
    return r


POLL = re.compile(r"^\s*\+\s*([0-9.]+) ms\s+c3=([0-9a-f]{2})")


def read_tune(path, rc):
    r = {"tune_rc": rc, "tune_locked": None, "tune_lock_ms": None, "tune_failed": 0}
    if not path or not os.path.exists(path):
        return r
    lock_line = None
    first_locked = None
    with open(path, errors="replace") as f:
        for line in f:
            m = POLL.match(line)
            if m and m.group(2) == "00" and first_locked is None:
                first_locked = float(m.group(1))
            if line.startswith("lock: "):
                lock_line = line[6:].strip()
    if lock_line is None:
        r["tune_failed"] = int(rc not in ("", None, "0", 0))
        r["tune_locked"] = 0
        return r
    r["tune_locked"] = int(lock_line == "Locked" and str(rc) == "0")
    if r["tune_locked"] and first_locked is not None:
        r["tune_lock_ms"] = f"{first_locked:.0f}"
    return r


def read_inspect(path, ts):
    r = {"ts_bytes": None, "ts_packets": None, "sync_losses": None, "tei": None, "cc": None}
    if ts and os.path.exists(ts):
        r["ts_bytes"] = os.path.getsize(ts)
    if not path or not os.path.exists(path) or os.path.getsize(path) == 0:
        return r
    with open(path) as f:
        j = json.load(f)
    r["ts_packets"] = j.get("packets")
    r["sync_losses"] = j.get("sync_losses", 0)
    r["tei"] = sum(p.get("tei", 0) for p in j.get("pids", []))
    r["cc"] = sum(c.get("count", 0) for c in j.get("cc", []) if c.get("kind") not in CC_BENIGN)
    return r


def read_tx(path):
    r = {"tx_missed": None, "tx_underrun_lines": None}
    if not path or not os.path.exists(path):
        return r
    missed = None
    under = 0
    with open(path, errors="replace") as f:
        for line in f:
            m = re.search(r"TX block MISSED (\d+)", line)
            if m:
                missed = int(m.group(1))
            if "underrun" in line.lower():
                under += 1
    r["tx_missed"] = missed
    r["tx_underrun_lines"] = under
    return r


def cmd_peek(a):
    s = read_status(a.status, a.loss_rounds)
    kv({"ROUNDS": s["rounds"], "LOCKED": s["locked_rounds"], "LOST": s["lost"],
        "LOCKED_ALL": s["locked_all"]})


def cmd_step(a):
    row = {k: "" for k in FIELDS}
    s = read_status(a.status, a.loss_rounds)
    t = read_tune(a.tune, a.tune_rc)
    i = read_inspect(a.inspect, a.ts)
    x = read_tx(a.tx)
    att = float(a.att_set)
    row.update({
        "utc": a.utc, "phase": a.phase, "mode": a.mode, "att_set": f"{att:g}",
        "att_read": a.att_read, "pad_db": f"{a.pad:g}",
        "level_db": f"{-(att - 45.0) - a.pad + 0.0:g}", "run": a.run, "note": a.note,
        "stream_rc": a.stream_rc,
    })
    if a.status:
        for k in FIELDS:
            if k in s:
                row[k] = fmt(s[k])
        for k in ("cn_raw_mean", "cn_raw_unlocked_mean"):
            row[k] = fmt(s[k], 0)
    for d in (t, i, x):
        for k, v in d.items():
            if k in row:
                row[k] = fmt(v)
    new = not os.path.exists(a.csv) or os.path.getsize(a.csv) == 0
    with open(a.csv, "a", newline="") as f:
        w = csv.DictWriter(f, fieldnames=FIELDS)
        if new:
            w.writeheader()
        w.writerow(row)
    clean = (i["ts_packets"] or 0) > 0 and not (i["sync_losses"] or i["tei"] or i["cc"])
    kv({
        "ROUNDS": s["rounds"], "LOST": s["lost"], "LOCKED_ALL": s["locked_all"],
        "CN_LEFT_FLOOR": s["cn_left_floor"], "ERR_ANY": s["err_any"],
        "TUNE_LOCKED": t["tune_locked"] if t["tune_locked"] is not None else "",
        "TUNE_FAILED": t["tune_failed"], "TS_CLEAN": int(clean),
    })


# ---------------------------------------------------------------------------
# table
# ---------------------------------------------------------------------------

MINUS = "−"


def num(s):
    try:
        return float(s)
    except (TypeError, ValueError):
        return None


def signed(x, unit=" dB"):
    if x is None:
        return "—"
    s = f"{abs(x):g}{unit}"
    return f"{MINUS}{s}" if x < 0 else s


def sgn(x, nd):
    s = f"{x:+.{nd}f}"
    return s.replace("-", MINUS)


def fit_line(pts):
    """Least squares y = a + b·x; (b, a, r², n) or None below 3 points."""
    n = len(pts)
    if n < 3:
        return None
    mx = sum(p[0] for p in pts) / n
    my = sum(p[1] for p in pts) / n
    sxx = sum((p[0] - mx) ** 2 for p in pts)
    if sxx == 0:
        return None
    sxy = sum((p[0] - mx) * (p[1] - my) for p in pts)
    syy = sum((p[1] - my) ** 2 for p in pts)
    b = sxy / sxx
    a = my - b * mx
    r2 = (sxy * sxy) / (sxx * syy) if syy else 1.0
    return b, a, r2, n


def cell_raw(r):
    m = num(r["cn_raw_mean"])
    if m is None:
        return "—"
    return f"{m:.0f} ({r['cn_raw_min']}…{r['cn_raw_max']})"


def cell_cn(r):
    db = num(r["cn_db_mean"])
    oor = int(num(r["cn_out_of_range_rounds"]) or 0)
    locked = int(num(r["locked_rounds"]) or 0)
    if db is None:
        return "above range" if oor else "—"
    s = f"{db:.1f} dB"
    if oor:
        s += f" ({oor}/{locked} above range)"
    return s


def cell_ba(r):
    lo, hi = r["ba_min"], r["ba_max"]
    if not lo:
        return "—"
    return lo if lo == hi else f"{lo}…{hi}"


def cell_hold(r):
    if not r["rounds"]:
        return "—"
    s = f"{r['locked_rounds']}/{r['rounds']}"
    return f"**{s} lost**" if r["lost"] == "1" else s


def cell_tune(r):
    if r["tune_locked"] == "1":
        return f"{r['tune_lock_ms']} ms" if r["tune_lock_ms"] else "yes"
    if r["tune_locked"] == "0":
        return "**no**"
    return "—"


def cell_ts(r):
    if not r["ts_packets"]:
        return "—" if not r["stream_rc"] else f"none (rc {r['stream_rc']})"
    return f"{r['sync_losses']} / {r['tei']} / {r['cc']}"


def cmd_table(a):
    with open(a.csv, newline="") as f:
        rows = list(csv.DictReader(f))
    lo45, hi45 = (float(x) for x in a.est_at_45.split(":"))
    main = [r for r in rows if r["phase"] in ("hold", "restart")]
    main.sort(key=lambda r: float(r["att_set"]))
    out = []
    p = out.append
    if main:
        secs = a.stream_secs
        p("| TX att | Level vs. att 45 | Est. tuner input | Hold (c3 = 00 rounds) | "
          f"Fresh tune: lock in | 0xBC raw mean (min…max) | C/N (tc90522 formula) | 0xBA | "
          f"Errors 0xEB / 0xF0 (max) | TS {secs} s: sync / TEI / CC | Run |")
        p("|" + "---|" * 11)
        for r in main:
            lvl = num(r["level_db"])
            est = (f"{signed(lo45 + lvl, '')}…{signed(hi45 + lvl, '')} dBm"
                   if lvl is not None else "—")
            err = ("—" if r["err_hi_max"] == "" else f"{r['err_hi_max']} / {r['err_lo_max']}")
            mode = " (restart)" if r["phase"] == "restart" else ""
            p(f"| {r['att_set']}{mode} | {signed(lvl)} | {est} | {cell_hold(r)} | {cell_tune(r)} | "
              f"{cell_raw(r)} | {cell_cn(r)} | {cell_ba(r)} | {err} | {cell_ts(r)} | {r['run']} |")
    hyst = [r for r in rows if r["phase"] == "hyst"]
    if hyst:
        p("")
        p("Hysteresis (attenuation lowered after the loss, no retune):")
        p("")
        p("| TX att | Level vs. att 45 | Rounds locked | 0xBC raw (locked / unlocked mean) | Run |")
        p("|---|---|---|---|---|")
        for r in hyst:
            unl = num(r["cn_raw_unlocked_mean"])
            p(f"| {r['att_set']} | {signed(num(r['level_db']))} | {r['locked_rounds']}/{r['rounds']} | "
              f"{r['cn_raw_mean'] or '—'} / {'—' if unl is None else f'{unl:.0f}'} | {r['run']} |")
    p("")
    p(summary(rows, main, hyst, a.fit_max_att))
    text = "\n".join(out) + "\n"
    if a.md:
        with open(a.md, "w") as f:
            f.write(text)
    sys.stdout.write(text)


def summary(rows, main, hyst, fit_max_att):
    lines = []
    w = lines.append
    pad = {r["pad_db"] for r in rows if r["pad_db"]}
    held = [float(r["att_set"]) for r in main if r["lost"] == "0" and r["rounds"]]
    lost = [float(r["att_set"]) for r in main if r["lost"] == "1"]
    first_loss = min(lost) if lost else None
    below = [x for x in held if first_loss is None or x < first_loss]

    def at(x):
        if x is None:
            return "—"
        return f"att {x:g} ({signed(-(x - 45.0) - float(next(iter(pad), 0)))} vs. att 45)"

    w(f"- Hold threshold: last held {at(max(below) if below else None)}; "
      f"first loss {at(first_loss) if first_loss is not None else 'none in the range swept'}.")
    acq_ok = [float(r["att_set"]) for r in main if r["tune_locked"] == "1"]
    acq_no = [float(r["att_set"]) for r in main if r["tune_locked"] == "0"]
    first_no = min(acq_no) if acq_no else None
    acq_below = [x for x in acq_ok if first_no is None or x < first_no]
    w(f"- Acquisition threshold (fresh tune): last lock {at(max(acq_below) if acq_below else None)}; "
      f"first failure {at(first_no) if first_no is not None else 'none'}.")
    if hyst:
        back = [float(r["att_set"]) for r in hyst if r["locked_all"] == "1"]
        lowest = min(float(r["att_set"]) for r in hyst)
        w(f"- Hysteresis: re-locked without a retune at {at(max(back))}." if back else
          f"- Hysteresis: no re-lock without a retune down to {at(lowest)}.")
    ber = [float(r["att_set"]) for r in main
           if (num(r["err_hi_max"]) or 0) > 0 or (num(r["err_lo_max"]) or 0) > 0]
    w(f"- Error-counter onset (0xEB / 0xF0 > 0 in a locked round): {at(min(ber)) if ber else 'none'}.")
    ts = [float(r["att_set"]) for r in main
          if r["ts_packets"] and ((num(r["sync_losses"]) or 0) + (num(r["tei"]) or 0) + (num(r["cc"]) or 0)) > 0]
    w(f"- TS-error onset (sync / TEI / CC > 0): {at(min(ts)) if ts else 'none'}.")
    held_raw = [(float(r["att_set"]), float(r["cn_raw_mean"])) for r in main
                if r["lost"] == "0" and r["cn_raw_mean"] and float(r["att_set"]) <= fit_max_att]
    # The linear range: off the floor the bench saw at att 45 (raw below the
    # formula's 3000), when there are enough such steps.
    pts = [p for p in held_raw if p[1] > CN_FORMULA_MIN]
    where = f"held steps with raw > {CN_FORMULA_MIN}, att ≤ {fit_max_att:g}"
    if len(pts) < 3:
        pts, where = held_raw, f"all held steps, att ≤ {fit_max_att:g}"
    f = fit_line(pts)
    if f:
        b, a0, r2, n = f
        w(f"- Raw 0xBC vs. att ({where}): {sgn(b, 1)} per dB "
          f"(raw = {sgn(a0, 0).lstrip('+')} {'+' if b >= 0 else MINUS} {abs(b):.1f}·att, r² {r2:.3f}, {n} steps).")
    else:
        w(f"- Raw 0xBC vs. att: fewer than 3 held steps with a raw value at att ≤ {fit_max_att:g}, no fit.")
    dpts = [(float(r["att_set"]), float(r["cn_db_mean"])) for r in main
            if r["lost"] == "0" and r["cn_db_mean"] and float(r["att_set"]) <= fit_max_att]
    g = fit_line(dpts)
    if g:
        b, a0, r2, n = g
        w(f"- tc90522-formula C/N vs. att: {sgn(b, 2)} dB per dB (r² {r2:.3f}, {n} steps; "
          f"≈ −1 if the formula's scale holds for the TC90532).")
    berm = [(float(r["att_set"]), r["ber_hi_max"], r["ber_lo_max"]) for r in main
            if (num(r["ber_hi_max"]) or 0) > 0 or (num(r["ber_lo_max"]) or 0) > 0]
    if berm:
        w("- BER max per step where non-zero (high / low layer, INFERRED): " +
          ", ".join(f"att {x:g}: {h or '—'} / {lo or '—'}" for x, h, lo in berm) + ".")
    miss = [(float(r["att_set"]), r["tx_missed"]) for r in main if r["tx_missed"]]
    if miss:
        w(f"- TX block MISSED at the end of each step: {', '.join(f'{x:g}: {m}' for x, m in miss)}.")
    if any(r["mode"] == "restart" for r in rows):
        w("- Steps marked (restart) restarted `isdb tx` at the level (no live attenuation "
          "write): their hold is the stability after a fresh acquisition, not a hold threshold.")
    w(f"- Pad beyond the 30 dB chain: {', '.join(sorted(pad)) or '0'} dB. Caveats: absolute "
      "level unmeasured (est. column from the bench script's −68 dBm and the "
      "no-PA −78 dBm at att 45); att > 80 not linear (TX1 leakage, "
      "PIX-DT295 docs/stream-links.md); the C/N polynomial, the 0xEB / 0xF0 "
      "error records and the BER are INFERRED from tc90522.")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    pk = sub.add_parser("peek")
    pk.add_argument("--status", required=True)
    pk.add_argument("--loss-rounds", type=int, default=1)
    st = sub.add_parser("step")
    st.add_argument("--csv", required=True)
    st.add_argument("--phase", required=True, choices=["hold", "hyst", "relock", "restart"])
    st.add_argument("--mode", default="live", choices=["live", "restart"])
    st.add_argument("--att-set", required=True)
    st.add_argument("--att-read", default="")
    st.add_argument("--pad", type=float, default=0.0)
    st.add_argument("--utc", default="")
    st.add_argument("--run", default="")
    st.add_argument("--note", default="")
    st.add_argument("--status")
    st.add_argument("--loss-rounds", type=int, default=1)
    st.add_argument("--tune")
    st.add_argument("--tune-rc", default="")
    st.add_argument("--stream-rc", default="")
    st.add_argument("--ts")
    st.add_argument("--inspect")
    st.add_argument("--tx")
    tb = sub.add_parser("table")
    tb.add_argument("csv")
    tb.add_argument("--md")
    tb.add_argument("--fit-max-att", type=float, default=80.0)
    tb.add_argument("--est-at-45", default="-78:-68")
    tb.add_argument("--stream-secs", default="5")
    a = ap.parse_args()
    {"peek": cmd_peek, "step": cmd_step, "table": cmd_table}[a.cmd](a)


if __name__ == "__main__":
    main()
