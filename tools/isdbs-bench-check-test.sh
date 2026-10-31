#!/usr/bin/env bash
# The checks are strings eval'd by check().
# shellcheck disable=SC2016,SC2034,SC2329
# Self-test of tools/isdbs-bench-check.sh --placed and tools/ts-align-compare.py
# on synthetic files, no radio and no tuner: the BS bench TS of isdbt-mux
# (crates/isdbt-mux/examples/bs-bench.toml, 16 s, CBR, its PCRs exact)
# stands for the placed TS, and "recordings" are cut from it as the
# PIX-DT295's raw output makes them (VERIFICATION.md, "ISDB-S PCR-paced
# placement and BS SI into the PIX-DT295 (2026-10-31)"): 6 s from 600
# packets before a PMT, the CAT (PID 0x0001) dropped without a null, the
# PMT dropped until the first PAT and the PCR until the first PMT (its
# AUTO_PID_MODE start-up), the CC of adaptation-only packets rewritten
# (0xF -> 0x0), and the first 60 bytes a partial packet. That recording
# must PASS with the dt295 receiver profile (CAT, index-based CBR fit and
# rate EXPLAINED, the PMT's start-up gap EDGE) and FAIL with the exact
# one; recordings that are wrong in a way no receiver profile allows must
# FAIL: one null packet missing, one PCR value changed, one byte of a
# null's payload changed, the CAT replaced by a null instead of dropped.
# Also: the placed TS against itself PASSes exactly; without --placed the
# CAT is EXPLAINED by the profile but the index-based fit FAILs; usage
# errors exit 2.
#
# Usage: tools/isdbs-bench-check-test.sh   (exits 0 on PASS; ~70 MB in
#   $TMPDIR; needs target/release/isdb or ISDB; KEEP=DIR keeps the
#   check logs there, as a failing run does beside its temporary dir)
set -euo pipefail
HERE=$(cd "$(dirname "$0")/.." && pwd)
ISDB=${ISDB:-$HERE/target/release/isdb}
[ -x "$ISDB" ] || { echo "isdbs-bench-check-test: $ISDB: build it with cargo build --release -p isdb-cli" >&2; exit 2; }
export ISDB
CHECK=$HERE/tools/isdbs-bench-check.sh
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
FAIL=0
check() { if eval "$2"; then echo "ok   $1"; else echo "FAIL $1"; FAIL=1; fi; }

"$ISDB" mux -c "$HERE/crates/isdbt-mux/examples/bs-bench.toml" --seconds 16 \
    --out-ts "$T/placed.ts" --log-level warn
# Its rate (CBR, the slot model's): the expected rate, to 1 ppm.
RATE=$(python3 -I "$HERE/tools/ts-pcr-residuals.py" "$T/placed.ts" |
    sed -n 's/.*(rate \([0-9.]*\) Mbit\/s).*/\1/p' | awk '{ printf "%.0f", $1 * 1e6 }')
[ -n "$RATE" ] || { echo "isdbs-bench-check-test: no rate from the placed TS" >&2; exit 2; }

