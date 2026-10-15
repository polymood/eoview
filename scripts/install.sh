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
# The executable and its signature of one version, not of two "latest" that a new release can separate.
v=$(curl -fsSL "$url/version.txt")
case "$v" in *[!0-9.]* | "") echo "eoview: no valid version in $url/version.txt" >&2; exit 1 ;; esac
rel=https://github.com/polymood/eoview/releases/download/v$v
d=${EOVIEW_INSTALL_DIR:-$HOME/.local/bin}
mkdir -p "$d"
curl -fSL "$rel/$a" -o "$d/eoview.new"
# The signature of the release (ed25519, the key of crates/eoview/src/update.rs). OpenSSL 3 checks it. Without
# OpenSSL 3 (for example the LibreSSL of macOS), only HTTPS protects the download.
key=$(mktemp)
cat > "$key" <<'KEY'
-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8r+3vit0GYl7rQyR/86Ptl9uazFc9di66jCroG0AKvc=
-----END PUBLIC KEY-----
KEY
if command -v openssl >/dev/null && openssl pkey -pubin -in "$key" -noout 2>/dev/null && openssl pkeyutl -help 2>&1 | grep -q -- -rawin; then
    curl -fsSL "$rel/$a.sig" -o "$d/eoview.sig"
    if ! openssl pkeyutl -verify -pubin -inkey "$key" -rawin -in "$d/eoview.new" -sigfile "$d/eoview.sig" >/dev/null; then
        rm -f "$d/eoview.new" "$d/eoview.sig" "$key"
        echo "eoview: the signature of $a $v is not valid. Nothing is installed." >&2
        exit 1
    fi
    rm -f "$d/eoview.sig"
    echo "The signature of eoview $v is valid."
else
    echo "eoview: OpenSSL 3 is not available, the signature is not checked."
