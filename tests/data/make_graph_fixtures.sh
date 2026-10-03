#!/usr/bin/env bash
# Make tests/data/graph/: real Graph_Bucket files written by AFNI's `ConvertDset
# -graphize` from small inputs whose contents are known, plus those inputs, so the
# tests can check what a reader recovers against what was put in.
#
#   tests/data/make_graph_fixtures.sh        (needs AFNI on PATH)
#
# Cases (inputs are written beside each result as <case>.truth):
#   full      4 nodes with names, a 16-row matrix with 2 measures (-onegraph, column-stacked)
#   lpi       the same nodes with coordinates given in LPI (AFNI flips them to RAI)
#   tri       4 nodes, one measure of 6 values: AFNI recognizes a lower triangle
#   sparse    nodes numbered 5..8 and an edge list naming nodes by index
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/graph"
command -v ConvertDset >/dev/null || { echo "ConvertDset not found (is AFNI installed?)" >&2; exit 1; }
export AFNI_DONT_LOGFILE=YES
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
rm -f "$out"/*.niml.dset "$out"/*.truth

# Inputs.
awk 'BEGIN{for (r = 0; r < 4; r++) for (c = 0; c < 4; c++) printf "%d %.3f\n", r * 10 + c, (r * 10 + c) / 10.0}' > "$work/m.1D"
printf "0 0 0\n1 10 -4\n2 -6 10\n3 10 10\n" > "$work/xyz.1D"
printf "0 Alpha\n1 Beta\n2 Gamma\n3 Delta\n" > "$work/names.txt"
printf "1\n2\n3\n4\n5\n6\n" > "$work/tri.1D"
printf "5\n6\n7\n8\n" > "$work/idx.1D"
printf "5 6\n6 7\n8 5\n" > "$work/edges.1D"
printf "1 10\n2 20\n3 30\n" > "$work/sparse.1D"

make() {   # name, then the ConvertDset arguments
  local name="$1"; shift
  # Run in the work directory with relative input names, so the dataset's `filename`
  # and history string name no temporary paths.
  (cd "$work" && ConvertDset "$@" -o_niml_asc -prefix "$name.niml.dset" >/dev/null 2>&1)
  # SUMA writes the user and host into the history string; same scrub as the other fixtures.
  perl -0777 -pi -e 's/\[[^\]\[@: \n]*@[^\]\[: \n]*:/[afni-io@:/g' "$work/$name.niml.dset"
  mv "$work/$name.niml.dset" "$out/$name.niml.dset"
}
make full   -i m.1D      -graphize -onegraph    -graph_named_nodelist_txt names.txt xyz.1D
make lpi    -i m.1D      -graphize -onegraph    -graph_named_nodelist_txt names.txt xyz.1D -graph_XYZ_LPI
make tri    -i tri.1D    -graphize -multigraph  -graph_named_nodelist_txt names.txt xyz.1D
make sparse -i sparse.1D -graphize -multigraph  -graph_nodelist_1D idx.1D xyz.1D -graph_edgelist_1D edges.1D

cp "$work/m.1D" "$out/full.truth";  cp "$work/m.1D" "$out/lpi.truth"
cp "$work/tri.1D" "$out/tri.truth"; cp "$work/sparse.1D" "$out/sparse.truth"
cp "$work/xyz.1D" "$out/nodes.xyz.truth"; cp "$work/names.txt" "$out/nodes.names.truth"
cp "$work/edges.1D" "$out/sparse.edges.truth"; cp "$work/idx.1D" "$out/sparse.index.truth"
echo "wrote $(ls "$out"/*.niml.dset | wc -l | tr -d ' ') graphs to $out"
