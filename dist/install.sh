#!/bin/sh
# Install filer for the current user (no root needed, except the optional
# system-wide .portal file at the end).
#
#   ./dist/install.sh [PATH_TO_BINARY] [--default] [--portal-conf]
#   ./dist/install.sh --uninstall
#
# PATH_TO_BINARY  defaults to target/release/filer (relative to the repo root)
# --default       make filer the default app for folders (inode/directory),
#                 which is what `xdg-open some-folder` and many apps use
# --portal-conf   make filer the file chooser of xdg-desktop-portal (the
#                 "Open file" / "Save as" dialogs of Firefox, Discord, ...).
#                 Edits ~/.config/xdg-desktop-portal/<desktop>-portals.conf,
#                 starting from a copy of the system one if you have none.
#
# Installs:
#   ~/.local/bin/filer
#   ~/.local/share/applications/filer.desktop
#   ~/.local/share/dbus-1/services/org.freedesktop.FileManager1.service
#   ~/.local/share/dbus-1/services/org.freedesktop.impl.portal.desktop.filer.service
#   ~/.local/share/xdg-desktop-portal/portals/filer.portal
set -eu

here=$(cd "$(dirname "$0")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}
conf=${XDG_CONFIG_HOME:-$HOME/.config}
bin_dir=$HOME/.local/bin
bin=$bin_dir/filer
desktop=$data/applications/filer.desktop
svc_fm=$data/dbus-1/services/org.freedesktop.FileManager1.service
svc_portal=$data/dbus-1/services/org.freedesktop.impl.portal.desktop.filer.service
portal_file=$data/xdg-desktop-portal/portals/filer.portal
chooser_key=org.freedesktop.impl.portal.FileChooser

# the user's portals.conf: ~/.config/xdg-desktop-portal/<desktop>-portals.conf
desk=$(printf %s "${XDG_CURRENT_DESKTOP:-}" | cut -d: -f1 | tr 'A-Z' 'a-z')
if [ -n "$desk" ]; then conf_name=$desk-portals.conf; else conf_name=portals.conf; fi
conf_file=$conf/xdg-desktop-portal/$conf_name

drop_chooser_line() { # $1 = file; removes any FileChooser= line, keeps the rest
    grep -v "^$chooser_key=" "$1" > "$1.tmp" || true
    mv "$1.tmp" "$1"
}

set_portal_conf() {
    mkdir -p "$conf/xdg-desktop-portal"
    if [ ! -f "$conf_file" ]; then
        # only the first portals.conf found is read, so start from the system's
        # to keep the other interfaces (screenshots, screencast, ...) as they are
        for name in "$conf_name" portals.conf; do
            for d in /etc/xdg-desktop-portal /usr/local/share/xdg-desktop-portal /usr/share/xdg-desktop-portal; do
                if [ -f "$d/$name" ]; then cp "$d/$name" "$conf_file"; break 2; fi
            done
        done
        [ -f "$conf_file" ] || printf '[preferred]\ndefault=*\n' > "$conf_file"
    fi
    drop_chooser_line "$conf_file"
    if grep -q '^\[preferred\]' "$conf_file"; then
        sed "/^\[preferred\]/a $chooser_key=filer" "$conf_file" > "$conf_file.tmp"
        mv "$conf_file.tmp" "$conf_file"
    else
        printf '\n[preferred]\n%s=filer\n' "$chooser_key" >> "$conf_file"
    fi
    echo "  portal config: $conf_file"
}

src=$here/../target/release/filer
want_default=0
want_portal=0
for arg in "$@"; do
    case $arg in
        --default) want_default=1 ;;
        --portal-conf) want_portal=1 ;;
        --uninstall)
            rm -f "$bin" "$desktop" "$svc_fm" "$svc_portal" "$portal_file"
            [ -f "$conf_file" ] && drop_chooser_line "$conf_file"
            command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
            echo "removed filer."
            echo "If you used --default:  xdg-mime default <other-app>.desktop inode/directory"
            echo "If you copied the .portal file to /usr/share: sudo rm /usr/share/xdg-desktop-portal/portals/filer.portal"
            echo "Then: systemctl --user restart xdg-desktop-portal"
            exit 0 ;;
        -*) echo "unknown option: $arg" >&2; exit 2 ;;
        *) src=$arg ;;
    esac
done

[ -x "$src" ] || { echo "binary not found: $src (build with: cargo build --release)" >&2; exit 1; }

mkdir -p "$bin_dir" "$data/applications" "$data/dbus-1/services" "$data/xdg-desktop-portal/portals"
install -m 755 "$src" "$bin"
# D-Bus service files need an absolute Exec path
sed "s|^Exec=.*|Exec=$bin %F|" "$here/filer.desktop" > "$desktop"
sed "s|^Exec=.*|Exec=$bin --dbus-activated|" "$here/org.freedesktop.FileManager1.service" > "$svc_fm"
sed "s|^Exec=.*|Exec=$bin --portal-backend|" "$here/org.freedesktop.impl.portal.desktop.filer.service" > "$svc_portal"
cp "$here/filer.portal" "$portal_file"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true

echo "installed:"
echo "  $bin"
echo "  $desktop"
echo "  $svc_fm"
echo "  $svc_portal"
echo "  $portal_file"

[ "$want_default" = 1 ] && { xdg-mime default filer.desktop inode/directory; echo "  default folder handler: filer"; }
[ "$want_portal" = 1 ] && set_portal_conf

echo
echo "Test 'show in folder':"
echo "  gdbus call --session --dest org.freedesktop.FileManager1 --object-path /org/freedesktop/FileManager1 \\"
echo "    --method org.freedesktop.FileManager1.ShowFolders \"['file://$HOME']\" \"\""
if [ "$want_portal" = 1 ]; then
    echo
    echo "File chooser: xdg-desktop-portal may only read .portal files from /usr/share, so also run:"
    echo "  sudo install -Dm644 $here/filer.portal /usr/share/xdg-desktop-portal/portals/filer.portal"
    echo "then restart the portal:  systemctl --user restart xdg-desktop-portal"
fi
echo
echo "If the session bus does not see the new service files, log out and back in."
