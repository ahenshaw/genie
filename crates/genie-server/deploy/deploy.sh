#!/usr/bin/env bash
#
# Builds genie-server and stages it on the VPS, with everything install.sh
# needs, in ~/deploy-genie-server/. sudo there asks for a password, so this
# ends by printing the one line to run in a real terminal.
set -euo pipefail

cd "$(dirname "$0")/../../.."
HOST="${GENIE_HOST:-tennis.henshaw.us}"
STAGE="deploy-genie-server"
BIN=target/release/genie-server

echo "==> Building (cargo build --release -p genie-server)"
cargo build --release -p genie-server

# The binary mustn't need a newer glibc than the server has.
need=$(objdump -T "$BIN" | grep -oE 'GLIBC_[0-9.]+' | sed 's/GLIBC_//' | sort -uV | tail -1)
have=$(ssh "$HOST" "ldd --version | head -1" | grep -oE '[0-9]+\.[0-9]+$')
if [[ "$(printf '%s\n%s\n' "$need" "$have" | sort -V | tail -1)" != "$have" ]]; then
  echo "error: the binary needs glibc $need; $HOST has $have" >&2
  exit 1
fi
echo "==> glibc: needs $need, $HOST has $have"

echo "==> Staging to ${HOST}:~/${STAGE}/"
ssh "$HOST" "mkdir -p ~/${STAGE}"
rsync -az "$BIN" crates/genie-server/deploy/{install.sh,genie-admin,genie-server.service,genie.henshaw.us.conf} "${HOST}:${STAGE}/"

cat <<EOF

==> Staged. Run this in a real terminal to install it:

      ssh -t ${HOST} 'sudo bash ~/${STAGE}/install.sh'

EOF
