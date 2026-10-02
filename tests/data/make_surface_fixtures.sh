#!/usr/bin/env bash
# Regenerate the tiny synthetic SUMA fixtures in tests/data/surface/.
#
# Requires AFNI/SUMA binaries on PATH. Everything is derived from a 42-node,
# 80-triangle icosahedron, so no subject anatomy is involved. Dataset values are
# closed-form functions of the node index n and column c, so tests can compute
# expected values directly:
#
#   dense.*      : 42 rows, 3 columns, v = n + 100*c       (node n in row n)
#   sparse.*     : 11 rows (nodes 0,4,8,...,40), 2 columns, v = n / 4 + c
#   stat.*       : 42 rows, 2 columns, v = n / 10 + c, Ttest(10) and Ftest(2,30)
#
# Usage: tests/data/make_surface_fixtures.sh

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/surface"
mkdir -p "$out"
cd "$out"
rm -f ./*.asc ./*.gii ./*.gii.dset ./*.ply ./*.1D* ./*.spec ./*.niml.* ./*.txt

export AFNI_ENVIRON_WARNINGS=NO
export AFNI_DONT_LOGFILE=YES
# Keep user@host out of HISTORY_NOTE: these files are committed publicly.
export AFNI_HISTORY_NAME=afni-io

# --- surfaces ----------------------------------------------------------------
CreateIcosahedron -ld 2 -rad 50 -prefix ico >/dev/null 2>&1
ConvertSurface -i ico.asc -o ico_ascii.gii -xml_ascii >/dev/null 2>&1
ConvertSurface -i ico.asc -o ico_b64.gii -xml_b64 >/dev/null 2>&1
ConvertSurface -i ico.asc -o ico_b64gz.gii -xml_b64gz >/dev/null 2>&1
ConvertSurface -i ico.asc -o_ply ico.ply >/dev/null 2>&1
ConvertSurface -i ico.asc -o_1D ico.1D.coord ico.1D.topo >/dev/null 2>&1
# A second, mirrored state with identical topology, for spec / domain-parent tests.
ConvertSurface -i ico.asc -o_fs ico_mirror.asc -xmat_1D 'NegXY' >/dev/null 2>&1 \
  || ConvertSurface -i ico.asc -o_fs ico_mirror.asc >/dev/null 2>&1
quickspec -tsnad FS smoothwm ico.asc Y SAME -tsnad FS mirrored ico_mirror.asc N ico.asc -spec ico_states.spec >/dev/null 2>&1
rm -f quickspec.log

# --- datasets ----------------------------------------------------------------
awk 'BEGIN{for(n=0;n<42;n++) printf "%d %d %d\n", n, n+100, n+200}' > _dense.1D
awk 'BEGIN{for(n=0;n<42;n+=4) printf "%g %g\n", n/4, n/4+1}'          > _sparse.1D
awk 'BEGIN{for(n=0;n<42;n+=4) printf "%d\n", n}'                        > _sparse_idx.1D
awk 'BEGIN{for(n=0;n<42;n++) printf "%g %g\n", n/10, n/10+1}'           > _stat.1D

for fmt in niml_asc niml_bi gii_asc gii_b64 gii_b64gz; do
  case "$fmt" in
    niml_asc)  ext=niml.dset ;    tag=asc ;;
    niml_bi)   ext=niml.dset ;    tag=bi ;;
    gii_asc)   ext=gii.dset ;     tag=asc ;;
    gii_b64)   ext=gii.dset ;     tag=b64 ;;
    gii_b64gz) ext=gii.dset ;     tag=b64gz ;;
  esac
  ConvertDset -i _dense.1D -add_node_index -o_$fmt -prefix "dense_$tag.$ext" >/dev/null 2>&1
  ConvertDset -i _sparse.1D -node_index_1D _sparse_idx.1D -o_$fmt \
              -prefix "sparse_$tag.$ext" >/dev/null 2>&1
done

ConvertDset -i _stat.1D -add_node_index -o_niml_asc -prefix stat.niml.dset >/dev/null 2>&1
3drefit -substatpar 0 fitt 10 -substatpar 1 fift 2 30 \
        -sublabel 0 'T#0' -sublabel 1 'F#1' stat.niml.dset >/dev/null 2>&1
# The same statistics as GIfTI: Intent codes plus intent_p1..3 metadata.
ConvertDset -i stat.niml.dset -o_gii_asc -prefix stat.gii.dset >/dev/null 2>&1

# Labels: a 4-entry colour lookup table (MakeColorMap's own example), as a
# SUMA label table, and a node-label dataset that carries it (n % 4 + 1).
printf '%s\n' '#integer label    String Label      R    G    B    A' \
  ' 1 Big_House 0.3 0.1 1 1' ' 2 Small_Face 1 0.2 0.4 1' \
  ' 3 Electric 1 1 0 1' ' 4 Atomic 0.1 1 0.3 1' > _colut.txt
MakeColorMap -usercolutfile _colut.txt -suma_cmap toylut >/dev/null 2>&1
awk 'BEGIN{for(n=0;n<42;n++) printf "%d\n", n%4+1}' > _lab.1D
ConvertDset -i _lab.1D -add_node_index -o_niml_asc -prefix labels.niml.dset \
            -labelize toylut.niml.cmap >/dev/null 2>&1

# FDR curves on the stat dataset (FDRCURVE_000000, _000001).
cp stat.niml.dset fdr.niml.dset
3drefit -addFDR fdr.niml.dset >/dev/null 2>&1

# A time series: 3 columns, v = n + c, TR = 2.5 s (SPARSE_DATA ni_timestep).
awk 'BEGIN{for(n=0;n<42;n++) printf "%d %d %d\n", n, n+1, n+2}' > _ts.1D
ConvertDset -i _ts.1D -add_node_index -o_niml_asc -prefix timeseries.niml.dset >/dev/null 2>&1
3drefit -TR 2.5 timeseries.niml.dset >/dev/null 2>&1

rm -f _*.1D _colut.txt
# Node coordinates as SUMA reports them.
SurfaceMetrics -i ico.asc -coords -prefix ico_ref >/dev/null 2>&1 || true

# --- scrub user@host ---------------------------------------------------------
# SUMA programs call tross_username()/tross_hostname() directly and ignore
# AFNI_HISTORY_NAME, so rewrite "[user@host:" in their history strings. Safe
# for NIML and GIfTI: string bodies carry no byte counts (binary NIML only
# counts numeric payloads). Never do this to a .HEAD, whose string attributes
# do record their length.
perl -0777 -pi -e 's/\[[^\]\[@: \n]*@[^\]\[: \n]*:/[afni-io@:/g' ./*.spec ./*.dset ./*.cmap

# --- reference text dumps ----------------------------------------------------
# Layout: node index, then one value per column. ConvertDset segfaults on
# -prepend_node_index_1D for a dense .gii.dset (no NODE_INDEX array), so dense
# files fall back to a plain dump with the row number as the node index.
for d in *.niml.dset; do
  3dinfo -label -nv -tr "$d" > "$d.info.txt" 2>/dev/null || true
done
for d in *.niml.dset *.gii.dset; do
  ConvertDset -i "$d" -o_1D_stdout -prepend_node_index_1D > "$d.dump.txt" 2>/dev/null \
    || ConvertDset -i "$d" -o_1D_stdout 2>/dev/null \
       | awk '/^#/ || NF == 0 {print; next} {print n++, $0}' > "$d.dump.txt"
done

echo "wrote $(ls | wc -l | tr -d ' ') files to $out"
