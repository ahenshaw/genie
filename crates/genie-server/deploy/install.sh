#!/usr/bin/env bash
#
# Installs or updates genie-server on genie.henshaw.us. Staged by deploy.sh;
# run with sudo in a real terminal:
#
#   ssh -t tennis.henshaw.us 'sudo bash ~/deploy-genie-server/install.sh'
#
# Safe to run again. The first run also creates the MySQL database and its
# account (localhost only), the settings file with fresh secrets, the Apache
# site and its certificate. Each run backs up the running binary and puts it
# back if the new one doesn't come up.
set -euo pipefail

STAGE="$(cd "$(dirname "$0")" && pwd)"
OPT=/opt/genie-server
ENV_FILE=$OPT/.env
WEB_DIR=$OPT/web
DATA=/var/lib/genie
WEB=/var/www/genie
SITE=genie.henshaw.us
PORT=3100

if [[ $EUID -ne 0 ]]; then
  echo "error: run with sudo" >&2
  exit 1
fi
if [[ ! -x "$STAGE/genie-server" ]]; then
  echo "error: $STAGE/genie-server not found -- run deploy.sh first" >&2
  exit 1
fi

# MySQL administration: through root's socket where that works, otherwise
# with a password, asked for once. MYSQL_PWD keeps it off the command line.
MYSQL_ADMIN="${MYSQL_ADMIN:-root}"
mysql_admin() { mysql -u "$MYSQL_ADMIN" "$@"; }
if ! mysql_admin -e "SELECT 1" >/dev/null 2>&1; then
  read -rsp "MySQL password for $MYSQL_ADMIN: " MYSQL_PWD
  echo
  export MYSQL_PWD
  if ! mysql_admin -e "SELECT 1" >/dev/null; then
    echo "error: couldn't sign in to MySQL as $MYSQL_ADMIN (set MYSQL_ADMIN for another account)" >&2
    exit 1
  fi
fi

echo "==> Directories"
install -d -m 755 "$OPT" "$WEB"
install -d -m 750 -o www-data -g www-data "$DATA" "$DATA/media"

if [[ ! -f "$ENV_FILE" ]]; then
  echo "==> First install: the genie database and its MySQL account"
  DB_PASS=$(openssl rand -hex 24)
  SECRET=$(openssl rand -hex 32)
  # The genie account is for localhost only, so it's useless from anywhere
  # else even though MySQL itself listens on every interface.
  mysql_admin <<SQL
CREATE DATABASE IF NOT EXISTS genie CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE USER IF NOT EXISTS 'genie'@'localhost' IDENTIFIED BY '$DB_PASS';
CREATE USER IF NOT EXISTS 'genie'@'127.0.0.1' IDENTIFIED BY '$DB_PASS';
ALTER USER 'genie'@'localhost' IDENTIFIED BY '$DB_PASS';
ALTER USER 'genie'@'127.0.0.1' IDENTIFIED BY '$DB_PASS';
GRANT ALL PRIVILEGES ON genie.* TO 'genie'@'localhost';
GRANT ALL PRIVILEGES ON genie.* TO 'genie'@'127.0.0.1';
FLUSH PRIVILEGES;
SQL
  (
    umask 077
    cat > "$ENV_FILE" <<ENV
# genie-server settings. Root only: it holds the database password and the
# secret that signs sign-ins (changing SESSION_SECRET signs everyone out).
DATABASE_URL=mysql://genie:$DB_PASS@127.0.0.1:3306/genie
SESSION_SECRET=$SECRET
BIND_ADDR=127.0.0.1:$PORT
MEDIA_DIR=$DATA/media
LIVING_YEARS=100
WEB_DIR=$WEB_DIR
ENV
  )
  chown root:root "$ENV_FILE"
  chmod 600 "$ENV_FILE"
  echo "    wrote $ENV_FILE"
fi

