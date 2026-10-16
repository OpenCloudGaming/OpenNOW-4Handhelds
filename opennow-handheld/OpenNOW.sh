#!/bin/bash
# EmulationStation Ports launcher. Copy this script and the opennow/ folder into
# the ports directory (for example /roms/ports). Output goes to opennow/opennow.log.
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/opennow" || exit 1
chmod +x ./opennow-handheld

# RK3326 exposes the H.264 decoder as /dev/vpu_service, owned by root.
# MPP cannot open it until the port user can read and write the node.
if [ -e /dev/vpu_service ] && [ ! -w /dev/vpu_service ]; then
    if [ "$(id -u)" -eq 0 ]; then
        chmod a+rw /dev/vpu_service
    else
        sudo -n chmod a+rw /dev/vpu_service 2>/dev/null || true
    fi
fi
export LD_LIBRARY_PATH="$here/opennow/libraries${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
./opennow-handheld "$@" > "$here/opennow/opennow.log" 2>&1
