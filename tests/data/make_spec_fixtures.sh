#!/usr/bin/env bash
# Make the AFNI reference output for tests/data/spec/*.spec, with `inspec`.
#
#   tests/data/make_spec_fixtures.sh        (needs AFNI on PATH)
#
# For each good spec:
#   <name>.inspec.txt   `inspec -spec <name>.spec -detail 3`: every field AFNI
#                       resolved (inherited values, defaults, path prefix)
#   <name>.rewrite.spec `inspec -spec <name>.spec -prefix ...`: the file AFNI's
#                       own writer (SUMA_Write_SpecFile) makes from them
# Every bad_*.spec must be refused by AFNI (the script stops if one is not);
# the tests check that afni-io refuses them too.
#
# Run from here, with the spec given by a bare name, so AFNI's path prefix is
# `./` and the output does not depend on where the repository is.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/spec"
command -v inspec >/dev/null || { echo "inspec not found (is AFNI installed?)" >&2; exit 1; }
export AFNI_DONT_LOGFILE=YES
rm -f ./*.inspec.txt ./*.rewrite.spec

for spec in ./*.spec; do
  name="$(basename "$spec" .spec)"
  case "$name" in
    bad_*)
      if inspec -spec "$name.spec" -detail 1 >/dev/null 2>&1; then
        echo "AFNI accepted $name.spec, which is meant to be bad" >&2; exit 1
      fi ;;
    *)
      inspec -spec "$name.spec" -detail 3 > "$name.inspec.txt" 2>/dev/null
      tmp="$(mktemp -d)"
      inspec -spec "$name.spec" -prefix "$tmp/out" >/dev/null 2>&1
      cp "$tmp/out.spec" "$name.rewrite.spec"
      rm -rf "$tmp" ;;
  esac
done
echo "wrote reference output in $here/spec"
