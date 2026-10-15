#!/bin/sh
# Remove eoview of install.sh for the user on Linux or macOS.
#   curl -fsSL https://github.com/polymood/eoview/releases/latest/download/uninstall.sh | sh
# EOVIEW_INSTALL_DIR: the directory of the executable (default ~/.local/bin).
# The preferences and the disk cache stay: the script shows the command that removes them.
set -e
d=${EOVIEW_INSTALL_DIR:-$HOME/.local/bin}
rm -f "$d/eoview" "$d/eoview.old" "$d/eoview.new"
if [ "$(uname -s)" = Linux ]; then
    share=${XDG_DATA_HOME:-$HOME/.local/share}
    rm -f "$share/applications/eoview.desktop" "$share/icons/hicolor/256x256/apps/eoview.png"
fi
# The PATH line of install.sh: the line "# eoview" and the line after it. "cat >" keeps the permissions and
# the symbolic links of the start files.
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/eoview.fish"
for rc in "${ZDOTDIR:-$HOME}/.zshrc" "$HOME/.bashrc" "$HOME/.profile"; do
    if grep -qsx '# eoview' "$rc"; then
        t=$(mktemp)
        awk '$0 == "# eoview" { skip = 1; next } skip { skip = 0; next } { print }' "$rc" > "$t"
        cat "$t" > "$rc"
        rm -f "$t"
        echo "The PATH line of eoview is removed from $rc."
    fi
done
echo "eoview is removed."
config=${XDG_CONFIG_HOME:-$HOME/.config}/eoview
if [ "$(uname -s)" = Darwin ]; then cache=$HOME/Library/Caches/eoview; else cache=${XDG_CACHE_HOME:-$HOME/.cache}/eoview; fi
if [ -e "$config" ] || [ -e "$cache" ]; then
    echo "Your preferences and the cache of remote data stay. To remove them too:"
    echo "  rm -rf \"$config\" \"$cache\""
fi
