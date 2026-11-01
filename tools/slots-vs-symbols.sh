#!/usr/bin/env bash
# The FPGA receive chain's model against the host receiver on one Symbols
# recording, TLV packet by TLV packet (docs/plans/isdbs3-fpga-rx.md,
# piece 5; the bench version of the ladder's step 4).
#
#   tools/slots-vs-symbols.sh SYMBOLS_FILE [OUT_DIR] [SECONDS]
#
# Three runs of a release `isdb`:
#   A  isdb rx --symbols FILE                       (the host receiver)
#   B  isdb rx --symbols FILE --chain fixed --slots-out OUT/chain.slots
#                                                   (the chain's CPU model,
#                                                    through the Slots words)
#   C  isdb rx --slots OUT/chain.slots              (B's recorded words: the
#                                                    path a radio's chain takes)
# each writing OUT/{a,b,c}/rx-{tlv_id}.tlv and OUT/{a,b,c}.json, then
# tools/tlv-packet-diff.py A B and B C (C must equal B exactly), and the
# frames the chain did not decode from B's summary.
#
# The model takes about 4.6 s per second of signal on one thread (plus the
# LDPC on all cores): the 40 s FPGA recording takes about 4 minutes.
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
sym=${1:?usage: $0 SYMBOLS_FILE [OUT_DIR] [SECONDS]}
out=${2:-$here/target/slots-vs-symbols}
secs=${3:-0}
isdb=${ISDB:-$here/target/release/isdb}
if [[ ! -x $isdb ]]; then
    cargo build --release --locked -p isdb-cli --manifest-path "$here/Cargo.toml"
fi
mkdir -p "$out"/{a,b,c}
rm -f "$out"/{a,b,c}/*.tlv
common=(--system isdbs3 --json --status-secs 0 --seconds "$secs")

echo "== A: host receiver" >&2
/usr/bin/time -v "$isdb" rx --symbols "$sym" "${common[@]}" \
    --out "$out/a/rx-{tlv_id}.tlv" >"$out/a.json" 2>"$out/a.log" || {
    tail "$out/a.log" >&2
    exit 1
}
echo "== B: chain model (--chain fixed), Slots words to $out/chain.slots" >&2
/usr/bin/time -v "$isdb" rx --symbols "$sym" --chain fixed --slots-out "$out/chain.slots" \
    "${common[@]}" --out "$out/b/rx-{tlv_id}.tlv" >"$out/b.json" 2>"$out/b.log" || {
    tail "$out/b.log" >&2
    exit 1
}
echo "== C: the recorded Slots words (--slots)" >&2
/usr/bin/time -v "$isdb" rx --slots "$out/chain.slots" "${common[@]}" \
    --out "$out/c/rx-{tlv_id}.tlv" >"$out/c.json" 2>"$out/c.log" || {
    tail "$out/c.log" >&2
    exit 1
}

for r in a b c; do
    printf '%s: ' "$r"
    python3 -I - "$out/$r.json" "$out/$r.log" <<'EOF'
import json, re, sys
j = json.load(open(sys.argv[1]))
log = open(sys.argv[2]).read()
def t(k):
    m = re.search(k + r":\s*([0-9.:]+)", log)
    return m.group(1) if m else "?"
c = j.get("chain") or {}
print(f"{j['decoded_by']}: {j['seconds']:.3f} s, {j['frames']} frames, {j['slots']} slots, "
      f"LDPC failed {j['ldpc_failed']}, BCH failed {j['bch_failed']}, "
      f"mean {j['ldpc_mean_iterations']:.3f} iterations; "
      f"undecoded frames {c.get('frames_undecoded', '-')}, restarts {c.get('restarts', '-')}, "
      f"discontinuities {c.get('discontinuities', '-')}; packets {j['written_packets']}; "
      f"wall {t('Elapsed .wall clock. time .h:mm:ss or m:ss.')}, user {t('User time .seconds.')} s")
EOF
done

echo "== A (host) against B (chain model)"
python3 -I "$here/tools/tlv-packet-diff.py" "$out/a" "$out/b" --json "$out/a-vs-b.json"
echo "== B (chain model in process) against C (its Slots words read back)"
python3 -I "$here/tools/tlv-packet-diff.py" "$out/b" "$out/c" --json "$out/b-vs-c.json"
