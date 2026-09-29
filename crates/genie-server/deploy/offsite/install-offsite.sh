#!/usr/bin/env bash
#
# Sets up the daily off-site copy of genie.henshaw.us's backups on this
# computer: a systemd user timer that pulls them into ~/Backups/genie. Run
# as yourself, not root. Your account on the VPS must be in the
# genie-backup group, which install.sh there arranges.
set -euo pipefail
cd "$(dirname "$0")"

install -Dm755 genie-backup-pull "$HOME/.local/bin/genie-backup-pull"
install -Dm644 genie-backup-pull.service "$HOME/.config/systemd/user/genie-backup-pull.service"
install -Dm644 genie-backup-pull.timer "$HOME/.config/systemd/user/genie-backup-pull.timer"
systemctl --user daemon-reload
systemctl --user enable --now genie-backup-pull.timer
echo "==> First copy now"
systemctl --user start genie-backup-pull.service
journalctl --user -u genie-backup-pull.service -n 3 --no-pager -o cat
systemctl --user list-timers genie-backup-pull.timer --no-pager | head -2
