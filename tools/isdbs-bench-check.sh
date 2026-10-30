#!/usr/bin/env bash
# Bench acceptance of the ISDB-S broadcast of tools/isdbs-bench-tx.sh
# (docs/plans/isdbs-next/pcr-pacing.md §8): a TS recorded by the PIX-DT295
# is checked with `isdb ts inspect` and `isdb ts validate --expect-rate
# --expect-tsid` (BS SI, the default: also --expect-nid 0x0004 --expect-onid
# 0x0004 --expect-freq-mhz of the transponder, 11727.48 for bs1;
# docs/plans/isdbs-next/bs-si.md §6.4 test 7), and judged PASS when
#   - no pcr.* finding is above info (pcr.rate included: the PCR-implied
#     rate is the expected one within its tolerance),
#   - no tables.frequency finding (PAT/PMT and SI repetition, B10 Table 5-2),
#   - no expect finding (the TS_ID; BS SI: network_id, original_network_id,
#     the 0x43 frequency),
#   - no cross.* finding (the identities of PAT, NIT, SDT, EIT and BIT
#     agree, and the NIT's delivery system with its network_id),
#   - the CBR-fit residual max of every PCR PID is at most --fit-max-ns,
#   - with --placed, the recording equals the transmitter's placed TS
#     byte for byte after aligning on the first packet they share.
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
#   --ignore-nulls     with --placed: compare without null packets
#   --record SECONDS   record RECORDING.ts first: dt295 stream --isdbs
#                      TRANSPONDER --tsid HEX --raw-config (tunes the
#                      receiver; it does not transmit). Refuses to
#                      overwrite an existing file. dt295's transfer logs go
#                      to captures/ beside RECORDING.ts, not the cwd.
#   --transponder TP   for --record, and the 0x43 frequency expected with
#                      BS SI (default bs1)
#   --dt295 PATH       dt295 binary (default $DT295, else
#                      ~/dev/PIX-DT295/target/release/dt295)
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
IGNORE_NULLS=
RECORD=
TP=bs1
DT295=${DT295:-$HOME/dev/PIX-DT295/target/release/dt295}
need() { [ $# -ge 2 ] || die "$1 needs a value"; }
while [ $# -gt 0 ]; do
    case $1 in
        --tsid) need "$@"; TSID=$2; shift 2 ;;
        --terrestrial-si) SI=terrestrial; shift ;;
        --rate) need "$@"; RATE=$2; shift 2 ;;
        --fit-max-ns) need "$@"; FIT_MAX=$2; shift 2 ;;
        --placed) need "$@"; PLACED=$2; shift 2 ;;
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

VERDICT=0
python3 -I - "$T/inspect.json" "$T/validate.json" "$FIT_MAX" "$SI" <<'PY' || VERDICT=1
import json, sys
insp = json.load(open(sys.argv[1]))
val = json.load(open(sys.argv[2]))
fit_max = float(sys.argv[3])
bs = sys.argv[4] == "bs"
ok = True

def verdict(good, what):
    global ok
    ok &= good
    print(f"{'PASS' if good else 'FAIL'}  {what}")

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
print("== verdict")
bad = lambda pred: [f for f in val["findings"] if pred(f)]
verdict(not bad(lambda f: f["check"].startswith("pcr.") and f["severity"] != "info"),
        "no pcr.* finding above info (rate, interval, accuracy, discontinuity)")
verdict(any(f["check"] == "pcr.rate" for f in val["findings"]), "PCR-implied rate measured")
verdict(not bad(lambda f: f["check"] == "tables.frequency"), "no tables.frequency finding")
verdict(not bad(lambda f: f["check"] == "expect"),
        "TS_ID and network as expected" if bs else "TS_ID as expected")
verdict(not bad(lambda f: f["check"].startswith("cross.")),
        "no cross.* finding (SI identities, delivery system)")
fits = [p["fit_max_ns"] for p in insp["pcr"]]
verdict(bool(fits) and all(m is not None and m <= fit_max for m in fits),
        f"CBR-fit max ≤ {fit_max:g} ns on every PCR PID")
sys.exit(0 if ok else 1)
PY

if [ -n "$PLACED" ]; then
    echo "== compare with the placed TS $PLACED"
    # shellcheck disable=SC2086 # empty or one flag
    python3 -I "$HERE/tools/ts-align-compare.py" $IGNORE_NULLS "$REC" "$PLACED" || VERDICT=1
fi
if [ "$VERDICT" = 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$VERDICT"
