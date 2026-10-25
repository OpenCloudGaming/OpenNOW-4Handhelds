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
  [ -f "${controlfolder}/mod_${CFW_NAME}.txt" ] && source "${controlfolder}/mod_${CFW_NAME}.txt"
  get_controls
else
  echo "PortMaster control.txt was not found."
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

export LD_LIBRARY_PATH="$GAMEDIR/libraries${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# Same controller list PortMaster gives every port. Select and Start are still
# this port's own buttons; the gamepad map supplies the rest.
db="${SDL_GAMECONTROLLERCONFIG_FILE:-}"
if [ -n "$db" ] && [ -f "$db" ]; then
  db_count=$(grep -c . "$db" 2>/dev/null || true)
  db_count=${db_count:-0}
  echo "controls: loaded gamecontrollerdb $db (${db_count} mappings, device ${DEVICE:-any})"
elif [ -f "$controlfolder/gamecontrollerdb.txt" ]; then
  echo "controls: gamecontrollerdb $controlfolder/gamecontrollerdb.txt was not copied into the selected config"
else
  echo "controls: gamecontrollerdb was not found"
fi
if [ -n "${sdl_controllerconfig:-}" ]; then
  export SDL_GAMECONTROLLERCONFIG="$sdl_controllerconfig"
  config_count=0
  while IFS= read -r line || [ -n "$line" ]; do
    [ -z "$line" ] && continue
    config_count=$((config_count + 1))
    if [ "$config_count" -le 8 ]; then
      echo "controls: selected gamepad config $line"
    fi
  done <<EOF
$sdl_controllerconfig
EOF
  if [ "$config_count" -gt 8 ]; then
    echo "controls: selected gamepad config has $config_count mappings"
  fi
else
  echo "controls: selected gamepad config was empty"
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