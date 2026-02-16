# tmax systemd deployment

This guide installs `tmax-local` on Ubuntu using a dedicated service user and `systemd` hardening.

## Automated path (recommended)

From the repository root, run the deploy automation as root on the target host:

```bash
sudo ./scripts/deploy-linux.sh \
  --artifact dist/tmax-x86_64-unknown-linux-gnu.tar.gz \
  --install-root /opt/tmax \
  --service-name tmax-local \
  --socket /run/tmax/tmax.sock
```

Rollback to the previous release (or a named release):

```bash
sudo ./scripts/rollback-linux.sh --install-root /opt/tmax --service-name tmax-local
sudo ./scripts/rollback-linux.sh --install-root /opt/tmax --service-name tmax-local --target tmax-x86_64-unknown-linux-gnu
```

Both scripts run `tmax health --json` and fail if service health checks fail.

## 1. Create service account and directories

```bash
sudo useradd --system --create-home --home /var/lib/tmax --shell /usr/sbin/nologin tmax || true
sudo install -d -o tmax -g tmax -m 0750 /var/lib/tmax
sudo install -d -o root -g root -m 0755 /etc/tmax
sudo install -d -o root -g root -m 0755 /opt/tmax/releases
```

## 2. Install release artifact

```bash
sudo tar -xzf tmax-x86_64-unknown-linux-gnu.tar.gz -C /opt/tmax/releases
sudo ln -sfn /opt/tmax/releases/tmax-x86_64-unknown-linux-gnu /opt/tmax/current
```

## 3. Install config and service unit

```bash
sudo cp /opt/tmax/current/ops/systemd/tmax-local.toml /etc/tmax/tmax-local.toml
sudo cp /opt/tmax/current/ops/systemd/tmax-local.env /etc/tmax/tmax-local.env
sudo cp /opt/tmax/current/ops/systemd/tmax-local.service /etc/systemd/system/tmax-local.service
```

If needed, edit `/etc/tmax/tmax-local.toml` and `/etc/tmax/tmax-local.env` for host-specific settings.

## 4. Enable and start

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now tmax-local
```

## 5. Validate health

```bash
# --socket is required here because the systemd service uses /run/tmax/tmax.sock,
# which differs from the CLI's default auto-discovery path.
/opt/tmax/current/bin/tmax --socket /run/tmax/tmax.sock health --json
```

Healthy output includes:
- `"healthy": true`
- matching protocol versions
- non-error `session_list` round-trip

## 6. Operational checks

```bash
systemctl status tmax-local --no-pager
journalctl -u tmax-local -n 100 --no-pager
```

## 7. Rolling update

```bash
sudo tar -xzf tmax-x86_64-unknown-linux-gnu.tar.gz -C /opt/tmax/releases
sudo ln -sfn /opt/tmax/releases/tmax-x86_64-unknown-linux-gnu /opt/tmax/current
sudo systemctl restart tmax-local
/opt/tmax/current/bin/tmax --socket /run/tmax/tmax.sock health --json
```