# make MODE OUT: a recording cut from placed.ts. MODE dt295 as above;
# exact: the same 6 s untouched; dt295+null, dt295+pcr, dt295+byte,
# dt295+catnull: dt295 and one thing wrong.
make() {
    python3 -I - "$T/placed.ts" "$2" "$1" <<'PY'
import sys
src, dst, mode = sys.argv[1:4]
d = open(src, "rb").read()
n = len(d) // 188
pk = [bytearray(d[i * 188:(i + 1) * 188]) for i in range(n)]
pid = lambda p: ((p[1] & 0x1F) << 8) | p[2]
rate_pkts = n / 16                                   # packets per second
# the first NIT after 8 s (the mux sends it every 9 s), then the first PMT
nit = next(i for i in range(int(8 * rate_pkts), n) if pid(pk[i]) == 0x10)
pmt = next(i for i in range(nit - int(0.5 * rate_pkts), n) if pid(pk[i]) == 0x01F0)
start = pmt - 600
cut = pk[start:start + int(6 * rate_pkts)]
if mode != "exact":
    out, seen_pat, seen_pmt = [], False, False
    for p in cut:
        q = pid(p)
        if q == 0x0001 and mode != "dt295+catnull":
            continue                                 # the CAT, dropped without a null
        if q == 0x0001:
            p = bytearray(b"\x47\x1f\xff\x10" + b"\xff" * 184)
        if q == 0x0000:
            seen_pat = True
        if q == 0x01F0 and not seen_pat:
            continue                                 # PMT PID not learnt yet
        if q == 0x01F0:
            seen_pmt = True
        if q == 0x01FF and not seen_pmt:
            continue                                 # PCR PID not learnt yet
        if p[3] & 0x30 == 0x20:
            p = bytearray(p)
            p[3] &= 0xF0                             # CC of adaptation-only packets -> 0
        out.append(p)
    cut = out
mid = len(cut) // 2
if mode == "dt295+null":
    k = next(i for i in range(mid, len(cut)) if pid(cut[i]) == 0x1FFF)
    del cut[k]
elif mode == "dt295+pcr":
    k = next(i for i in range(mid, len(cut)) if pid(cut[i]) == 0x01FF)
    cut[k][10] ^= 0x80                               # PCR base + 1 (bit 0 of the base)
elif mode == "dt295+byte":
    k = next(i for i in range(mid, len(cut)) if pid(cut[i]) == 0x1FFF)
    cut[k][100] ^= 0x01
with open(dst, "wb") as f:
    f.write(bytes(d[(start - 1) * 188 + 128:start * 188]))   # 60 bytes: a partial packet
    for p in cut:
        f.write(p)
PY
}
run() { # run NAME ARGS...: the check's output in $T/NAME.log, its status in $T/NAME.rc
    local name=$1 rc=0
    shift
    "$CHECK" --rate "$RATE:1" --fit-max-ns 100 "$@" > "$T/$name.log" 2>&1 || rc=$?
    echo "$rc" > "$T/$name.rc"
}
rc() { cat "$T/$1.rc"; }
has() { grep -qE -- "$2" "$T/$1.log"; }

for m in exact dt295 dt295+null dt295+pcr dt295+byte dt295+catnull; do make "$m" "$T/rec-$m.ts"; done

run self --receiver exact --placed "$T/placed.ts" "$T/placed.ts"
check "the placed TS against itself: PASS exactly" \
    '[ $(rc self) = 0 ] && has self "^PASS  content identical to the placed TS \([0-9]+ packets\)" && has self "^PASS  CBR-fit max"'

run exact --receiver exact --placed "$T/placed.ts" "$T/rec-exact.ts"
check "an untouched cut (partial first packet): PASS with the exact receiver" \
    '[ $(rc exact) = 0 ] && has exact "^PASS: [0-9]+ packets identical$" && ! has exact "EXPLAINED|EDGE"'

run dt295 --placed "$T/placed.ts" "$T/rec-dt295.ts"
check "the DT295-like recording: PASS with the dt295 profile" '[ $(rc dt295) = 0 ] && ! has dt295 "^FAIL"'
check "dt295: the CAT EXPLAINED by the drops" \
    'has dt295 "^EXPLAINED \[tables.mandatory\] CAT not received: explained by the receiver, which dropped all [1-9]"'
check "dt295: the index-based fit and rate EXPLAINED" \
    'has dt295 "^EXPLAINED \[pcr.accuracy\]" && has dt295 "^EXPLAINED \[pcr.rate\]" && has dt295 "^EXPLAINED CBR-fit max"'
check "dt295: the PMT's start-up gap at the edge" \
    'has dt295 "^EDGE  \[tables.frequency\] PMT .*before it had learnt the PID"'
check "dt295: PCRs all found, corrected fit within 100 ns, content identical modulo drops and CC" \
    'has dt295 "^PASS  PCR values all present in the placed TS" && has dt295 "^PASS  corrected CBR-fit max 0 ns" && has dt295 "^PASS  content identical to the placed TS modulo receiver drops \(pid 0x0001 x[1-9][0-9]*, pid 0x01f0 x1 before it had learnt the PID, pid 0x01ff x[1-9][0-9]* before it had learnt the PID\) and [1-9][0-9]* CC rewrites"'

