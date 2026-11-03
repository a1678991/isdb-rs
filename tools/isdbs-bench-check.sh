#!/usr/bin/env bash
# Bench acceptance of the ISDB-S broadcast of tools/isdbs-bench-tx.sh
# (docs/plans/isdbs-next/pcr-pacing.md §8): a TS recorded by the PIX-DT295
# is checked with `isdb ts inspect` and `isdb ts validate --expect-rate
# --expect-tsid` (BS SI, the default: also --expect-nid 0x0004 --expect-onid
# 0x0004 --expect-freq-mhz of the transponder, 11727.48 for bs1;
# docs/plans/isdbs-next/bs-si.md §6.4 test 7), and judged PASS when
#   - no pcr.* finding is above info (pcr.rate included: the PCR-implied
#     rate is the expected one within its tolerance),
#   - no tables.mandatory finding above info,
#   - no tables.frequency finding (PAT/PMT and SI repetition, B10 Table 5-2),
#   - no expect finding (the TS_ID; BS SI: network_id, original_network_id,
#     the 0x43 frequency),
#   - no cross.* finding (the identities of PAT, NIT, SDT, EIT and BIT
#     agree, and the NIT's delivery system with its network_id),
#   - the CBR-fit residual max of every PCR PID is at most --fit-max-ns,
#   - with --placed (tools/ts-align-compare.py): every PCR value of the
#     recording is in the transmitter's placed TS, the recording's CBR fit
#     with the receiver-dropped packets re-inserted is within
#     --fit-max-ns, and the recording equals the placed TS byte for byte
#     but for what the receiver profile allows.
#
# The receiver. The PIX-DT295's raw output (dt295 --raw-config) is not
# the TS on air: its PID filter passes the SI PIDs 0x10, 0x11, 0x12, 0x14,
# 0x23, 0x24, 0x25, 0x28, 0x29 and 0x1FFF plus the PMT/PCR/ES PIDs it
# learns from the PAT and PMT (AUTO_PID_MODE; PIX-DT295
# docs/stream-investigation.md, "The other lists"), and it drops what it
# does not pass without a null in its place: the CAT (PID 0x0001) every
# time, the PMT and PCR packets before it has learnt their PIDs; and it
# rewrites the continuity_counter of adaptation-only packets (the PCR
# packets: 0xF -> 0x0). Each CAT dropped shortens the recording by one
# packet, so `inspect`'s index-based CBR fit sees a sawtooth of one
# packet time (±14.4 µs, the CAT's period) and `validate` a missing CAT
# and a PMT gap at the start (VERIFICATION.md, "ISDB-S PCR-paced
# placement and BS SI into the PIX-DT295 (2026-10-31)"). With --placed
# and --receiver dt295 (the default) these are verified against the
# placed TS and reported as EXPLAINED (by the receiver) or EDGE (a
# tables.frequency gap only at the capture's edge, every interval within
# its criterion), not FAIL; without --placed the missing CAT is taken
# from the profile, but the index-based fit cannot be corrected.
#
# Usage: tools/isdbs-bench-check.sh [OPTIONS] RECORDING.ts
#   --tsid HEX         expected TS_ID, hex (default 4017, the BS bench
#                      TS; with --terrestrial-si 4010)
#   --terrestrial-si   the TS of tools/isdbs-bench-tx.sh --terrestrial-si:
#                      terrestrial SI, no network expectations
#   --rate BPS[:PPM]   expected PCR-implied rate (default 52170000:100, the
#                      TC8PSK 2/3 x 48 TS; 100 ppm for the tuner's clock)
#   --fit-max-ns N     bound on the CBR-fit residual max (default 1000; the
#                      target is 100)
#   --placed REF.ts    compare with the placed TS (tools/ts-align-compare.py)
#   --receiver NAME    dt295 (default): the PIX-DT295's raw output (this
#                      file's header): receiver drops 0x0001, PMT/PCR/ES
#                      PIDs learnt, the CC of adaptation-only packets
#                      ignored;
#                      exact: the recording must be the TS on air
#                      (isdb rx; tools/isdbs-pacing-e2e.sh)
#   --receiver-drops PIDS  comma list replacing the profile's dropped PIDs
#                      (dt295: 0x0001; "" for none)
#   --ignore-nulls     with --placed: compare without null packets
#   --record SECONDS   record RECORDING.ts first: dt295 stream --isdbs
#                      TRANSPONDER --tsid HEX --raw-config (tunes the
#                      receiver; it does not transmit). Refuses to
#                      overwrite an existing file. dt295's transfer logs go
#                      to captures/ beside RECORDING.ts, not the cwd.
#   --transponder TP   for --record, and the 0x43 frequency expected with
#                      BS SI (default bs1)
#   --dt295 PATH       dt295 binary (default $DT295, else
#                      dt295 on PATH)
# Environment: ISDB (default target/release/isdb of this checkout).
# Exit status: 0 PASS, 1 FAIL, 2 usage or a tool failed.
set -euo pipefail
die() { echo "isdbs-bench-check: $*" >&2; exit 2; }
usage() { sed -n '/^# Usage:/,/^# Exit status:/s/^# \{0,1\}//p' "$0"; }

