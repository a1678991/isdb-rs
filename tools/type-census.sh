#!/bin/sh
# Census of the places where the type system is not yet carrying a spec
# constraint: per crate, the clippy cast lints that the workspace does not
# enable yet, `pub` fields typed as a bare integer, and `#[allow]`s. The
# numbers drive docs/plans/type-safety.md; run it before and after a
# newtype/enum change to see what the change retired. Read-only.
#
# Usage: tools/type-census.sh [crate ...]      (default: every crate)
#   CASTS=0   skip the clippy run (slow on a cold target directory)
set -eu
cd "$(dirname "$0")/.."
crates=${*:-$(ls crates)}
S=${TMPDIR:-/tmp}/type-census.$$
trap 'rm -f "$S"' EXIT

if [ "${CASTS:-1}" != 0 ]; then
  cargo clippy --workspace --all-targets --locked --message-format=short -- \
    -W clippy::cast_possible_truncation -W clippy::cast_sign_loss \
    -W clippy::cast_possible_wrap -W clippy::cast_lossless \
    -W clippy::cast_precision_loss 2>&1 | grep -E '^crates/' > "$S" || true
  printf '%-15s %9s %9s %9s %9s  %10s %7s\n' crate truncate lossless sign/wrap precision 'pub int' allow
else
  : > "$S"
  printf '%-15s  %10s %7s\n' crate 'pub int' allow
fi

for c in $crates; do
  pubint=$(grep -rhoE 'pub [a-z_0-9]+: (u8|u16|u32|u64|usize|i8|i16|i32|i64),' "crates/$c/src" 2>/dev/null | wc -l)
  allow=$(grep -rhoE '#!?\[allow\(' "crates/$c/src" 2>/dev/null | wc -l)
  if [ "${CASTS:-1}" != 0 ]; then
    t=$(grep -c "^crates/$c/.*may truncate" "$S" || true)
    l=$(grep -c "^crates/$c/.*infallibly" "$S" || true)
    s=$(grep -cE "^crates/$c/.*(lose the sign|wrap around)" "$S" || true)
    p=$(grep -c "^crates/$c/.*loss of precision" "$S" || true)
    printf '%-15s %9s %9s %9s %9s  %10s %7s\n' "$c" "$t" "$l" "$s" "$p" "$pubint" "$allow"
  else
    printf '%-15s  %10s %7s\n' "$c" "$pubint" "$allow"
  fi
done
