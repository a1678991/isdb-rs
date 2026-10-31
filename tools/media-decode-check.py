#!/usr/bin/env -S python3 -I
"""Decode check of the media in a TS: a PIX-DT295 recording of the bench
service of tools/isdbs-bench-tx.sh --media, or any TS
(docs/plans/isdbs-next/media-and-margin.md §2.4). Run with python3 -I;
needs ffmpeg and ffprobe, and `isdb` for the continuity counters (else the
counters are checked here).

Checks, per video / audio stream of the program (the first program whose
streams carry decodable media, or --program):

  decode   ffmpeg's error lines decoding the stream with
           -err_detect crccheck+bitstream+buffer (reported; a verdict only
           with --max-decode-errors: a cut first GOP, the cut PES at the
           end of the recording and at each loop seam of a looped TS
           legitimately give some);
  time     DTS (PTS where a PES has none) strictly increasing, PTS >= DTS,
           step statistics (the nominal step and every other one: one
           irregular step per media loop of isdbt-mux, 4503 instead of
           3003 for the 10.010 s video of a 10.027 s loop, and at each loop
           seam of a looped TS);
  cc       continuity errors and transport errors of the stream's PID
           (`isdb ts inspect --json`, else a counter check here);
  content  with --source DIR (video.m2v, audio.m2a): every decoded picture
           and audio frame (ffmpeg framemd5 of the decoded frames) must be
           one of the source's, and they must follow the source's cyclic
           order. Frames not in the source are allowed only in short runs
           (--max-run) at the start or the end of the recording (within
           --edge frames of the start, --max-run of the end), where the
           order jumps (a loop seam of a looped TS, a hole), or replacing
           the first frames after the source's own loop point (the AAC
           decoder's overlap state, open-GOP pictures). A run inside an
           otherwise continuous stretch is a FAIL: that is a corrupted
           frame. Order jumps are listed; --max-jumps bounds them.

Usage: python3 -I tools/media-decode-check.py [OPTIONS] TS
  --source DIR            the media the TS was made from (content check)
  --program N             program_number (decimal or 0x…) to check
  --isdb PATH             isdb binary (default $ISDB, else target/release/isdb
                          of this checkout, else `isdb` on PATH)
  --max-decode-errors N   FAIL above N ffmpeg error lines per stream
  --max-jumps N           FAIL above N order jumps per stream (--source)
  --max-run V,A           longest run of frames not in the source allowed
                          at an edge, a jump or a loop point (default 3,6)
  --edge V,A              frames at the start counted as its edge (default
                          16,8: a GOP of 15 and the AAC decoder's start)
  --keep DIR              keep ffmpeg's error logs and framemd5 files here
  --json FILE             write the results as JSON
Exit status: 0 PASS, 1 FAIL, 2 usage or a tool failed.
"""

import argparse
import bisect
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
from collections import Counter

PTS_WRAP = 1 << 33


def die(msg):
    print(f"media-decode-check: {msg}", file=sys.stderr)
    sys.exit(2)


def run(cmd, **kw):
    try:
        return subprocess.run(cmd, check=False, capture_output=True, text=True, **kw)
    except FileNotFoundError:
        die(f"{cmd[0]} not found")


def pair(text, name):
    try:
        v, a = (int(x) for x in text.split(","))
    except ValueError:
        die(f"{name} {text}: expected V,A (two whole numbers)")
    return {"video": v, "audio": a}


# ---------------------------------------------------------------- streams

def probe_streams(ts, program):
    """The video and audio streams to check: (program_number, [stream])."""
    r = run(["ffprobe", "-v", "error", "-show_programs", "-show_streams",
             "-of", "json", ts])
    if r.returncode != 0:
        die(f"ffprobe {ts}: {r.stderr.strip()}")
    info = json.loads(r.stdout or "{}")

    def usable(s):
        if s.get("codec_type") == "video":
            return bool(s.get("codec_name")) and int(s.get("width") or 0) > 0
        if s.get("codec_type") == "audio":
            return bool(s.get("codec_name")) and int(s.get("sample_rate") or 0) > 0
        return False

    progs = info.get("programs") or []
    if program is not None:
        progs = [p for p in progs if int(p.get("program_id", -1)) == program]
        if not progs:
            die(f"program {program} not in {ts}")
    for p in progs:
        streams = [s for s in p.get("streams", []) if usable(s)]
        if streams:
            return int(p["program_id"]), streams
    # a TS without a PMT ffprobe understood: every usable stream
    streams = [s for s in info.get("streams", []) if usable(s)]
    if not streams:
        die(f"{ts}: no decodable video or audio stream")
    return None, streams