TSID=
SI=bs
RATE=52170000:100
FIT_MAX=1000
PLACED=
RECEIVER=dt295
DROPS=
DROPS_SET=
IGNORE_NULLS=
RECORD=
TP=bs1
DT295=${DT295:-dt295}
need() { [ $# -ge 2 ] || die "$1 needs a value"; }
while [ $# -gt 0 ]; do
    case $1 in
        --tsid) need "$@"; TSID=$2; shift 2 ;;
        --terrestrial-si) SI=terrestrial; shift ;;
        --rate) need "$@"; RATE=$2; shift 2 ;;
        --fit-max-ns) need "$@"; FIT_MAX=$2; shift 2 ;;
        --placed) need "$@"; PLACED=$2; shift 2 ;;
        --receiver) need "$@"; RECEIVER=$2; shift 2 ;;
        --receiver-drops) need "$@"; DROPS=$2; DROPS_SET=1; shift 2 ;;
        --ignore-nulls) IGNORE_NULLS=--ignore-nulls; shift ;;
        --record) need "$@"; RECORD=$2; shift 2 ;;
        --transponder) need "$@"; TP=$2; shift 2 ;;
        --dt295) need "$@"; DT295=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        --) shift; break ;;
        -*) die "unknown option $1 (see --help)" ;;
        *) break ;;
    esac
done
[ $# -eq 1 ] || { usage >&2; exit 2; }
REC=$1
if [ -z "$TSID" ]; then
    if [ "$SI" = bs ]; then TSID=4017; else TSID=4010; fi
fi
TSID=${TSID#0x}
[[ $TSID =~ ^[0-9a-fA-F]{1,4}$ ]] || die "--tsid $TSID: expected hex, e.g. 4010"
[[ $FIT_MAX =~ ^[0-9]+(\.[0-9]+)?$ ]] || die "--fit-max-ns $FIT_MAX: expected a number"
[ -z "$PLACED" ] || [ -r "$PLACED" ] || die "--placed $PLACED: not readable"
# The receiver profile: what the compare may skip or ignore.
RX_FLAGS=()
case $RECEIVER in
    dt295)
        [ -n "$DROPS_SET" ] || DROPS=0x0001
        RX_FLAGS=(--receiver-learns --ignore-af-only-cc) ;;
    exact) ;;
    *) die "--receiver $RECEIVER: dt295 or exact" ;;
esac
[[ $DROPS =~ ^((0x[0-9a-fA-F]{1,4}|[0-9]{1,4}),)*(0x[0-9a-fA-F]{1,4}|[0-9]{1,4})?$ ]] ||
    die "--receiver-drops $DROPS: expected a comma list of PIDs, e.g. 0x0001"
[ -z "$DROPS" ] || RX_FLAGS+=(--receiver-drops "$DROPS")
# BS SI: the NIT of examples/bs-bench.toml (network_id 0x0004, B10 Annex N;
# original_network_id the same) and the 0x43 frequency of the transponder,
# 11727.48 + 19.18·(n − 1) MHz (SPECS.md §1.2, as isdbt-mux derives it).
EXPECT_SI=()
if [ "$SI" = bs ]; then
    [[ $TP =~ ^[bB][sS]([0-9]+)$ ]] || die "--transponder $TP: expected bsN for BS SI (or --terrestrial-si)"
    FREQ=$(awk -v n="${BASH_REMATCH[1]}" 'BEGIN { printf "%.2f", 11727.48 + 19.18 * (n - 1) }')
    EXPECT_SI=(--expect-nid 0x0004 --expect-onid 0x0004 --expect-freq-mhz "$FREQ")
fi
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || die "$ISDB not found (cargo build --release -p isdb-cli, or set ISDB)"

