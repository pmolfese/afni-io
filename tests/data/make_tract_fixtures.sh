#!/usr/bin/env bash
# Make tests/data/tract/: `.niml.tract` files written by AFNI's OWN FATCAT code
# (ptaylor/TrackIO.c: Network_2_NIgr plus NI_write_element, ASCII and binary), from a
# small network built in memory, together with the network as it was built and the
# lengths AFNI's `Tract_Length` reports.
#
#   AFNI_SRC=~/Documents/Programming/afni tests/data/make_tract_fixtures.sh
#
# 3dTrackID needs diffusion data, so this compiles TrackIO.c itself into a tiny C
# program linked against AFNI's libmri and libSUMA (built in $AFNI_SRC/build/
# targets_built). Needs a C compiler. Outputs:
#   net_ascii.niml.tract, net_bin.niml.tract
#   net.truth    "bundle <tag> <alt tag> <ends>" then "tract <id> <npts> x y z ..."
#   net.lengths  "<bundle> <tract> <Tract_Length>"
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/tract"
afni="${AFNI_SRC:-$HOME/Documents/Programming/afni}"
lib="$afni/build/targets_built"
[ -f "$afni/src/ptaylor/TrackIO.c" ] && [ -d "$lib" ] || { echo "AFNI source/build not found at $afni (set AFNI_SRC)" >&2; exit 1; }
command -v cc >/dev/null || { echo "need a C compiler" >&2; exit 1; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

inc="-I$afni/src -I$afni/src/ptaylor -I$afni/src/SUMA -I$afni/src/nifti/nifti2 -I$afni/src/nifti/nifti_clib -I$afni/src/niml -I$afni/src/rickr -I$afni/src/f2c -I$afni/src/f2cdir -I$afni/src/eispack"
for d in "$afni"/src/nifti/*; do [ -d "$d" ] && inc="$inc -I$d"; done
for d in /opt/homebrew/include /opt/X11/include /usr/local/include; do [ -d "$d" ] && inc="$inc -I$d"; done

cat > "$work/harness.c" <<'C'
#include "mrilib.h"
#include "TrackIO.h"
/* Three bundles of tracts with varied lengths: ids are not consecutive, one tract has a
   single point, one bundle has no ends label and no alternate tag. */
int main(void){
  const int nb = 3;
  int ntr[3] = {3, 2, 1};
  int tags[3] = {7, 12, 3}, alts[3] = {3, -1, 9};
  const char *ends[3] = {"A-B", NULL, "left to right"};
  TAYLOR_NETWORK *net = (TAYLOR_NETWORK*)calloc(1,sizeof(TAYLOR_NETWORK));
  net->N_tbv = nb; net->tbv=(TAYLOR_BUNDLE**)calloc(nb,sizeof(TAYLOR_BUNDLE*));
  net->bundle_tags=(int*)calloc(nb,sizeof(int)); net->bundle_alt_tags=(int*)calloc(nb,sizeof(int));
  FILE *truth = fopen("net.truth","w"), *lens = fopen("net.lengths","w");
  int id = 100;
  for(int b=0;b<nb;b++){
    TAYLOR_BUNDLE *tb=(TAYLOR_BUNDLE*)calloc(1,sizeof(TAYLOR_BUNDLE));
    tb->N_tracts=ntr[b]; tb->tracts=(TAYLOR_TRACT*)calloc(ntr[b],sizeof(TAYLOR_TRACT));
    if(ends[b]) tb->bundle_ends=strdup(ends[b]);
    net->tbv[b]=tb; net->bundle_tags[b]=tags[b]; net->bundle_alt_tags[b]=alts[b];
    fprintf(truth,"bundle %d %d %s\n",tags[b],alts[b],ends[b]?ends[b]:"-");
    for(int t=0;t<ntr[b];t++){
      int np = (b==2) ? 1 : 2 + 2*t + b;
      TAYLOR_TRACT *tt = tb->tracts+t; tt->id = id; id += 3; tt->N_pts3 = 3*np;
      tt->pts=(float*)calloc(3*np,sizeof(float));
      fprintf(truth,"tract %d %d",tt->id,np);
      for(int p=0;p<np;p++){
        tt->pts[3*p]   = -20.0f + 3.5f*p + 7*b - t;
        tt->pts[3*p+1] = 15.25f - 2.0f*p*p*0.25f + 4*t;
        tt->pts[3*p+2] = 0.5f*p*(b+1) - 6*t;
        fprintf(truth," %.9g %.9g %.9g",tt->pts[3*p],tt->pts[3*p+1],tt->pts[3*p+2]);
      }
      fprintf(truth,"\n");
      fprintf(lens,"%d %d %.9g\n",b,t,Tract_Length(tt));
    }
  }
  fclose(truth); fclose(lens);
  NI_group *ngr=Network_2_NIgr(net,1);
  NI_stream ns=NI_stream_open("file:net_ascii.niml.tract","w"); NI_write_element(ns,ngr,NI_TEXT_MODE); NI_stream_close(ns);
  ns=NI_stream_open("file:net_bin.niml.tract","w"); NI_write_element(ns,ngr,NI_BINARY_MODE); NI_stream_close(ns);
  return 0; }
C
cc -O1 $inc "$work/harness.c" "$afni/src/ptaylor/TrackIO.c" -L"$lib" -lmri -lSUMA -Wl,-rpath,"$lib" -o "$work/harness" 2>"$work/cc.log" \
  || { cat "$work/cc.log" >&2; exit 1; }
(cd "$out" && rm -f net_* net.truth net.lengths && "$work/harness" >/dev/null 2>&1)
echo "wrote $(ls "$out" | wc -l | tr -d ' ') files to $out"