def pid_of(stream):
    v = stream.get("id")
    return int(v, 16) if isinstance(v, str) and v.startswith("0x") else None


# ---------------------------------------------------------------- decode

def decode_errors(ts, idx, log):
    r = run(["ffmpeg", "-hide_banner", "-nostats", "-v", "error",
             "-err_detect", "crccheck+bitstream+buffer", "-i", ts,
             "-map", f"0:{idx}", "-f", "null", "-"])
    lines = [l for l in r.stderr.splitlines() if l.strip()]
    with open(log, "w") as f:
        f.write("\n".join(lines) + ("\n" if lines else ""))
    return lines


# ---------------------------------------------------------------- time stamps

def timestamps(ts, idx):
    r = run(["ffprobe", "-v", "error", "-select_streams", str(idx),
             "-show_entries", "packet=pts,dts", "-of", "csv=p=0", ts])
    if r.returncode != 0:
        die(f"ffprobe packets of stream {idx}: {r.stderr.strip()}")
    out = []
    for line in r.stdout.splitlines():
        f = line.strip().split(",")
        if len(f) < 2:
            continue
        pts = int(f[0]) if f[0] not in ("", "N/A") else None
        dts = int(f[1]) if f[1] not in ("", "N/A") else None
        if pts is None and dts is None:
            continue
        out.append((pts, dts))
    return out


def unwrap(prev, v):
    """v moved to within ±2^32 of prev (33-bit time stamps)."""
    if prev is None:
        return v
    while v - prev > PTS_WRAP // 2:
        v -= PTS_WRAP
    while prev - v > PTS_WRAP // 2:
        v += PTS_WRAP
    return v


def time_check(stamps):
    keys, back, pts_before_dts = [], [], 0
    prev = None
    for i, (pts, dts) in enumerate(stamps):
        k = unwrap(prev, dts if dts is not None else pts)
        if pts is not None and dts is not None and unwrap(dts, pts) < dts:
            pts_before_dts += 1
        if prev is not None and k <= prev:
            back.append({"packet": i, "step": k - prev})
        keys.append(k)
        prev = k
    steps = [b - a for a, b in zip(keys, keys[1:])]
    res = {"packets": len(stamps), "back": len(back), "back_first": back[:10],
           "pts_before_dts": pts_before_dts}
    if steps:
        nominal, n = Counter(steps).most_common(1)[0]
        odd = [{"packet": i + 1, "t_s": (keys[i + 1] - keys[0]) / 90000, "step": s}
               for i, s in enumerate(steps) if s != nominal]
        res.update(nominal=nominal, nominal_count=n, min=min(steps), max=max(steps),
                   median=statistics.median(steps), irregular=len(odd),
                   irregular_list=odd[:20],
                   span_s=(keys[-1] - keys[0]) / 90000)
    return res


# ---------------------------------------------------------------- continuity

def cc_from_isdb(isdb, ts):
    r = run([isdb, "ts", "inspect", "--json", ts])
    if r.returncode != 0:
        return None
    try:
        d = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    if "cc" not in d or "pids" not in d:
        return None
    by_pid = {}
    for e in d["cc"]:
        # identical duplicates and signalled discontinuities are allowed
        if e["kind"] in ("duplicate", "signalled discontinuity"):
            continue
        by_pid.setdefault(e["pid"], Counter())[e["kind"]] += e["count"]
    tei = {p["pid"]: p.get("tei", 0) for p in d["pids"]}
    return {"source": "isdb ts inspect", "sync_losses": d.get("sync_losses", 0),
            "by_pid": by_pid, "tei": tei}


