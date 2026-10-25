#!/bin/sh
# Phase 3 acceptance: receive every ISDB-S transponder recording in a
# capture directory with `isdb rx --system isdbs`, one TS per TS_ID, and
# check each TS with `isdb ts inspect` / `isdb ts validate`. Prints one line
# per TS with numbers only (no service or network names: project policy).
# Usage: tools/isdbs-ts-acceptance.sh DIR [OUTDIR]
#   DIR     recordings
#   OUTDIR  where the TS files go (default a new temporary directory)
# CAPTURES="bs3 nd2-61" restricts the list (recording base names in DIR).
set -eu
DIR=${1:?usage: DIR, the directory of the recordings}
OUT=${2:-$(mktemp -d)}
ISDB=${ISDB:-target/release/isdb}
cargo build -q --release -p isdb-cli
mkdir -p "$OUT"
echo "TS files in $OUT"
for c in ${CAPTURES:-bs1-40-bw56-iio-cs8 bs3 bs5 bs9 bs13 bs15 bs19 bs21 bs23 \
         nd2-61 nd4-61 nd6-61 nd8-61 nd10-61 nd12-61 nd14-61 nd16-61 nd18-61 nd20-61 nd22-61 nd24-61}; do
    m="$DIR/$c.sigmf-meta"
    [ -f "$m" ] || { echo "$c: missing"; continue; }
    "$ISDB" rx --system isdbs --iq "$m" --out "$OUT/$c-{ts_id}.ts" --json \
        > "$OUT/$c.rx.json" 2> "$OUT/$c.rx.log" || { echo "$c: rx failed (see $OUT/$c.rx.log)"; continue; }
    for ts in "$OUT/$c"-*.ts; do
        [ -f "$ts" ] || continue
        "$ISDB" ts inspect --json "$ts" > "$ts.inspect.json" 2>/dev/null || true
        "$ISDB" ts validate "$ts" > "$ts.validate.txt" 2>&1 || true
        python3 -I - "$c" "$ts" "$OUT/$c.rx.json" <<'EOF'
import json, re, sys
c, ts, rxj = sys.argv[1:4]
rx = json.load(open(rxj))
ins = json.load(open(ts + ".inspect.json"))
val = open(ts + ".validate.txt").read()
tsid = int(re.search(r"-([0-9a-f]{4})\.ts$", ts).group(1), 16)
st = next(t for t in rx["ts"] if t["ts_id"] == tsid)
pids = ins["pids"]
cc = sum(p["cc_events"] for p in pids)
tei = sum(p["tei"] for p in pids)
scr = sum(p["scrambled"] for p in pids)
fit = max((p["fit_max_ns"] for p in ins["pcr"] if p.get("fit_max_ns") is not None), default=None)
pat_tsid = ins["si"].get("transport_stream_id")
tids = sorted({s["key"]["table_id"] for s in ins["sections"]})
have = lambda t: "y" if t in tids else "-"
m = re.search(r"(\d+) errors, (\d+) warnings", val)
print(f"{c:22s} TS_ID {tsid:#06x} slots {st['slots_per_frame']:2d} pkts {st['packets']:6d} "
      f"RS clean {st['rs_clean']:6d} corr {st['rs_corrected_packets']:4d} bad {st['rs_uncorrectable']:4d} | "
      f"CC {cc} TEI {tei} PCRfit {'-' if fit is None else f'{fit:.0f}ns'} scrambled {100*scr/max(ins['packets'],1):4.1f}% | "
      f"PAT tsid {'ok' if pat_tsid == tsid else pat_tsid} PAT{have(0)} CAT{have(1)} PMT{have(2)} NIT{have(0x40)} SDT{have(0x42)} EIT{have(0x4e)} TOT{have(0x73)} BIT{have(0xc4)} | "
      f"validate {m.group(1)}E {m.group(2)}W")
EOF
    done
done
