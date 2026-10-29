#!/bin/sh
# SNR sweep of the ISDB-S software loopback (`isdb loopback --system isdbs`):
# one line per (SNR, seed) with the verdict, the input packets delivered
# byte-exact against those sent, the packets lost or wrong, the RS words
# corrected (of all decoded, the lead included) and uncorrectable, the main
# MER and the trim the front end found. The channel uses the seed | 1, so
# give odd seeds.
# Used for the VERIFICATION.md rung "ISDB-S transmitter and loopback".
#
#   cargo build --release -p isdb-cli
#   tools/isdbs-loopback-sweep.sh "tc8psk2/3:48" "8 9 10 11 12" "1 2 3"
#   tools/isdbs-loopback-sweep.sh "qpsk3/4:48" "3 4 5 6" "1 2 3" --cfo-hz 50000
#
# Arguments: the --combination list (space-separated WORD:N), the SNRs in dB,
# the seeds; anything after them goes to every loopback (e.g. --cfo-hz,
# --sample-offset, --superframes; default 6 superframes). ISDB=path/to/isdb
# selects the binary (default target/release/isdb). Needs jq.
set -eu
combos=${1:?usage: tools/isdbs-loopback-sweep.sh COMBINATIONS SNRS SEEDS [LOOPBACK OPTIONS]}
snrs=${2:?usage: tools/isdbs-loopback-sweep.sh COMBINATIONS SNRS SEEDS [LOOPBACK OPTIONS]}
seeds=${3:?usage: tools/isdbs-loopback-sweep.sh COMBINATIONS SNRS SEEDS [LOOPBACK OPTIONS]}
shift 3
isdb=${ISDB:-target/release/isdb}
layout=""
for c in $combos; do
  layout="$layout --combination $c"
done
printf '%-8s %-5s %-6s %-13s %-6s %-6s %-12s %-9s %-7s %-8s\n' \
  snr_db seed result input_exact fail lost rs_corr/all rs_uncorr mer_db trim
for snr in $snrs; do
  for seed in $seeds; do
    # shellcheck disable=SC2086 # $layout is a list of options
    out=$("$isdb" --log-level warn loopback --system isdbs $layout \
      --snr-db "$snr" --seed "$seed" --json "$@" 2>/dev/null || true)
    if [ -z "$out" ]; then
      printf '%-8s %-5s %s\n' "$snr" "$seed" "no JSON (error)"
      continue
    fi
    printf '%s\n' "$out" | jq -r --arg snr "$snr" --arg seed "$seed" '
      ([.verify[]] | map(.input_pass) | add) as $ok
      | ([.rx.ts[]] | map(.packets) | add) as $all
      | ([.verify[]] | map(.fail) | add) as $fail
      | ([.verify[]] | map(.lost) | add) as $lost
      | ([.rx.ts[]] | map(.rs_corrected_packets) | add) as $corr
      | ([.rx.ts[]] | map(.rs_uncorrectable) | add) as $unc
      | [$snr, $seed, .result, "\($ok)/\($ok + $lost)", $fail, $lost,
         "\($corr)/\($all)", $unc, ((.rx.main_mer_db // 0) * 10 | round / 10),
         ((.trim_symbols // 0) * 1000 | round / 1000)]
      | @tsv' | awk -F'\t' '{ printf "%-8s %-5s %-6s %-13s %-6s %-6s %-12s %-9s %-7s %-8s\n", $1, $2, $3, $4, $5, $6, $7, $8, $9, $10 }'
  done
done