def cc_own(ts, pids):
    """H.222.0 §2.4.3.3: +1 per payload packet, unchanged without payload,
    one duplicate allowed, any value after a discontinuity_indicator."""
    last, dup = {}, {}
    by_pid = {p: Counter() for p in pids}
    tei = Counter()
    sync_losses = 0
    with open(ts, "rb") as f:
        while True:
            b = f.read(188)
            if len(b) < 188:
                break
            if b[0] != 0x47:
                sync_losses += 1
                # resynchronise on the next 0x47 followed by another 188 on
                rest = b[1:] + f.read(188 * 2)
                j = next((k for k in range(len(rest) - 188)
                          if rest[k] == 0x47 and rest[k + 188] == 0x47), None)
                if j is None:
                    break
                f.seek(f.tell() - len(rest) + j)
                continue
            pid = ((b[1] & 0x1F) << 8) | b[2]
            if pid not in by_pid:
                continue
            if b[1] & 0x80:
                tei[pid] += 1
                continue
            afc = (b[3] >> 4) & 3
            cc = b[3] & 0x0F
            disc = afc & 2 and b[4] > 0 and b[5] & 0x80
            prev = last.get(pid)
            if prev is not None and not disc:
                if afc & 1:
                    if cc == prev:
                        if dup.get(pid):
                            by_pid[pid]["excess duplicate"] += 1
                        dup[pid] = True
                    elif cc != (prev + 1) % 16:
                        by_pid[pid]["break"] += 1
                        dup[pid] = False
                    else:
                        dup[pid] = False
                elif cc != prev:
                    by_pid[pid]["counter changed without payload"] += 1
            last[pid] = cc
    return {"source": "own counter check", "sync_losses": sync_losses,
            "by_pid": by_pid, "tei": dict(tei)}


# ---------------------------------------------------------------- content

def framemd5(path, spec, out):
    """([(pts, hash)], pts units per second) of the decoded frames of
    `spec` (-map) in `path`."""
    r = run(["ffmpeg", "-hide_banner", "-nostats", "-v", "error", "-i", path,
             "-map", spec, "-fps_mode", "passthrough", "-f", "framemd5", out])
    if r.returncode != 0 or not os.path.exists(out):
        die(f"ffmpeg framemd5 of {path} ({spec}): {r.stderr.strip()[-400:]}")
    frames, rate = [], 90000
    for line in open(out):
        if line.startswith("#tb "):
            num, den = line.split(":", 1)[1].strip().split("/")
            rate = int(den) / int(num)
            continue
        if line.startswith("#") or not line.strip():
            continue
        f = [x.strip() for x in line.split(",")]
        if len(f) >= 6:
            frames.append((int(f[2]), f[5]))
    return frames, rate


def content_check(dec, src, rate, edge, max_run):
    """Membership of the decoded frames in the source set and their cyclic
    order. `rate`: pts units per second of the decoded frames (for times)."""
    n = len(src)
    where = {}
    for i, (_, h) in enumerate(src):
        where.setdefault(h, []).append(i)
    unique = sum(1 for v in where.values() if len(v) == 1)
    total = len(dec)
    inside = [h in where for _, h in dec]
    in_src = sum(inside)
    t0 = dec[0][0] if dec else 0

    def t(i):
        return round((dec[i][0] - t0) / rate, 3)

    # Order: candidate source indices of the current frame, carried over
    # frames not in the source; a jump where no candidate continues.
    jumps, cand, gap, wraps = [], None, 0, set()
    for i, (_, h) in enumerate(dec):
        if h not in where:
            gap += 1
            continue
        here = set(where[h])
        if cand is not None:
            nxt = {(c + 1 + gap) % n for c in cand}
            cont = nxt & here
            if cont:
                # passing the source's loop point (index n-1 -> 0) on every
                # path that continues (periodic content may have others)
                if all(c + 1 + gap >= n for c in cand if (c + 1 + gap) % n in cont):
                    wraps.add(i)
                here = cont
            else:
                jumps.append({"frame": i, "t_s": t(i), "from": sorted(cand)[:3],
                              "to": sorted(here)[:3], "after_foreign": gap,
                              # the decoder's flush of a cut last PES
                              "at_end": i >= total - max_run})
        cand, gap = here, 0

    # Runs of frames not in the source, classified.
    runs, i = [], 0
    jump_at = {j["frame"] for j in jumps}
    while i < total:
        if inside[i]:
            i += 1
            continue
        j = i
        while j < total and not inside[j]:
            j += 1
        length = j - i
        # Within max_run frames: in display order the cut last picture of a
        # pass (an anchor) comes before the B pictures decoded ahead of it,
        # so the run may end a few frames before the jump it belongs to.
        near = range(i - max_run, j + max_run + 1)
        if i < edge:
            kind = "start"  # a run ending inside the edge window is fine
            ok = j <= edge or length <= max_run
        else:
            if j == total or i >= total - max_run:
                kind = "end"  # the cut last PES
            elif any(f in jump_at for f in near):
                kind = "jump"
            elif any(f in wraps for f in near):
                kind = "loop point"
            else:
                kind = "inside"
            ok = kind != "inside" and length <= max_run
        runs.append({"frame": i, "t_s": t(i), "length": length, "at": kind, "ok": ok})
        i = j
    return {"decoded": total, "in_source": in_src, "source_frames": n,
            "source_unique": unique, "jumps": sum(1 for j in jumps if not j["at_end"]),
            "jump_list": jumps[:20],
            "loop_points": len(wraps), "loop_point_list": [t(w) for w in sorted(wraps)][:20],
            "foreign_runs": runs[:50],
            "bad_runs": sum(1 for r in runs if not r["ok"])}


