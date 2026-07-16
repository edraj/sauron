# Sauron — Setup & Operations (RPM install)

Assumes the RPMs from [INSTALL.md](INSTALL.md) are installed. Sauron needs an
external **PostgreSQL** and **Redis/Valkey**; they can be on this host or remote.

## 1. Provision PostgreSQL

Local example (Fedora):

```bash
sudo dnf install -y postgresql-server
sudo postgresql-setup --initdb
sudo systemctl enable --now postgresql
sudo -u postgres psql <<'SQL'
CREATE ROLE sauron LOGIN PASSWORD 'change-me-strong';
CREATE DATABASE sauron OWNER sauron;
SQL
```

Remote Postgres: just note its host, port, database, user, password.

## 2. Provision Redis / Valkey

```bash
sudo dnf install -y valkey        # Fedora 41+ (use 'redis' on RHEL)
sudo systemctl enable --now valkey
```

## 3. Configure the shared connection

Edit `/etc/sauron/sauron.env`:

```sh
DATABASE_URL=postgres://sauron:change-me-strong@localhost:5432/sauron
REDIS_URL=redis://127.0.0.1:6379
RUST_LOG=info,sauron=info
```

Per-service tunables live in `/etc/sauron/{api,ingest,monitor,tier}.env` — the
defaults are fine to start.

## 4. JWT secret

`/etc/sauron/secret.env` is generated with a random `JWT_SECRET` on first install.
Verify it exists:

```bash
sudo test -s /etc/sauron/secret.env && echo "JWT secret present"
```

To rotate it (invalidates existing sessions):

```bash
sudo sh -c 'umask 077; printf "JWT_SECRET=%s\n" "$(head -c 32 /dev/urandom | od -An -tx1 | tr -d " \n")" > /etc/sauron/secret.env'
sudo chgrp sauron /etc/sauron/secret.env && sudo chmod 0640 /etc/sauron/secret.env
sudo systemctl restart sauron-api
```

## 5. Run database migrations

```bash
sudo systemctl start sauron-migrate
journalctl -u sauron-migrate --no-pager | tail
```

Expected: `migrations up to date`. Re-runnable safely (idempotent).

## 6. Enable and start the services

```bash
sudo systemctl enable --now sauron-api sauron-ingest sauron-monitor sauron-tier
systemctl --no-pager status 'sauron-*'
```

- `sauron-api` → `:8080` (dashboard API)
- `sauron-ingest` → `:8081` (SDK ingest)
- `sauron-monitor`, `sauron-tier` → no listener

## 7. Dashboard

1. Set the **browser-facing** URLs in `/etc/sauron/dashboard.env` (public/reverse-proxied addresses):

   ```sh
   API_BASE_URL=https://sauron.example.com/api
   INGEST_BASE_URL=https://sauron.example.com/ingest
   ```

2. Regenerate `config.js` and (re)load nginx:

   ```bash
   sudo /usr/libexec/sauron/sauron-dashboard-config
   sudo systemctl enable --now nginx
   sudo systemctl reload nginx
   ```

   Fedora's stock nginx ships a default `server { listen 80 default_server; }`.
   Either remove it from `/etc/nginx/nginx.conf` or add `default_server` +
   `server_name` to `/etc/nginx/conf.d/sauron-dashboard.conf` so the dashboard is
   served on `:80`. For TLS, terminate at nginx or a fronting proxy.

## 8. Firewall

```bash
sudo firewall-cmd --add-service=http --permanent          # dashboard :80
sudo firewall-cmd --add-port=8080/tcp --permanent         # API (if reached directly)
sudo firewall-cmd --add-port=8081/tcp --permanent         # ingest (SDKs)
sudo firewall-cmd --reload
```

Prefer fronting `:8080`/`:8081` with nginx/TLS rather than exposing them directly.

## 9. Verify

```bash
curl -fsS http://localhost:8080/health && echo               # API up
curl -fsS http://localhost:8081/health && echo               # ingest up
curl -fsS http://localhost/config.js                          # dashboard runtime config
journalctl -u sauron-api -u sauron-ingest --no-pager | tail
```

(If a service exposes a different health path, check `journalctl` for the bound
address logged at startup.)

## 10. Troubleshooting

| Symptom | Check |
|---|---|
| Service fails immediately | `journalctl -u sauron-<svc> -e` — usually `DATABASE_URL` wrong/unreachable |
| `DATABASE_URL is required` | `/etc/sauron/sauron.env` not set or unreadable by the `sauron` user |
| API 401 / login broken | `secret.env` missing or changed since sessions issued — rotate & restart |
| Dashboard shows wrong API URL | edit `/etc/sauron/dashboard.env`, re-run `sauron-dashboard-config`, reload nginx, hard-refresh |
| Ingest 429 | raise `INGEST_RATE_LIMIT_PER_MIN` in `/etc/sauron/ingest.env`, restart |
| Tier can't write cold | confirm `/var/lib/sauron/cold` is owned by `sauron` (see `tmpfiles`) |
