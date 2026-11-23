#!/usr/bin/env bash
# Ozi Browser'ı kullanıcı dizinine kurar (sudo gerekmez): ~/.local/bin + uygulama menüsü.
set -euo pipefail
cd "$(dirname "$0")"
[ -x target/release/ozi-browser ] || cargo build --release -p ozi-browser
install -Dm755 target/release/ozi-browser "$HOME/.local/bin/ozi-browser"
install -Dm644 packaging/ozi-browser.desktop "$HOME/.local/share/applications/ozi-browser.desktop"
install -Dm644 packaging/ozi-browser.svg "$HOME/.local/share/icons/hicolor/scalable/apps/ozi-browser.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications" || true
echo "Kuruldu: ozi-browser (uygulama menüsünde 'Ozi Browser')"
