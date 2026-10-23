#!/bin/bash

XDG_DATA_HOME=${XDG_DATA_HOME:-$HOME/.local/share}

if [ -d "/opt/system/Tools/PortMaster/" ]; then
  controlfolder="/opt/system/Tools/PortMaster"
elif [ -d "/opt/tools/PortMaster/" ]; then
  controlfolder="/opt/tools/PortMaster"
elif [ -d "$XDG_DATA_HOME/PortMaster/" ]; then
  controlfolder="$XDG_DATA_HOME/PortMaster"
else
  controlfolder="/roms/ports/PortMaster"
fi

if [ -f "$controlfolder/control.txt" ]; then
  source "$controlfolder/control.txt"
fi

[ -f "${controlfolder}/mod_${CFW_NAME}.txt" ] && source "${controlfolder}/mod_${CFW_NAME}.txt"

if type get_controls >/dev/null 2>&1; then
  get_controls
fi

if [ -n "${directory:-}" ]; then
  GAMEDIR="/$directory/ports/opennow"
else
  GAMEDIR="$(cd "$(dirname "$0")" && pwd)/opennow"
fi
DATADIR="$GAMEDIR/data"

mkdir -p "$DATADIR"
cd "$GAMEDIR" || exit 1

> "$GAMEDIR/log.txt" && exec > >(tee "$GAMEDIR/log.txt") 2>&1

export XDG_DATA_HOME="$DATADIR"
export OPENNOW_DATA_DIR="$DATADIR"

# RK3326 exposes the H.264 decoder as /dev/vpu_service, owned by root.
if [ -e /dev/vpu_service ] && [ ! -w /dev/vpu_service ]; then
  if [ "$(id -u)" -eq 0 ]; then
    chmod a+rw /dev/vpu_service
  elif [ -n "${ESUDO:-}" ]; then
    $ESUDO chmod a+rw /dev/vpu_service 2>/dev/null || true
  else
    sudo -n chmod a+rw /dev/vpu_service 2>/dev/null || true
  fi
fi

arch="${DEVICE_ARCH:-aarch64}"

pm_sdl=""
for candidate in \
  "$controlfolder/libs/libSDL2-2.0.so.0" \
  "$controlfolder/libs/libSDL2-2.0.so" \
  "$controlfolder/libs.${arch}/libSDL2-2.0.so.0" \
  "$controlfolder/libSDL2-2.0.so.0"
do
  if [ -f "$candidate" ]; then
    export OPENNOW_SDL2="$candidate"
    pm_sdl="$(dirname "$candidate")"
    break
  fi
done
if [ -z "$pm_sdl" ]; then
  echo "PortMaster SDL was not found. Using the system SDL."
fi
export LD_LIBRARY_PATH="${pm_sdl:+$pm_sdl:}$GAMEDIR/libraries${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

if [ -n "${sdl_controllerconfig:-}" ]; then
  # Face buttons, sticks, and the D-pad come from PortMaster. Select and Start
  # stay on this port's own buttons.
  filtered=""
  while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    cleaned=""
    IFS=','
    for field in $line; do
      case "$field" in
        back:*|start:*) continue ;;
      esac
      if [ -z "$cleaned" ]; then
        cleaned="$field"
      else
        cleaned="$cleaned,$field"
      fi
    done
    unset IFS
    filtered="${filtered}${cleaned}"$'\n'
  done <<EOF
$sdl_controllerconfig
EOF
  export SDL_GAMECONTROLLERCONFIG="$filtered"
fi
export SDL_GAMECONTROLLER_USE_BUTTON_LABELS=0

chmod +x ./opennow-handheld ./opennow-core

if type pm_platform_helper >/dev/null 2>&1; then
  pm_platform_helper "$GAMEDIR/opennow-handheld"
fi

./opennow-handheld "$@"
status=$?

if type pm_finish >/dev/null 2>&1; then
  pm_finish
fi

exit "$status"