#!/usr/bin/env bash
# Fetch danh sách repo (pinned) trong .github/benches/repos/sources.txt về
# .github/benches/repos/checkout/<name>, rồi ghi đường dẫn TUYỆT ĐỐI vào
# .github/benches/repos/list.txt để codegraph-bench (CodSpeed) đọc qua env
# CODEGRAPH_BENCH_REPOS_LIST (${{ github.workspace }}/.github/benches/repos/list.txt).
#
# Chạy local:  bash .github/benches/fetch_repos.sh
# Chạy trong CI (codspeed.yml) trước `cargo codspeed build`.
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$DIR/repos/checkout"
LIST="$DIR/repos/list.txt"
SRC="$DIR/repos/sources.txt"

mkdir -p "$OUT"
: > "$LIST"

# Mỗi dòng: <name>|<url>|<commit>
# `|| [ -n "$name" ]` xử lý dòng cuối không có trailing `\n`.
while IFS='|' read -r name url commit || [ -n "$name" ]; do
  name="$(printf '%s' "$name" | xargs)"   # trim
  [ -z "$name" ] && continue
  [[ "$name" == \#* ]] && continue
  dest="$OUT/$name"
  if [ ! -d "$dest/.git" ]; then
    echo ">> clone $name ..."
    git clone --quiet --filter=blob:none --no-checkout "$url" "$dest"
  fi
  echo ">> checkout $name @ ${commit:0:12}"
  git -C "$dest" fetch --quiet --depth 1 origin "$commit"
  git -C "$dest" checkout --quiet "$commit"
  echo "$dest" >> "$LIST"
done < "$SRC"

echo "=== repos ready (${LIST}) ==="
cat "$LIST"