fi
rm -f "$key"
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
# The logo (assets/eoview.svg) in half blocks and the name in the ANSI Shadow font of figlet, 256 colors.
if [ -t 1 ] && [ -z "$NO_COLOR" ]; then
    printf '\n'
    printf '           \033[38;5;24m▄▄▄▄         \033[0m   \n'
    printf '       \033[38;5;24m▄\033[48;5;31m▀▀▀▀\033[49m████\033[48;5;255m▀▀\033[49m▄     \033[0m   \033[38;5;117m███████\033[38;5;240m╗ \033[38;5;117m██████\033[38;5;240m╗ \033[38;5;117m██\033[38;5;240m╗   \033[38;5;117m██\033[38;5;240m╗\033[38;5;117m██\033[38;5;240m╗\033[38;5;117m███████\033[38;5;240m╗\033[38;5;117m██\033[38;5;240m╗    \033[38;5;117m██\033[38;5;240m╗\033[0m\n'
    printf '     \033[38;5;24m▄█\033[38;5;31m\033[48;5;24m▀\033[38;5;24m\033[49m████\033[38;5;31m\033[48;5;24m▀▀▀\033[49m█\033[38;5;24m\033[48;5;255m▀\033[38;5;255m\033[49m██\033[38;5;24m\033[48;5;255m▀\033[49m▄   \033[0m   \033[38;5;75m██\033[38;5;240m╔════╝\033[38;5;75m██\033[38;5;240m╔═══\033[38;5;75m██\033[38;5;240m╗\033[38;5;75m██\033[38;5;240m║   \033[38;5;75m██\033[38;5;240m║\033[38;5;75m██\033[38;5;240m║\033[38;5;75m██\033[38;5;240m╔════╝\033[38;5;75m██\033[38;5;240m║    \033[38;5;75m██\033[38;5;240m║\033[0m\n'
    printf '    \033[38;5;24m█\033[48;5;31m▀\033[38;5;31m\033[48;5;24m▀\033[38;5;24m\033[49m███████\033[38;5;31m█\033[38;5;24m█\033[38;5;255m███\033[48;5;24m▀\033[38;5;24m\033[48;5;31m▀\033[49m█  \033[0m   \033[38;5;74m█████\033[38;5;240m╗  \033[38;5;74m██\033[38;5;240m║   \033[38;5;74m██\033[38;5;240m║\033[38;5;74m██\033[38;5;240m║   \033[38;5;74m██\033[38;5;240m║\033[38;5;74m██\033[38;5;240m║\033[38;5;74m█████\033[38;5;240m╗  \033[38;5;74m██\033[38;5;240m║ \033[38;5;74m█\033[38;5;240m╗ \033[38;5;74m██\033[38;5;240m║\033[0m\n'
    printf '   \033[38;5;24m▄█\033[38;5;31m█\033[48;5;24m▀▀\033[38;5;24m\033[48;5;31m▀▀\033[49m███\033[48;5;31m▀\033[49m██\033[38;5;255m███\033[38;5;24m██\033[38;5;31m█\033[38;5;24m▄ \033[0m   \033[38;5;32m██\033[38;5;240m╔══╝  \033[38;5;32m██\033[38;5;240m║   \033[38;5;32m██\033[38;5;240m║╚\033[38;5;32m██\033[38;5;240m╗ \033[38;5;32m██\033[38;5;240m╔╝\033[38;5;32m██\033[38;5;240m║\033[38;5;32m██\033[38;5;240m╔══╝  \033[38;5;32m██\033[38;5;240m║\033[38;5;32m███\033[38;5;240m╗\033[38;5;32m██\033[38;5;240m║\033[0m\n'
    printf '   \033[38;5;24m█\033[38;5;31m█\033[38;5;24m██████\033[38;5;31m\033[48;5;24m▀\033[49m██\033[38;5;24m\033[48;5;31m▀\033[38;5;255m\033[49m███\033[38;5;24m███\033[38;5;31m█\033[38;5;24m█ \033[0m   \033[38;5;31m███████\033[38;5;240m╗╚\033[38;5;31m██████\033[38;5;240m╔╝ ╚\033[38;5;31m████\033[38;5;240m╔╝ \033[38;5;31m██\033[38;5;240m║\033[38;5;31m███████\033[38;5;240m╗╚\033[38;5;31m███\033[38;5;240m╔\033[38;5;31m███\033[38;5;240m╔╝\033[0m\n'
    printf '   \033[38;5;24m▀\033[38;5;31m█\033[38;5;24m███████\033[38;5;31m\033[48;5;24m▀\033[38;5;24m\033[49m█\033[38;5;255m███\033[48;5;24m▀\033[38;5;24m\033[48;5;31m▀▀\033[38;5;31m\033[49m█\033[38;5;24m█▀ \033[0m   \033[38;5;240m╚══════╝ ╚═════╝   ╚═══╝  ╚═╝╚══════╝ ╚══╝╚══╝\033[0m\n'
    printf '    \033[38;5;24m█\033[38;5;31m\033[48;5;24m▀\033[38;5;24m\033[48;5;31m▀▀▀\033[49m██\033[38;5;31m█\033[38;5;24m█\033[38;5;255m███\033[48;5;24m▀\033[38;5;24m\033[49m██\033[48;5;31m▀\033[38;5;31m\033[48;5;24m▀\033[38;5;24m\033[49m█  \033[0m   \n'
    printf '     \033[38;5;24m▀███\033[38;5;31m\033[48;5;24m▀\033[49m█\033[38;5;24m\033[48;5;31m▀\033[38;5;255m\033[49m███\033[48;5;24m▀\033[38;5;24m\033[49m██\033[48;5;31m▀\033[49m█▀   \033[0m   \033[2mFast viewer for Earth observation data\033[0m\n'
    printf '       \033[38;5;24m▀██\033[48;5;255m▀\033[38;5;255m\033[49m███\033[48;5;24m▀\033[38;5;31m▀▀▀\033[38;5;24m\033[49m▀     \033[0m   \033[1meoview %s\033[0m is installed.\n' "$v"
    printf '           \033[38;5;24m▀▀▀▀         \033[0m   \n'
    printf '\n'
fi
echo "eoview is installed: $d/eoview"
# The directory goes in PATH through the start file of the shell of the user. EOVIEW_NO_MODIFY_PATH=1: no change.
# The PATH of this shell is not the PATH of a new terminal (for example, a directory that an export of this
# terminal added), thus the line is always in the start file. The line itself adds the directory only once.
sh_line="case \":\$PATH:\" in *\":$d:\"*) ;; *) export PATH=\"$d:\$PATH\" ;; esac"
case "${SHELL##*/}" in
    zsh) rc=${ZDOTDIR:-$HOME}/.zshrc line=$sh_line ;;
    bash) rc=$HOME/.bashrc line=$sh_line ;;
    fish) rc=${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/eoview.fish line="fish_add_path \"$d\"" ;;
    *) rc=$HOME/.profile line=$sh_line ;;
esac
if [ -n "$EOVIEW_NO_MODIFY_PATH" ]; then
    echo "Add $d to PATH to start eoview with the command eoview."
    exit 0
fi
if ! grep -qsF "$line" "$rc"; then
    mkdir -p "$(dirname "$rc")"
    printf '\n# eoview\n%s\n' "$line" >> "$rc"
    echo "$d is added to PATH in $rc."
fi
case ":$PATH:" in
    *":$d:"*) ;;
    *) echo "Open a new terminal, or type this command, to start eoview with the command eoview:"
       if [ "${SHELL##*/}" = fish ]; then echo "  $line"; else echo "  export PATH=\"$d:\$PATH\""; fi ;;
esac
