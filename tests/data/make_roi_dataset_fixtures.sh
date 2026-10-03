#!/usr/bin/env bash
# Regenerate tests/data/roi_dataset/: what AFNI's own `ROI2dataset` makes from the
# ROI files in tests/data/real/roi/. Used by tests/roi_conformance.rs to check
# afni-core's ROI-to-dataset conversion and node ordering.
#
#   tests/data/make_roi_dataset_fixtures.sh        (needs AFNI on PATH)
#
# For every case this writes
#   <case>.dset      rows "node label" (the `-of 1D` output without comments)
#   <case>.nodes     for single-file cases: the nodes in drawn order, repeats kept
#                    (`-nodelist`), per label, one node per line
#   <case>.nodups    the same without repeats (`-nodelist.nodups`)
#   <case>.pad       padded cases only: "rows N", the full row count (the .dset then
#                    holds just the rows of ROI nodes)
# and one line per case in cases.txt:
#   <case> | <files, comma separated> | <extra options>
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
roi="$here/real/roi"
out="$here/roi_dataset"
command -v ROI2dataset >/dev/null || { echo "ROI2dataset not found (is AFNI installed?)" >&2; exit 1; }
export AFNI_DONT_LOGFILE=YES
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out" "$out/inputs"

# Derived inputs: copies of real ROIs with another integer label, so that ROIs with
# DIFFERENT labels share nodes (the real files either share no nodes or share a label).
for n in 2 3; do
  sed -e "s/^#  iLabel = \"[0-9]*\"/#  iLabel = \"$n\"/" -e "s/^#  Label = \"[^\"]*\"/#  Label = \"copy_$n\"/" \
      "$roi/demo.lh.3.filled.niml.roi" > "$out/inputs/demo.lh.3.filled.label$n.niml.roi"
done

# The biggest node number in a .niml.roi file (all numbers in the data block).
max_node() {
  awk '!/^#/ { for (i = 3; i <= NF; i++) if ($i + 0 > m) m = $i + 0 } END { print m }' "$1"
}

: > "$out/cases.txt"
run_case() {   # name, "file1,file2", extra options
  local name="$1" files="$2" extra="${3:-}"
  local inputs=()
  IFS=',' read -ra parts <<< "$files"
  for f in "${parts[@]}"; do
    if [ -f "$out/inputs/$f" ]; then inputs+=("$out/inputs/$f"); else inputs+=("$roi/$f"); fi
  done
  rm -f "$work"/o* "$work"/NL*
  (cd "$work" && ROI2dataset -prefix o -of 1D $extra -input "${inputs[@]}" >/dev/null 2>&1)
  grep -v '^#' "$work/o.1D.dset" | grep -v '^ *$' > "$work/full.dset"
  if [[ "$extra" == *-pad_to_node* ]]; then
    # A padded dataset has a row per node up to the limit (130 thousand lines): keep
    # only the rows of ROI nodes (label != the pad label) and the row counts.
    local pad=0
    [[ "$extra" == *-pad_label* ]] && pad="${extra##*-pad_label }"
    awk -v pad="$pad" '$2 != pad' "$work/full.dset" > "$out/$name.dset"
    echo "rows $(wc -l < "$work/full.dset" | tr -d ' ')" > "$out/$name.pad"
  else
    cp "$work/full.dset" "$out/$name.dset"
  fi
  if [ "${#parts[@]}" = 1 ] && [[ "$extra" != *-pad_to_node* ]]; then
    # -nodelist keeps every node as drawn; -nodelist.nodups keeps first occurrences.
    for kind in nodes nodups; do
      flag="-nodelist"; [ "$kind" = nodups ] && flag="-nodelist.nodups"
      rm -f "$work"/NL*
      (cd "$work" && ROI2dataset $flag NL -input "${inputs[@]}" >/dev/null 2>&1) || true
      # one file per integer label; keep them in label order, one node per line
      : > "$out/$name.$kind"
      for nl in $(ls "$work"/NL.*.1D 2>/dev/null | sort -t. -k2 -n); do
        echo "label $(basename "$nl" | cut -d. -f2)" >> "$out/$name.$kind"
        grep -v '^#' "$nl" | awk 'NF { print $1 }' >> "$out/$name.$kind"
      done
    done
  fi
  echo "$name | $files | $extra" >> "$out/cases.txt"
}

# Every ROI file on its own (except the 750 KB "bigfill" one, to keep the fixtures small;
# it is still round-tripped by the adapter tests).
for f in "$roi"/*.niml.roi; do
  b="$(basename "$f")"
  case "$b" in *bigfill*) continue ;; esac
  run_case "single_${b%.niml.roi}" "$b"
done
# Overlaps: the FIRST ROI should keep the shared nodes, whichever order they come in.
run_case overlap_demo_1_3 "demo.lh.1.nojoin.niml.roi,demo.lh.3.filled.niml.roi"
run_case overlap_demo_3_1 "demo.lh.3.filled.niml.roi,demo.lh.1.nojoin.niml.roi"
run_case overlap_roi_1_2 "roi_1.lh.inflated.niml.roi,roi_2.niml.roi"
run_case overlap_roi_2_1 "roi_2.niml.roi,roi_1.lh.inflated.niml.roi"
run_case overlap_same_label "roi_2.niml.roi,test.roi_2.niml.roi"
# Different labels on the same nodes: who keeps them?
run_case contested_1_2 "demo.lh.3.filled.niml.roi,demo.lh.3.filled.label2.niml.roi"
run_case contested_2_1 "demo.lh.3.filled.label2.niml.roi,demo.lh.3.filled.niml.roi"
run_case contested_3_way "demo.lh.1.nojoin.niml.roi,demo.lh.3.filled.label3.niml.roi,demo.lh.3.filled.label2.niml.roi"
# Padding.
m="$(max_node "$roi/demo.lh.3.filled.niml.roi")"
run_case "pad_default" "demo.lh.3.filled.niml.roi" "-pad_to_node $((m + 25))"
run_case "pad_label" "demo.lh.3.filled.niml.roi" "-pad_to_node $((m + 25)) -pad_label 77"
echo "wrote $(wc -l < "$out/cases.txt") cases to $out"