run dt295-exact --receiver exact --placed "$T/placed.ts" "$T/rec-dt295.ts"
check "the DT295-like recording: FAIL with the exact receiver" \
    '[ $(rc dt295-exact) = 1 ] && has dt295-exact "^FAIL  content identical" && has dt295-exact "^FAIL  CBR-fit max"'

run nodrops --receiver-drops "" --placed "$T/placed.ts" "$T/rec-dt295.ts"
check "dt295 without its CAT drop (--receiver-drops \"\"): FAIL" \
    '[ $(rc nodrops) = 1 ] && has nodrops "^FAIL  \[tables.mandatory\]"'

run null --placed "$T/placed.ts" "$T/rec-dt295+null.ts"
check "one null packet missing: FAIL (content, corrected fit stays a step)" \
    '[ $(rc null) = 1 ] && has null "^FAIL  content identical" && has null "^FAIL  CBR-fit max" && ! has null "^EXPLAINED \[pcr.accuracy\]"'

run pcr --placed "$T/placed.ts" "$T/rec-dt295+pcr.ts"
check "one PCR value changed: FAIL (absent from the placed TS)" \
    '[ $(rc pcr) = 1 ] && has pcr "^FAIL  PCR values all present in the placed TS" && has pcr "1 absent"'

run byte --placed "$T/placed.ts" "$T/rec-dt295+byte.ts"
check "one payload byte changed: FAIL (content only)" \
    '[ $(rc byte) = 1 ] && has byte "^FAIL  content identical" && has byte "first differing byte 100" && has byte "^PASS  corrected CBR-fit max"'

run catnull --placed "$T/placed.ts" "$T/rec-dt295+catnull.ts"
check "the CAT replaced by a null, not dropped: FAIL" \
    '[ $(rc catnull) = 1 ] && has catnull "^FAIL  content identical"'

run noplaced "$T/rec-dt295.ts"
check "no --placed: CAT EXPLAINED by the profile, unverified; the index-based fit FAILs" \
    '[ $(rc noplaced) = 1 ] && has noplaced "not verified without --placed" && has noplaced "^FAIL  CBR-fit max" && has noplaced "--placed corrects it"'

run nulls --ignore-nulls --placed "$T/placed.ts" "$T/rec-dt295.ts"
check "--ignore-nulls: aligned on PCR values, content PASS, no corrected fit" \
    'has nulls "^aligned \(null packets ignored\) on [0-9]+ PCR values" && has nulls "^PASS  content identical" && has nulls "^FAIL  corrected CBR-fit max -"'

RC=0; "$CHECK" --receiver bogus "$T/rec-dt295.ts" > "$T/u1" 2>&1 || RC=$?
check "--receiver bogus: exit 2" '[ $RC = 2 ] && grep -q "dt295 or exact" "$T/u1"'
RC=0; "$CHECK" --receiver-drops 0x1,cat "$T/rec-dt295.ts" > "$T/u2" 2>&1 || RC=$?
check "--receiver-drops 0x1,cat: exit 2" '[ $RC = 2 ]'
RC=0; python3 -I "$HERE/tools/ts-align-compare.py" --receiver-drops 0x2000 "$T/rec-dt295.ts" "$T/placed.ts" > "$T/u3" 2>&1 || RC=$?
check "ts-align-compare.py --receiver-drops 0x2000: exit 2" '[ $RC = 2 ] && grep -q "13-bit" "$T/u3"'

if [ -n "${KEEP:-}" ] || [ "$FAIL" != 0 ]; then
    K=${KEEP:-$T.keep}
    mkdir -p "$K"
    cp "$T"/*.log "$K"/
    echo "logs kept in $K"
fi
if [ "$FAIL" = 0 ]; then echo "PASS"; else echo "FAIL"; fi
exit "$FAIL"