if [ -n "$RECORD" ]; then
    [[ $RECORD =~ ^[1-9][0-9]*$ ]] || die "--record $RECORD: expected whole seconds"
    [ ! -e "$REC" ] || die "$REC exists; not overwriting it"
    [ -x "$DT295" ] || die "dt295 not found at $DT295 (--dt295 or DT295)"
    echo "== dt295 stream --isdbs $TP --tsid $TSID --raw-config --seconds $RECORD"
    "$DT295" stream --isdbs "$TP" --tsid "$TSID" --raw-config --out "$REC" --seconds "$RECORD" \
        --log-dir "$(dirname "$REC")/captures" || die "dt295 stream failed"
fi
[ -s "$REC" ] || die "$REC: missing or empty"

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
"$ISDB" ts inspect --json "$REC" > "$T/inspect.json" || die "isdb ts inspect failed"
echo "== isdb ts validate --expect-rate $RATE --expect-tsid 0x$TSID${EXPECT_SI[*]:+ ${EXPECT_SI[*]}}"
"$ISDB" ts validate --json --fail-on never --expect-rate "$RATE" --expect-tsid "0x$TSID" \
    "${EXPECT_SI[@]}" "$REC" > "$T/validate.json" || die "isdb ts validate failed"

CMP=
if [ -n "$PLACED" ]; then
    # the report is printed by the verdict below, after validate's
    CMP=$T/compare.json
    RC=0
    {
        echo "== compare with the placed TS $PLACED (receiver $RECEIVER${RX_FLAGS[*]:+: ${RX_FLAGS[*]}})"
        # shellcheck disable=SC2086 # empty or one flag
        python3 -I "$HERE/tools/ts-align-compare.py" $IGNORE_NULLS "${RX_FLAGS[@]}" --json "$CMP" \
            "$REC" "$PLACED" 2>&1 || RC=$?
    } > "$T/compare.txt"
    [ "$RC" -le 1 ] || { cat "$T/compare.txt" >&2; die "ts-align-compare.py failed"; }
    [ -s "$CMP" ] || die "ts-align-compare.py wrote no result"
fi

VERDICT=0
python3 -I - "$T/inspect.json" "$T/validate.json" "$FIT_MAX" "$SI" "$CMP" "$RECEIVER" "$DROPS" \
    "$T/compare.txt" "$RATE" <<'PY' || VERDICT=1
import json, sys
insp = json.load(open(sys.argv[1]))
val = json.load(open(sys.argv[2]))
fit_max = float(sys.argv[3])
bs = sys.argv[4] == "bs"
cmp = json.load(open(sys.argv[5])) if sys.argv[5] else None
receiver = sys.argv[6]
drops = {int(t, 0) for t in sys.argv[7].split(",") if t}
rate_arg = sys.argv[9]
ok = True

def verdict(word, what):
    """PASS, FAIL, or EXPLAINED / EDGE: a finding accounted for by the
    receiver or by the capture's edge, not a failure."""
    global ok
    ok &= word != "FAIL"
    print(f"{word:<5} {what}")

def judge(good, what):
    verdict("PASS" if good else "FAIL", what)

d = insp.get("duration_s")
print(f"== {insp['source']}: {insp['packets']} packets, "
      f"{'?' if d is None else f'{d:.3f}'} s, {insp['null_packets']} null")
si = insp.get("si") or {}
for n in si.get("networks", []):
    for t in n.get("transport_streams", []):
        sat = t.get("satellite")
        if sat:
            where = (f"satellite {sat['frequency_mhz']:.2f} MHz "
                     f"{sat['orbital_position']:.1f}°{'E' if sat['east'] else 'W'} "
                     f"{sat['polarization_name']}")
        else:
            where = "terrestrial" if t.get("delivery") else "no delivery descriptor"
        print(f"NIT network_id {n['network_id']:#06x} \"{n.get('name') or ''}\": "
              f"TS {t['transport_stream_id']:#06x} onid {t['original_network_id']:#06x}, {where}")
for v in si.get("services", []):
    print(f"SDT TS {v['transport_stream_id']:#06x} service {v['service_id']:#06x} "
          f"type {v['service_type']:#04x} \"{v.get('name') or ''}\"")
for p in insp["pcr"]:
    m, r = p["fit_max_ns"], p["fit_rms_ns"]
    rate = p["rate_bps"]
    print(f"PCR pid {p['pid']:#06x}: {p['count']} PCRs, rate "
          f"{'-' if rate is None else f'{rate * 1e-6:.6f}'} Mbit/s, "
          f"CBR-fit max {'-' if m is None else f'{m:.0f}'} ns rms {'-' if r is None else f'{r:.0f}'} ns")
