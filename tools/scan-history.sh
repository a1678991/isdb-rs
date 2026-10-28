#!/bin/sh
# Search every file version a revision range adds (blobs reachable from the
# range, so content introduced by merge commits counts too, unlike
# `git log -p`) and its commit messages for extended regular expressions,
# one per line of PATTERN_FILE. Run before publishing a branch:
#
#   tools/scan-history.sh origin/main..HEAD ~/private-patterns.txt
#
# Keep the pattern file outside the repository: it names what must not be
# published. Prints `path:line:text` per blob hit and `commit <hash>:text`
# per message hit; exits 1 if anything matched, 0 if nothing did.
set -eu
range=${1:?usage: tools/scan-history.sh RANGE PATTERN_FILE}
pats=${2:?usage: tools/scan-history.sh RANGE PATTERN_FILE}
found=0
git rev-list --objects "$range" |
  git cat-file --batch-check='%(objecttype) %(objectname) %(rest)' |
  awk '$1 == "blob" { print $2, $3 }' | {
  while read -r sha file; do
    if git cat-file -p "$sha" | grep -nEI -f "$pats" | sed "s|^|$file:|"; then
      found=1
    fi
  done
  [ "$found" = 0 ]
} || found=1
for c in $(git rev-list "$range"); do
  if git log -1 --format=%B "$c" | grep -EI -f "$pats" | sed "s|^|commit $c:|"; then
    found=1
  fi
done
exit "$found"