# ---------------------------------------------------------------- main

def main():
    ap = argparse.ArgumentParser(usage=argparse.SUPPRESS, description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ts")
    ap.add_argument("--source")
    ap.add_argument("--program", type=lambda s: int(s, 0))
    ap.add_argument("--isdb")
    ap.add_argument("--max-decode-errors", type=int)
    ap.add_argument("--max-jumps", type=int)
    ap.add_argument("--max-run", default="3,6")
    ap.add_argument("--edge", default="16,8")
    ap.add_argument("--keep")
    ap.add_argument("--json")
    a = ap.parse_args()
    max_run = pair(a.max_run, "--max-run")
    edge = pair(a.edge, "--edge")
    for tool in ("ffmpeg", "ffprobe"):
        if not shutil.which(tool):
            die(f"{tool} not found on PATH")
    if not os.path.isfile(a.ts) or os.path.getsize(a.ts) == 0:
        die(f"{a.ts}: missing or empty")
    if a.source:
        for f in ("video.m2v", "audio.m2a"):
            if not os.path.isfile(os.path.join(a.source, f)):
                die(f"--source {a.source}: no {f}")
    here = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    isdb = a.isdb or os.environ.get("ISDB") or os.path.join(here, "target/release/isdb")
    if not (os.path.isfile(isdb) and os.access(isdb, os.X_OK)):
        isdb = shutil.which("isdb")

    work_ctx = tempfile.TemporaryDirectory(prefix="media-decode-check-")
    work = a.keep or work_ctx.name
    os.makedirs(work, exist_ok=True)

    prog, streams = probe_streams(a.ts, a.program)
    print(f"== {a.ts}: program {prog if prog is not None else '-'}: " + ", ".join(
        f"#{s['index']} pid {s.get('id', '?')} {s['codec_name']}" for s in streams))

    pids = [p for p in (pid_of(s) for s in streams) if p is not None]
    cc = cc_from_isdb(isdb, a.ts) if isdb else None
    if cc is None:
        cc = cc_own(a.ts, pids)

    src_frames = {}
    if a.source:
        for kind, f in (("video", "video.m2v"), ("audio", "audio.m2a")):
            src_frames[kind], _ = framemd5(os.path.join(a.source, f), "0:0",
                                           os.path.join(work, f"source-{kind}.framemd5"))

    ok = True
    results = {"ts": a.ts, "program": prog, "source": a.source, "streams": []}

    def verdict(good, what):
        nonlocal ok
        ok &= good
        print(f"{'PASS' if good else 'FAIL'}  {what}")

    print(f"== continuity ({cc['source']}): sync losses {cc['sync_losses']}")
    for s in streams:
        kind = s["codec_type"]
        idx = s["index"]
        pid = pid_of(s)
        name = f"{kind} pid {s.get('id', '?')} ({s['codec_name']})"
        print(f"== {name}")
        res = {"index": idx, "pid": pid, "type": kind, "codec": s["codec_name"]}

        errs = decode_errors(a.ts, idx, os.path.join(work, f"decode-{idx}.err"))
        res["decode_errors"] = len(errs)
        print(f"decode: {len(errs)} ffmpeg error lines"
              + "".join(f"\n    {l}" for l in errs[:5]) + ("\n    …" if len(errs) > 5 else ""))
        if a.max_decode_errors is not None:
            verdict(len(errs) <= a.max_decode_errors,
                    f"{kind}: decode errors {len(errs)} ≤ {a.max_decode_errors}")

        tc = time_check(timestamps(a.ts, idx))
        res["time"] = tc
        if "nominal" in tc:
            print(f"time: {tc['packets']} PES over {tc['span_s']:.3f} s, step nominal "
                  f"{tc['nominal']} ({tc['nominal_count']}x), median {tc['median']}, "
                  f"min {tc['min']}, max {tc['max']}; {tc['irregular']} irregular")
            for o in tc["irregular_list"][:10]:
                print(f"    PES {o['packet']} at {o['t_s']:.3f} s: step {o['step']}")
        verdict(tc["packets"] > 1 and tc["back"] == 0,
                f"{kind}: {'DTS' if kind == 'video' else 'PTS'} strictly increasing "
                f"({tc['back']} steps back or equal)")
        verdict(tc["pts_before_dts"] == 0, f"{kind}: PTS ≥ DTS ({tc['pts_before_dts']} not)")

        if pid is not None:
            ev = cc["by_pid"].get(pid, Counter())
            tei = cc["tei"].get(pid, 0)
            res["cc_errors"] = dict(ev)
            res["tei"] = tei
            n_ev = sum(ev.values())
            verdict(n_ev == 0 and tei == 0,
                    f"{kind}: continuity errors {n_ev}"
                    + (f" ({', '.join(f'{k} {v}' for k, v in ev.items())})" if n_ev else "")
                    + f", transport errors {tei}")

        if a.source:
            dec, rate = framemd5(a.ts, f"0:{idx}", os.path.join(work, f"decoded-{idx}.framemd5"))
            c = content_check(dec, src_frames[kind], rate, edge[kind], max_run[kind])
            res["content"] = c
            print(f"content: {c['in_source']} of {c['decoded']} decoded frames in the "
                  f"source's {c['source_frames']} ({c['source_unique']} unique hashes); "
                  f"{c['loop_points']} loop points, {c['jumps']} order jumps")
            if c["source_unique"] < c["source_frames"]:
                print(f"    note: {c['source_frames'] - c['source_unique']} source frames share "
                      "their hash with another (periodic content): the order is followed "
                      "from the first unique frame on, a jump by a period is not seen")
            for w in c["loop_point_list"][:10]:
                print(f"    source loop point at {w:.3f} s")
            for j in c["jump_list"][:10]:
                print(f"    jump at frame {j['frame']} ({j['t_s']:.3f} s): source "
                      f"{j['from']} -> {j['to']}"
                      + (f" after {j['after_foreign']} not in the source" if j["after_foreign"] else "")
                      + (" (at the end, not counted)" if j["at_end"] else ""))
            for r_ in c["foreign_runs"][:10]:
                print(f"    {r_['length']} not in the source from frame {r_['frame']} "
                      f"({r_['t_s']:.3f} s), at {r_['at']}{'' if r_['ok'] else ': not allowed'}")
            verdict(c["decoded"] > 0 and c["bad_runs"] == 0,
                    f"{kind}: every decoded frame in the source but at the edges, "
                    f"jumps and loop points (runs ≤ {max_run[kind]})")
            if a.max_jumps is not None:
                verdict(c["jumps"] <= a.max_jumps, f"{kind}: order jumps {c['jumps']} ≤ {a.max_jumps}")
        results["streams"].append(res)

    kinds = {s["codec_type"] for s in streams}
    if a.source:
        verdict(kinds >= {"video", "audio"}, "a video and an audio stream to compare")
    results["pass"] = ok
    if a.json:
        with open(a.json, "w") as f:
            json.dump(results, f, indent=1, default=lambda o: dict(o))
    if a.keep:
        print(f"== logs and framemd5 files in {a.keep}")
    work_ctx.cleanup()
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
