#!/usr/bin/env bash
# Make tests/data/talk/: what AFNI itself says about its ports, and real
# messages AFNI sent to SUMA, so the `talk` tests can check afni-io against them.
#
#   tests/data/make_talk_fixtures.sh        (needs AFNI on PATH, and python3)
#
# ports_*.txt      `afni -list_ports` under different offsets and environments
# afni_crosshair.niml
#                  the SUMA_crosshair group AFNI sends when its crosshair moves
# afni_irgba_10.niml
#                  the first 10 rows of a SUMA_irgba AFNI sent (binary.lsbfirst,
#                  int,4*byte); rows are AFNI's own bytes, ni_dimen edited to 10
#
# The last two come from a session recorded by sumaru (`sumaru --niml-record`).
# Set SUMARU_NIML_RECORDING to a .nimlrec(.gz); if it is missing those two
# files are left as they are.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
out="$here/talk"
command -v afni >/dev/null || { echo "afni not found (is AFNI installed?)" >&2; exit 1; }
export AFNI_DONT_LOGFILE=YES
mkdir -p "$out"

list() { # name, then VAR=value settings
  local name="$1"; shift
  env -i PATH="$PATH" HOME="$HOME" "$@" afni -list_ports 2>/dev/null | grep -E '^[0-9]+: ' > "$out/ports_$name.txt"
}
list default
list np3000       AFNI_PORT_OFFSET=3000
list npb3         AFNI_PORT_BLOC=3
list npb_max      AFNI_PORT_BLOC=2686
list np_max       AFNI_PORT_OFFSET=65500
list env_ports    SUMA_AFNI_TCP_PORT=4000 SUMA_AFNI_TCP_PORT2=4001 SUMA_MATLAB_LISTEN_PORT=4005 AFNI_PLUGOUT_TCP_BASE=7000
list np_ignores   AFNI_PORT_OFFSET=3000 SUMA_AFNI_TCP_PORT=4000 AFNI_PLUGOUT_TCP_BASE=7000
# Things AFNI refuses or ignores: it falls back to the default ports.
list np_too_big   AFNI_PORT_OFFSET=65501
list npb_too_big  AFNI_PORT_BLOC=2687
list np_too_small AFNI_PORT_OFFSET=1023
list bloc_wins    AFNI_PORT_BLOC=3 AFNI_PORT_OFFSET=3000
list first_port   AFNI_NIML_FIRST_PORT=4000

rec="${SUMARU_NIML_RECORDING:-$here/../../../sumaru/testing/afni_niml/basic_func.nimlrec.gz}"
if [ -f "$rec" ]; then
python3 - "$rec" "$out" <<'PY'
import binascii, gzip, re, sys
rec, out = sys.argv[1:3]
opener = gzip.open if rec.endswith('.gz') else open
messages = []
with opener(rec, 'rt') as f:
    for line in f:
        parts = line.rstrip('\n').split('\t')
        if len(parts) >= 4:
            messages.append((parts[2], binascii.unhexlify(parts[3])))
rx = [b for d, b in messages if d == 'rx']
crosshair = next(b for b in rx if b.lstrip().startswith(b'<SUMA_crosshair'))
open(f'{out}/afni_crosshair.niml', 'wb').write(crosshair)
irgba = next(b for b in rx if b.lstrip().startswith(b'<SUMA_irgba'))
end = irgba.index(b'>') + 1
header = re.sub(rb'ni_dimen="\d+"', b'ni_dimen="10"', irgba[:end])
open(f'{out}/afni_irgba_10.niml', 'wb').write(header + irgba[end:end + 80] + b'</SUMA_irgba>\n\n')
PY
fi
echo "wrote $out"
