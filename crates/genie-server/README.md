# genie-server

The API behind genie.henshaw.us. It serves one shared family tree to people who sign
in:

- Editors' changes are merged, and each change is recorded against whoever made it.
- Guests see only people who have died.

The desktop app talks to this server over HTTPS, and the server also serves the browser
version of the app at `/`.
MySQL is reached only by this server, over loopback, and never from outside.

## Roles

| Role | Sees | Can |
|---|---|---|
| admin | everything | edit, manage accounts, import a `.gdz` |
| editor | everything | edit, upload documents |
| family | everything | read only |
| guest | the deceased; living people as "Private" | read only |

## API (JSON, under `/api`)

The desktop app sends `Authorization: Bearer <token>`. The browser app signs in with
`"cookie": true` and gets a session cookie instead. That cookie is HttpOnly, Secure and
SameSite=Strict, and any change made with it must also carry an `X-Genie` header,
which a page on another site can't add.

| Endpoint | Who | What |
|---|---|---|
| `POST /login` `{username, password, cookie?}` | anyone | → `{token, expires_at, user}`. Five wrong passwords lock the username or address for 15 minutes. |
| `POST /logout` | signed in | ends every session of the account |
| `GET /me` | signed in | the account |
| `POST /password` `{current, new}` | signed in | → a new token; other sessions end |
| `GET /tree` | signed in | `{revision, role, gedcom}`: guests get the guest view. `If-None-Match: "rev-N"` → 304 |
| `POST /tree` `{base_revision, gedcom, resolve?}` | editor | Others' changes since `base_revision` are merged in. Returns `{revision, merged, gedcom?, renamed, changes}`, or 409 `{conflicts: [{xref, kind, label, changed_by}]}`. Resubmit with `resolve: {"I12": "mine" \| "theirs"}`. |
| `GET /changes` | signed in | Who changed what, newest first, with each save's note. Filters: `since=N` / `before=N` (revisions), `xref=I12`, `from=` / `to=` (`YYYY-MM-DD`, UTC), `username=`, `limit=` (most 500). Guests see deceased records only. |
| `POST /revert` `{revision, xref?, force?}` | admin | Puts back what that save changed (or just `xref`), as a new revision noted "Reverted …". A 409 lists later changes to the same records (resubmit with `force`), or refuses when links would be left pointing at deleted records. |
| `GET /media` | signed in | `[{path, sha256, size}]` for documents the caller's view links to |
| `GET /media/{sha256}` | signed in | the file, if the caller's view links to it; otherwise 404 |
| `PUT /media?path=…` | editor | stores the body as the document at that path |
| `POST /import[?replace=true]` | admin | the body is a `.gdz`: replaces the tree (a new revision) and adds its documents |
| `GET /users`, `POST /users`, `PATCH /users/{id}` | admin | list, create `{username, display_name, password, role}`, change `{display_name?, role?, disabled?, password?}` |

Changing an account's role, password or disabled flag ends its sessions. The last
active admin can't be demoted or disabled.

## Settings (environment or `.env`)

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | (required) | `mysql://genie:PASSWORD@127.0.0.1:3306/genie` |
| `SESSION_SECRET` | (required) | at least 32 characters; paste the output of `openssl rand -hex 32` |
| `BIND_ADDR` | `127.0.0.1:3100` | |
| `MEDIA_DIR` | `/var/lib/genie/media` | documents, as `<sha256>` files |
| `LIVING_YEARS` | `100` | born this long ago counts as deceased for guests |
| `WEB_DIR` | (unset) | the browser app (a `trunk build` bundle), served at `/` |

The schema is applied at startup.

- `genie-server create-admin <username>` adds the first administrator. It asks for a
  password.
- `genie-server reset-password <username>` sets a new one.

## Developing

```sh
docker run -d --name genie-test-mysql -e MYSQL_ROOT_PASSWORD=genie-test -p 127.0.0.1:33306:3306 mysql:8.4
GENIE_TEST_MYSQL=mysql://root:genie-test@127.0.0.1:33306 cargo test -p genie-server
```

Each test makes and drops its own database. Without `GENIE_TEST_MYSQL`, the database
tests pass without running.

## Deployment (genie.henshaw.us)

It runs on the same VPS as tennis.henshaw.us (Ubuntu 22.04, Apache 2.4, MySQL 8.0).
Apache terminates TLS and proxies everything to `127.0.0.1:3100`.

```
/opt/genie-server/genie-server                  release binary
/opt/genie-server/.env                          DATABASE_URL, SESSION_SECRET, …; mode 600, root
/var/lib/genie/media/                           documents, <sha256> files; www-data
/etc/systemd/system/genie-server.service        runs as www-data, writes only /var/lib/genie
/etc/apache2/sites-available/genie.henshaw.us.conf          :80, redirects to HTTPS
/etc/apache2/sites-available/genie.henshaw.us-le-ssl.conf   :443, written by certbot
/opt/genie-server/web/                          the browser app (trunk bundle)
/usr/local/bin/genie-admin                      the CLI with .env loaded
```

MySQL has a `genie` database. Its `genie` account can connect from localhost only.

**Deploying a new build.** sudo on the VPS asks for a password, so this is two steps:

```sh
crates/genie-server/deploy/deploy.sh          # build the app and server, check glibc, stage to ~/deploy-genie-server/
ssh -t tennis.henshaw.us 'sudo bash ~/deploy-genie-server/install.sh'
```

`install.sh` is safe to rerun.

- **First run:** it creates the database, its account (asking for the MySQL root
  password if root has one), `.env` with fresh secrets, the Apache site and the
  certificate.
- **Every run:** it backs up the binary, restarts the service, and checks that it
  answers. If it doesn't, it restores the previous binary.

**Accounts from the command line:**

```sh
ssh -t tennis.henshaw.us 'sudo genie-admin create-admin <username>'
ssh -t tennis.henshaw.us 'sudo genie-admin reset-password <username>'
```

**Backups.** `install.sh` sets up `/usr/local/sbin/genie-backup`, which runs nightly at
03:17 from `/etc/cron.d/genie-backup`. It writes to `/var/backups/genie/` (root only):

- **The database**, as `genie-YYYY-MM-DD.sql.gz`: every revision, who changed what, and
  the accounts. It's dumped as the `genie` account, with a consistent snapshot, so the
  site keeps running. Dumps are kept for 30 days.
- **The documents**, copied into `media/`. They're named by content and never change, so
  the copy only gains files, and a document removed from the tree can still be recovered.

Each run logs to syslog (`journalctl -t genie-backup`), and fails if the dump comes out
empty. To run one now: `sudo genie-backup`. To restore the database:
`gunzip -c genie-YYYY-MM-DD.sql.gz | mysql -u root -p genie`.

**Off-site copy.** The backups are readable by the `genie-backup` group; `install.sh`
creates it and adds whoever runs it. A computer whose account is in that group can pull
a copy over SSH. `deploy/offsite/install-offsite.sh`, run as yourself, sets that up:

- a systemd user timer copies `/var/backups/genie` into `~/Backups/genie` every day at
  09:00, catching up after the computer was off
- files there are readable by you only, and are never deleted, so every day's dump is kept
- a desktop notification warns when the newest copy is over three days old

`systemctl --user list-timers genie-backup-pull.timer` shows when it runs next, and
`journalctl --user -u genie-backup-pull` shows what it did.

**Signing everyone out:** change `SESSION_SECRET` in `.env`
(`openssl rand -hex 32`), then `systemctl restart genie-server`.