print("== validate (findings at warning and above, and pcr.*)")
order = {"info": 0, "warning": 1, "error": 2}
for f in val["findings"]:
    if order[f["severity"]] >= 1 or f["check"].startswith("pcr."):
        cnt = f" (x{f['count']})" if f["count"] > 1 else ""
        print(f"{f['severity'].upper():<7} [{f['check']}] {f['location']}: {f['message']}{cnt}")
print(f"{val['errors']} errors, {val['warnings']} warnings, {val['infos']} info")
if cmp is not None:
    print(open(sys.argv[8]).read(), end="")

# What the compare established about the receiver.
aligned = bool(cmp and cmp.get("aligned"))
content_ok = aligned and cmp["content_ok"]
pcr = cmp["pcr"] if aligned else {}
pcrs_found = (aligned and pcr["count"] > 0
              and not (pcr["absent"] or pcr["ambiguous"] or pcr["misplaced"]))
cor = pcr.get("corrected_fit") if aligned else None
cor_ok = cor is not None and cor["max_ns"] <= fit_max
steady = {int(k, 16): c for k, c in (cmp or {}).get("receiver_dropped", {}).items()}
startup = {int(k, 16): c for k, c in (cmp or {}).get("startup_dropped", {}).items()}
n_drop = sum(steady.values()) + sum(startup.values())
# The receiver's drops account for an index-based fit off the mark when
# the recording is otherwise the placed TS and the corrected fit is good.
fit_explained = content_ok and pcrs_found and cor_ok and n_drop > 0
cmp_pcr_pid = int(cmp["pcr_pid"], 16) if aligned else None
rec_pids = {int(k, 16) for k in (cmp or {}).get("recording_pids", [])}

def drops_text():
    parts = [f"pid {q:#06x} x{c}" for q, c in sorted(steady.items())]
    parts += [f"pid {q:#06x} x{c} before it had learnt the PID" for q, c in sorted(startup.items())]
    return ", ".join(parts)

print("== verdict")
bad = lambda pred: [f for f in val["findings"] if pred(f)]
pcr_bad = bad(lambda f: f["check"].startswith("pcr.") and f["severity"] != "info")
here = lambda f: f["location"] == f"pid {cmp_pcr_pid:#06x}"
acc = [f for f in pcr_bad if f["check"] == "pcr.accuracy" and fit_explained and here(f)]
# The index-based rate is off by the drops too (n_drop in the packets
# compared: 22 in 692736 is -32 ppm); the corrected fit's rate is judged.
bps, _, ppm = rate_arg.partition(":")
bps, ppm = float(bps), float(ppm or 100)
cor_ppm = None if cor is None or not cor.get("rate_bps") else (cor["rate_bps"] / bps - 1) * 1e6
rate = [f for f in pcr_bad if f["check"] == "pcr.rate" and fit_explained and here(f)
        and cor_ppm is not None and abs(cor_ppm) <= ppm]
judge(len(pcr_bad) == len(acc) + len(rate),
      "no pcr.* finding above info (rate, interval, accuracy, discontinuity)"
      + (f" but the index-based {' and '.join(f['check'] for f in acc + rate)}, explained below"
         if acc or rate else ""))
judge(any(f["check"] == "pcr.rate" for f in val["findings"]), "PCR-implied rate measured")
for f in acc:
    raw = pcr["raw_fit"]
    spacing = pcr.get("drop_spacing_packets")
    verdict("EXPLAINED", f"[pcr.accuracy] {f['location']}: index-based CBR-fit max "
            f"{raw['max_ns']:.0f} ns rms {raw['rms_ns']:.0f} ns: explained by the receiver, which "
            f"dropped {n_drop} packets without a null ({drops_text()})"
            + (f", one every {spacing:.0f} packets" if spacing else "")
            + (f"; the residual's period {raw['period_packets']:.0f} packets"
               if raw.get("period_packets") else ""))
for f in rate:
    verdict("EXPLAINED", f"[pcr.rate] {f['location']}: {f['message']}: the index-based rate, "
            f"explained by the receiver's {n_drop} dropped packets; with them re-inserted "
            f"{cor['rate_bps'] * 1e-6:.6f} Mbit/s ({cor_ppm:+.1f} ppm, within {ppm:g})")

