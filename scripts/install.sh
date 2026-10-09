#!/bin/sh
# Install eoview for the user on Linux or macOS. eoview then updates itself at each start.
#   curl -fsSL https://github.com/polymood/eoview/releases/latest/download/install.sh | sh
# EOVIEW_INSTALL_DIR: the directory of the executable (default ~/.local/bin).
set -e
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) a=eoview-linux-x86_64 ;;
    Darwin-arm64) a=eoview-macos-aarch64 ;;
    *) echo "eoview: no release for $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac
url=https://github.com/polymood/eoview/releases/latest/download
d=${EOVIEW_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$d"
curl -fSL "$url/$a" -o "$d/eoview.new"
chmod 755 "$d/eoview.new"
mv "$d/eoview.new" "$d/eoview"
# Linux: an entry in the menu of applications.
if [ "$(uname -s)" = Linux ]; then
    share=${XDG_DATA_HOME:-$HOME/.local/share}
    mkdir -p "$share/applications" "$share/icons/hicolor/256x256/apps"
    curl -fsSL "$url/eoview.png" -o "$share/icons/hicolor/256x256/apps/eoview.png" || true
    cat > "$share/applications/eoview.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=eoview
Comment=Fast viewer for Earth observation data
Exec="$d/eoview" %F
Icon=eoview
Terminal=false
Categories=Science;Geoscience;Graphics;
DESKTOP
fi
echo "eoview is installed: $d/eoview"
case ":$PATH:" in *":$d:"*) ;; *) echo "Add $d to PATH to start eoview with the command eoview." ;; esac
