#!/bin/sh
# Decode recordings with two `isdb` binaries and compare the outputs byte
# for byte, with the wall time of each: for checking that a speed-up of
# the receive chain leaves its output unchanged (plan §0.10).
# Usage: tools/rx-compare.sh BASE_ISDB NEW_ISDB SYSTEM REC [REC…]
#   SYSTEM  isdbs3 (TLV streams, `{tlv_id}`) or isdbs (TS, `{ts_id}`)
#   OUT=dir keeps the outputs (default a new temporary directory).
set -eu
BASE=$1 NEW=$2 SYS=$3
shift 3
OUT=${OUT:-$(mktemp -d)}
echo "outputs in $OUT"
case $SYS in
    isdbs3) ph='{tlv_id}' ext=tlv ;;
    *) ph='{ts_id}' ext=ts ;;
esac
status=0
for rec in "$@"; do
    name=$(basename "$rec" .sigmf-meta)
    for which in base new; do
        bin=$BASE
        [ "$which" = new ] && bin=$NEW
        mkdir -p "$OUT/$which"
        t0=$(date +%s.%N)
        "$bin" rx --system "$SYS" --iq "$rec" --out "$OUT/$which/$name-$ph.$ext" \
            > /dev/null 2> "$OUT/$which/$name.log"
        t1=$(date +%s.%N)
        echo "$t1 - $t0" | bc > "$OUT/$which/$name.time"
    done
    same=yes
    for f in "$OUT/base/$name"-*."$ext"; do
        g="$OUT/new/$(basename "$f")"
        cmp -s "$f" "$g" || same=no
    done
    nb=$(ls "$OUT/base/$name"-*."$ext" 2>/dev/null | wc -l)
    nn=$(ls "$OUT/new/$name"-*."$ext" 2>/dev/null | wc -l)
    [ "$nb" = "$nn" ] || same=no
    [ "$same" = yes ] || status=1
    printf '%-24s streams %s/%s identical %s  base %6.2f s  new %6.2f s\n' "$name" "$nb" "$nn" "$same" \
        "$(cat "$OUT/base/$name.time")" "$(cat "$OUT/new/$name.time")"
done
exit $status
