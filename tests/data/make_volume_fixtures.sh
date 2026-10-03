#!/usr/bin/env bash
# Regenerate the tiny AFNI volume fixtures in tests/data/volume/.
#
# Requires AFNI binaries on PATH. Voxel values are closed-form functions of
# (i, j, k, t) rather than random data, so tests can compute the expected value
# of any voxel without a lookup table. The per-dataset reference dumps
# (*.3dinfo.txt, *.aform.txt, *.dump.txt) are AFNI's own view of each file and
# are what the Rust tests assert against.
#
# Usage: tests/data/make_volume_fixtures.sh

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
out="$here/volume"
mkdir -p "$out"
cd "$out"
rm -f ./*+orig.* ./*+tlrc.* ./*.nii ./*.nii.gz ./*.txt

# Keep AFNI's output predictable regardless of the user's ~/.afnirc.
export AFNI_COMPRESSOR=NONE
export AFNI_BYTEORDER=LSB_FIRST
export AFNI_NIFTI_TYPE_WARN=NO
export AFNI_DONT_LOGFILE=YES
export AFNI_ENVIRON_WARNINGS=NO
# Keep user@host out of HISTORY_NOTE: these files are committed publicly.
export AFNI_HISTORY_NAME=afni-io

grid=jRandomDataset:4,5,6

# --- datum types (little-endian, uncompressed) -------------------------------
# short, 3 sub-bricks, signed values; v = i + 10j + 100k + 1000t - 300
3dcalc -a "$grid,3" -expr 'i+10*j+100*k+1000*t-300' -datum short -prefix s16 >/dev/null
# byte, 1 sub-brick; v = i + 4j + 20k  (max 119)
3dcalc -a "$grid,1" -expr 'i+4*j+20*k' -datum byte -prefix u8 >/dev/null
# float, 1 sub-brick, non-integer; v = (i + 10j + 100k) / 8
3dcalc -a "$grid,1" -expr '(i+10*j+100*k)/8' -datum float -prefix f32 >/dev/null
# short with a scale factor: -fscale forces BRICK_FLOAT_FACS != 0
3dcalc -a "$grid,1" -expr '(i+10*j+100*k)/8' -datum short -fscale -prefix s16scaled >/dev/null

# --- mixed datum types across sub-bricks (byte | float) ---------------------
3dbucket -prefix mixed u8+orig f32+orig >/dev/null 2>&1

# --- RGB (datum code 6) ------------------------------------------------------
3dcalc -a "$grid,1" -expr 'i*60'  -datum byte -prefix _r >/dev/null
3dcalc -a "$grid,1" -expr 'j*50'  -datum byte -prefix _g >/dev/null
3dcalc -a "$grid,1" -expr 'k*40'  -datum byte -prefix _b >/dev/null
3dThreetoRGB -prefix rgb _r+orig _g+orig _b+orig >/dev/null
rm -f _r+orig.* _g+orig.* _b+orig.*

# --- big-endian copy of s16 --------------------------------------------------
AFNI_BYTEORDER=MSB_FIRST 3dcalc -a s16+orig -expr a -datum short -prefix s16_msb >/dev/null

# --- gzipped BRIK ------------------------------------------------------------
3dcopy s16+orig s16gz >/dev/null
gzip -f s16gz+orig.BRIK

# --- stat metadata: t-test (dof 23) on #0, F-test (2, 40) on #1 -------------
3dcalc -a "$grid,2" -expr '(i+j+k)/4+t' -datum float -prefix stat >/dev/null
3drefit -substatpar 0 fitt 23 -substatpar 1 fift 2 40 \
        -sublabel 0 'Tstat#0' -sublabel 1 'Fstat~with~tildes' stat+orig >/dev/null
# (3drefit stores that label as 'Fstat*with*tildes': '~' is the BRICK_LABS separator.)

# --- timing: 3 sub-bricks as a 3D+time dataset with TR = 2 s ----------------
3dTcat -prefix timeseries s16+orig >/dev/null
3drefit -TR 2.0 timeseries+orig >/dev/null
# Same data with TR = 2.5 s and slice timing (TAXIS_OFFSETS, one per z slice).
# (3drefit refuses millisecond TRs, so the TAXIS ms case is a Rust unit test.)
3dTcat -prefix slicetimed s16+orig >/dev/null
3drefit -TR 2.5 -Tslices 0 1.25 0.25 1.5 0.5 1.75 slicetimed+orig >/dev/null

# --- non-RAI orientation, non-zero origin, anisotropic voxels ---------------
3dresample -orient LPI -prefix lpi -input s16+orig >/dev/null
3drefit -xorigin 12.5 -yorigin -7 -zorigin 3 \
        -xdel 1.5 -ydel 2 -zdel 2.5 lpi+orig >/dev/null

# --- tlrc view ---------------------------------------------------------------
3dcopy u8+orig u8tlrc+tlrc >/dev/null 2>&1 || 3dcopy u8+orig u8tlrc >/dev/null
[ -e u8tlrc+tlrc.HEAD ] || 3drefit -view tlrc u8tlrc+orig >/dev/null

# --- oblique: overwrite IJK_TO_DICOM_REAL with a 10 deg rotation about z ----
3dcopy f32+orig oblique >/dev/null
echo '0.984808 -0.173648 0 1  0.173648 0.984808 0 2  0 0 1 3' > _oblique.1D
3drefit -atrfloat IJK_TO_DICOM_REAL _oblique.1D oblique+orig >/dev/null
rm -f _oblique.1D

# --- correlation: AFNI's 3-parameter form Correl(samples, nfit, nort) -------
# afni-core must tell this apart from the NIfTI standard's 1-parameter form.
# v = (i+j+k)/20 - 0.2, which lies inside (-1, 1).
3dcalc -a "$grid,1" -expr '(i+j+k)/20-0.2' -datum float -prefix correl >/dev/null
3drefit -substatpar 0 fico 30 2 1 correl+orig >/dev/null

# --- FDR: AFNI's own FDR/MDF curves and 3dFDR z-scores for a null-plus-signal map
# 20x20x20 = 8000 voxels, enough for AFNI's true-positive estimate (needs >= 233
# voxels) and its missed-detection curve. #0 is a t(23) map, #1 an F(2,40) map;
# p-values are uniform except in every 5th/6th voxel, where p is cubed (signal).
# NOTE: jRandomDataset is random, so regenerating changes these data AND every
# reference derived from them (curves, 3dFDR output, fdrval table) together.
fdr_grid=jRandomDataset:20,20,20
3dcalc -a "$fdr_grid,1" -b "$fdr_grid,1" \
  -expr '(1-2*step(0.5-b))*fitt_p2t(0.00005+0.99990*a^(1+3*iszero(mod(i+j+k,5))),23)' \
  -datum float -prefix _fdr_t >/dev/null
3dcalc -a "$fdr_grid,1" \
  -expr 'fift_p2t(0.00005+0.99990*a^(1+3*iszero(mod(i+2*j+k,6))),2,40)' \
  -datum float -prefix _fdr_f >/dev/null
3dbucket -prefix fdr _fdr_t+orig _fdr_f+orig >/dev/null
3drefit -substatpar 0 fitt 23 -substatpar 1 fift 2 40 \
        -sublabel 0 Tnull_sig -sublabel 1 Fnull_sig -addFDR fdr+orig >/dev/null
rm -f _fdr_t+orig.* _fdr_f+orig.*
3dFDR -input fdr+orig -prefix fdr_z >/dev/null 2>&1                # independent tests
3dFDR -input fdr+orig -cdep -prefix fdr_zdep >/dev/null 2>&1       # arbitrary dependence
# fdrval: q for a threshold, and the threshold for a q (-qinput), per sub-brick.
# Format `fdrval <sub> <q|inverse> <value> => <output>` (fdrval prints 5 digits).
{
  for v in 0.5 1 1.5 2 2.5 3 3.5 4 5 8; do
    echo "fdrval 0 q $v => $(fdrval fdr+orig 0 "$v" 2>/dev/null)"
  done
  for v in 0.3 1 2 3 5 8 13 30; do
    echo "fdrval 1 q $v => $(fdrval fdr+orig 1 "$v" 2>/dev/null)"
  done
  for sub in 0 1; do
    for q in 0.5 0.3 0.1 0.05 0.01 0.001 0.000001 0.000000000001; do
      echo "fdrval $sub inverse $q => $(fdrval -qinput fdr+orig $sub "$q" 2>/dev/null)"
    done
  done
} > fdr+orig.fdrval.txt

# --- a label table (VALUE_LABEL_DTABLE), from the committed aparc table -----
3dcopy u8+orig labelled >/dev/null
3drefit -labeltable ../real/labels/aparc+aseg_REN_all.niml.lt labelled+orig >/dev/null

# --- NIfTI: with the AFNI ecode-4 extension, without it, and gzipped ---------
3dAFNItoNIFTI -prefix stat.nii stat+orig >/dev/null 2>&1
3dAFNItoNIFTI -pure -prefix stat_pure.nii stat+orig >/dev/null 2>&1
3dAFNItoNIFTI -prefix s16.nii.gz s16+orig >/dev/null 2>&1
# Correlation three ways: with the AFNI extension, pure (AFNI copies its own
# parameters 30,2,1 into intent_p1..3), and a "standards-compliant" file whose
# header holds the NIfTI one-parameter form: intent_p1 = dof 18, p2 = p3 = 0.
# The last is made by patching bytes 56..67 of the pure file (intent_p1..3, float32).
3dAFNItoNIFTI -prefix correl.nii correl+orig >/dev/null 2>&1
3dAFNItoNIFTI -pure -prefix correl_pure.nii correl+orig >/dev/null 2>&1
cp correl_pure.nii correl_standard.nii
printf '\x00\x00\x90\x41\x00\x00\x00\x00\x00\x00\x00\x00' \
  | dd of=correl_standard.nii bs=1 seek=56 conv=notrunc 2>/dev/null
3dAFNItoNIFTI -prefix lpi.nii lpi+orig >/dev/null 2>&1
3dAFNItoNIFTI -prefix oblique.nii oblique+orig >/dev/null 2>&1
3dAFNItoNIFTI -prefix labelled.nii labelled+orig >/dev/null 2>&1
3dinfo -labeltable labelled+orig > labelled+orig.labeltable.txt 2>/dev/null

# --- AFNI's own reference values ---------------------------------------------
for head in *.HEAD; do
  ds="${head%.HEAD}"
  3dinfo -verb "$ds" > "$ds.3dinfo.txt" 2>/dev/null
  3dinfo -aform_real -is_oblique -obliquity -datum -orient -nv -tr -av_space "$ds" \
    > "$ds.aform.txt" 2>/dev/null
  3dinfo -slice_timing "$ds" > "$ds.slice_timing.txt" 2>/dev/null
  # Every voxel: i j k then one value per sub-brick (scaled to true values).
  # Each sub-brick is dumped on its own and pasted together, because
  # 3dmaskdump on a whole mixed-datum dataset (byte|float) prints the float
  # sub-brick as zeros. Skipped for rgb, which 3dmaskdump cannot represent.
  case "$ds" in rgb+*) continue ;; esac
  nv=$(3dinfo -nv "$ds" 2>/dev/null)
  3dmaskdump "$ds[0]" 2>/dev/null | awk '{print $1, $2, $3}' > _ijk.txt
  cols=()
  for ((b = 0; b < nv; b++)); do
    3dmaskdump -noijk "$ds[$b]" > "_b$b.txt" 2>/dev/null
    cols+=("_b$b.txt")
  done
  paste -d' ' _ijk.txt "${cols[@]}" > "$ds.dump.txt"
  rm -f _ijk.txt _b*.txt
done
for nii in *.nii *.nii.gz; do
  3dinfo -aform_real -is_oblique -obliquity -datum -orient -nv -tr "$nii" > "$nii.aform.txt" 2>/dev/null
done

echo "wrote $(ls | wc -l | tr -d ' ') files to $out"
