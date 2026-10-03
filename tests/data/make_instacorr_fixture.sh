#!/usr/bin/env bash
# Make tests/data/surface/ts_groups.niml.dset: a 42-node surface time series long
# enough for seed correlation (40 time points, TR 2 s). Nodes 0..20 follow one smooth
# signal and nodes 21..41 another, each with a slow drift and a little pseudo-random
# noise, so correlations are strong within a group and weak between groups.
#
#   tests/data/make_instacorr_fixture.sh        (needs AFNI on PATH)
#
# Used by tests/instacorr_dataset.rs, which feeds it through afni-core's seed
# correlation. Not part of make_surface_fixtures.sh so that script's other outputs stay
# byte-identical when it is rerun.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
command -v ConvertDset >/dev/null || { echo "ConvertDset not found (is AFNI installed?)" >&2; exit 1; }
export AFNI_DONT_LOGFILE=YES
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# One row per node, one column per time point. rand() is seeded, so reruns agree.
awk 'BEGIN{
  srand(5);
  for (n = 0; n < 42; n++) {
    line = "";
    for (t = 0; t < 40; t++) {
      g = (n < 21) ? sin(0.45 * t) + 0.5 * cos(0.2 * t) : cos(0.8 * t + 1) + 0.4 * sin(0.3 * t);
      v = 100 + 0.05 * t + 2.0 * g * (1 + 0.02 * n) + 0.25 * (rand() - 0.5);
      line = line sprintf("%.5f ", v);
    }
    print line;
  }
}' > "$work/ts.1D"

rm -f "$here/surface/ts_groups.niml.dset"   # ConvertDset will not overwrite
# Run in the destination so the dataset's `filename` attribute is the relative
# "./ts_groups.niml.dset" (as in the other fixtures), not a temporary path.
(cd "$here/surface" && ConvertDset -i "$work/ts.1D" -add_node_index -o_niml_asc \
    -prefix ts_groups.niml.dset >/dev/null 2>&1)
(cd "$here/surface" && 3drefit -TR 2.0 ts_groups.niml.dset >/dev/null 2>&1)
# SUMA writes the user and host into the history string; same scrub as
# make_surface_fixtures.sh (safe for NIML text; never do this to a .HEAD).
perl -0777 -pi -e 's/\[[^\]\[@: \n]*@[^\]\[: \n]*:/[afni-io@:/g' "$here/surface/ts_groups.niml.dset"
echo "wrote $here/surface/ts_groups.niml.dset"
