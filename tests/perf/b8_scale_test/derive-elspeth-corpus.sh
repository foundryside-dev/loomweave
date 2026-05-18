#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: derive-elspeth-corpus.sh <elspeth-checkout> <output-dir>

Copies the Python source corpus used by the B.8/Phase 3 scale gate into a clean
directory while recording the elspeth commit and dirty status that produced it.
The output directory must not already exist.
USAGE
}

if [[ $# -ne 2 ]]; then
  usage
  exit 2
fi

elspeth_root=$1
output_dir=$2

if [[ ! -d "$elspeth_root/.git" ]]; then
  echo "elspeth checkout not found: $elspeth_root" >&2
  exit 1
fi

if [[ -e "$output_dir" ]]; then
  echo "output directory already exists: $output_dir" >&2
  exit 1
fi

mkdir -p "$output_dir"
elspeth_root=$(cd "$elspeth_root" && pwd -P)
output_dir=$(cd "$output_dir" && pwd -P)

git -C "$elspeth_root" rev-parse HEAD >"$output_dir/elspeth-commit.txt"
git -C "$elspeth_root" status --short >"$output_dir/elspeth-dirty-status.txt"

git -C "$elspeth_root" ls-files -co --exclude-standard '*.py' \
  | sort \
  | while IFS= read -r relative_path; do
      source_path="$elspeth_root/$relative_path"
      mkdir -p "$output_dir/$(dirname "$relative_path")"
      cp -p "$source_path" "$output_dir/$relative_path"
    done

find "$output_dir" -type f -name '*.py' | sort >"$output_dir/corpus-copy.txt"
echo "$output_dir"