# Installs from before the browser app lacked this.
if ! grep -q '^WEB_DIR=' "$ENV_FILE"; then
  echo "WEB_DIR=$WEB_DIR" >> "$ENV_FILE"
  echo "    added WEB_DIR to $ENV_FILE"
fi

if [[ -f "$STAGE/web/index.html" ]]; then
  echo "==> Installing the browser app in $WEB_DIR"
  install -d -m 755 "$WEB_DIR"
  rsync -a --delete "$STAGE/web/" "$WEB_DIR/"
  chown -R root:root "$WEB_DIR"
  find "$WEB_DIR" -type d -exec chmod 755 {} +
  find "$WEB_DIR" -type f -exec chmod 644 {} +
fi

echo "==> Installing the binary"
if [[ -x "$OPT/genie-server" ]]; then
  cp -a "$OPT/genie-server" "$OPT/genie-server.previous"
fi
install -m 755 "$STAGE/genie-server" "$OPT/genie-server.new"
mv "$OPT/genie-server.new" "$OPT/genie-server"
install -m 755 "$STAGE/genie-admin" /usr/local/bin/genie-admin
install -m 644 "$STAGE/genie-server.service" /etc/systemd/system/genie-server.service
systemctl daemon-reload
systemctl enable --quiet genie-server
systemctl restart genie-server

# Up, and refusing a request that isn't signed in: proves it's serving and
# its database works, without changing anything.
ok=false
for _ in $(seq 1 30); do
  code=$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT/api/me" || true)
  if [[ "$code" == 401 ]]; then ok=true; break; fi
  sleep 0.5
done
if ! $ok; then
  echo "error: genie-server didn't come up. Its log:" >&2
  journalctl -u genie-server -n 30 --no-pager >&2 || true
  if [[ -x "$OPT/genie-server.previous" ]]; then
    echo "==> Putting the previous binary back" >&2
    mv "$OPT/genie-server.previous" "$OPT/genie-server"
    systemctl restart genie-server
  fi
  exit 1
fi
echo "==> genie-server is up on 127.0.0.1:$PORT"

if [[ ! -f /etc/apache2/sites-available/$SITE.conf ]]; then
  echo "==> Apache site $SITE"
  install -m 644 "$STAGE/$SITE.conf" "/etc/apache2/sites-available/$SITE.conf"
  a2ensite -q "$SITE"
  apache2ctl configtest
  systemctl reload apache2
fi

if [[ ! -d /etc/letsencrypt/live/$SITE ]]; then
  echo "==> HTTPS certificate for $SITE"
  if ! certbot --apache -d "$SITE" --redirect --non-interactive --agree-tos; then
    echo "certbot failed; run it by hand:  sudo certbot --apache -d $SITE --redirect" >&2
    exit 1
  fi
fi

code=$(curl -s -o /dev/null -w '%{http_code}' "https://$SITE/api/me" || true)
echo "==> https://$SITE/api/me answers $code (401 is right: not signed in)"
code=$(curl -s -o /dev/null -w '%{http_code}' "https://$SITE/" || true)
echo "==> https://$SITE/ answers $code (200: the browser app)"

echo "==> Nightly backups"
install -m 755 "$STAGE/genie-backup" /usr/local/sbin/genie-backup
install -m 644 "$STAGE/genie-backup.cron" /etc/cron.d/genie-backup
if ! ls /var/backups/genie/genie-*.sql.gz >/dev/null 2>&1; then
  echo "    first backup now:"
  /usr/local/sbin/genie-backup | sed 's/^/    /'
else
  echo "    last: $(ls -t /var/backups/genie/genie-*.sql.gz | head -1)"
fi

users=$(mysql_admin -N genie -e "SELECT COUNT(*) FROM users" 2>/dev/null || echo "?")
if [[ "$users" == 0 ]]; then
  cat <<EOF

    No accounts yet. Create the first administrator (it asks for a password):

      sudo genie-admin create-admin <username>

EOF
fi
echo "==> Done."