mand = bad(lambda f: f["check"] == "tables.mandatory" and f["severity"] != "info")
for f in mand:
    if f["location"] == "table_id 0x01" and 0x0001 in drops:
        if content_ok and steady.get(0x0001) and 0x0001 not in rec_pids:
            verdict("EXPLAINED", f"[tables.mandatory] CAT not received: explained by the receiver, "
                    f"which dropped all {steady[0x0001]} CAT packets of the placed TS in the capture")
        elif cmp is None:
            verdict("EXPLAINED", f"[tables.mandatory] CAT not received: the {receiver} receiver "
                    "drops PID 0x0001 (not verified without --placed)")
        else:
            verdict("FAIL", "[tables.mandatory] CAT not received, and the compare does not account "
                    "for it (no CAT drop seen, or the content differs beyond the receiver's drops)")
    else:
        verdict("FAIL", f"[tables.mandatory] {f['location']}: {f['message']}")
if not mand:
    judge(True, "no tables.mandatory finding")

def key_text(k):
    t = f"pid={k['pid']:#06x} tid={k['table_id']:#04x}"
    if k.get("ext") is not None:
        t += f" ext={k['ext']:#06x}"
    if k.get("sn") is not None:
        t += f" sn={k['sn']}"
    return t

freq_bad = False
for r in val.get("frequencies", []):
    if not r["status"].startswith("exceeded"):
        continue
    iv = r.get("interval")
    if r["status"] == "exceeded (edge gap)" and iv and iv["max"] <= r["criterion_s"] * 1.0001:
        why = ""
        if startup.get(r["key"]["pid"]):
            why = (f"; the receiver dropped {startup[r['key']['pid']]} packet(s) of pid "
                   f"{r['key']['pid']:#06x} before it had learnt the PID")
        verdict("EDGE", f"[tables.frequency] {r['table']} {key_text(r['key'])}: every interval "
                f"within {r['criterion_s']:g} s (max {iv['max']:.3f} s, {iv['n']} intervals); only "
                f"the gap at the capture's edge, {r['max_gap_s']:.3f} s, exceeds it{why}")
    else:
        freq_bad = True
        verdict("FAIL", f"[tables.frequency] {r['table']} {key_text(r['key'])}: criterion "
                f"{r['criterion_s']:g} s, max interval "
                f"{'-' if not iv else format(iv['max'], '.3f')} s, max gap incl. edges "
                f"{r['max_gap_s']:.3f} s")
listed = {key_text(r["key"]) for r in val.get("frequencies", [])}
for f in bad(lambda f: f["check"] == "tables.frequency" and f["location"] not in listed):
    freq_bad = True
    verdict("FAIL", f"[tables.frequency] {f['location']}: {f['message']}")
if not freq_bad:
    judge(True, "no tables.frequency finding beyond the capture's edges")
judge(not bad(lambda f: f["check"] == "expect"),
      "TS_ID and network as expected" if bs else "TS_ID as expected")
judge(not bad(lambda f: f["check"].startswith("cross.")),
      "no cross.* finding (SI identities, delivery system)")
fits = [(p["pid"], p["fit_max_ns"], p["fit_rms_ns"]) for p in insp["pcr"]]
over = [(q, m, r) for q, m, r in fits if m is None or m > fit_max]
if fits and over and fit_explained and all(m is not None and q == cmp_pcr_pid for q, m, r in over):
    for q, m, r in over:
        verdict("EXPLAINED", f"CBR-fit max {m:.0f} ns rms {r:.0f} ns (index-based, pid {q:#06x}) "
                f"> {fit_max:g} ns: explained by the receiver's dropped packets (corrected fit below)")
else:
    judge(bool(fits) and not over, f"CBR-fit max ≤ {fit_max:g} ns on every PCR PID")
    if over and cmp is None and drops:
        print(f"      (the {receiver} receiver drops pid "
              f"{', '.join(f'{q:#06x}' for q in sorted(drops))} without a null: an index-based fit "
              "is off by one packet time per drop; --placed corrects it)")
if cmp is not None:
    judge(pcrs_found, "PCR values all present in the placed TS"
          + (f" ({pcr['matched']} of {pcr['count']})" if aligned else " (no alignment)"))
    judge(cor_ok, "corrected CBR-fit max "
          + ("-" if cor is None else f"{cor['max_ns']:.0f} ns rms {cor['rms_ns']:.0f} ns")
          + f" {'≤' if cor_ok else '>'} {fit_max:g} ns (receiver-dropped packets re-inserted)")
    mods = []
    if steady or startup:
        mods.append(f"receiver drops ({drops_text()})")
    if aligned and cmp["cc_rewrites"]:
        mods.append(f"{cmp['cc_rewrites']} CC rewrites")
    judge(content_ok, "content identical to the placed TS"
          + (f" modulo {' and '.join(mods)}" if mods else "")
          + (f" ({cmp['compared']} packets)" if aligned else ""))
sys.exit(0 if ok else 1)
PY

if [ "$VERDICT" = 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$VERDICT"
