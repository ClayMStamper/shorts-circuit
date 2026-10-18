# Cloud runner

The cloud runner posts approved publications while your computer is off. It is optional: the
desktop app works exactly as before until you set one up and connect it in Settings.

How it fits together:

```
desktop app ──HTTPS──▶ Caddy (TLS, your domain) ──▶ shorts-runner ──▶ PostgreSQL
                                                         │
                                                         └─▶ media volume (uploaded renders)
```

- You approve on the desktop as usual, then choose **Post from the cloud** on an approved
  publication. The desktop hands the runner that exact revision, the rendered video and the
  account's credential. From then on only the runner can publish it; the desktop shows its
  progress and the published link.
- **Take back** returns it to the desktop. The runner refuses while it is uploading or
  publishing, or while it will resume an upload the platform already has; try again a little
  later.
- Anything that needs you (an unknown outcome after a lost publish response, a rejected
  video, an overdue post) is handled on the desktop after taking it back. A sign-in that runs
  out is fixed by reconnecting the account on the desktop; the runner gets the new sign-in on
  the next sync and continues by itself.

Design details are in `docs/architecture.md` ("Cloud runner").

## What you need

- A small Linux server. A DigitalOcean droplet works well: Ubuntu 24.04, the Basic plan with
  1 vCPU and 1–2 GB RAM, and a disk with room for a few days of renders (25 GB is plenty for
  short videos). Any VPS with Docker works the same way.
- A domain or subdomain you control, e.g. `runner.example.com`, for the HTTPS certificate.
- Docker with the Compose plugin on the server.

## Set it up (DigitalOcean example)

1. **Create the droplet.** Ubuntu 24.04 LTS, Basic, the smallest size that fits your
   budget. Add your SSH key. Optionally turn on DigitalOcean's weekly backups.
2. **Point the domain at it.** Add an `A` record (and `AAAA` if you use IPv6) for
   `runner.example.com` with the droplet's address. Wait until `dig runner.example.com`
   shows it.
3. **Firewall.** Allow only SSH, HTTP (for the certificate challenge) and HTTPS:
   ```sh
   sudo ufw allow OpenSSH && sudo ufw allow 80/tcp && sudo ufw allow 443/tcp && sudo ufw enable
   ```
   (Or use a DigitalOcean cloud firewall with the same rules.)
4. **Install Docker.** Follow Docker's instructions for Ubuntu
   (docs.docker.com/engine/install/ubuntu), then `sudo usermod -aG docker $USER` and log in
   again.
5. **Build the image** on the server (or build it elsewhere and push it to a registry you
   control, then set `RUNNER_IMAGE` in `.env`):
   ```sh
   git clone https://github.com/ClayMStamper/shorts-circuit.git
   cd shorts-circuit
   docker build -f crates/runner/Dockerfile -t shorts-runner:latest .
   ```
6. **Configure.**
   ```sh
   sudo mkdir -p /opt/shorts-runner && sudo chown $USER /opt/shorts-runner
   cp crates/runner/deploy/compose.yaml crates/runner/deploy/Caddyfile /opt/shorts-runner/
   cp crates/runner/deploy/env.example /opt/shorts-runner/.env
   chmod 600 /opt/shorts-runner/.env
   docker run --rm shorts-runner:latest generate-key   # → SHORTS_RUNNER_MASTER_KEY
   openssl rand -base64 32                              # → POSTGRES_PASSWORD
   ```
   Edit `/opt/shorts-runner/.env`: `RUNNER_DOMAIN`, `POSTGRES_PASSWORD` and
   `SHORTS_RUNNER_MASTER_KEY`. **Also save the master key in your password manager.** It
   isn't stored anywhere else; without it the stored credentials can't be decrypted.
7. **Start it.**
   ```sh
   cd /opt/shorts-runner && docker compose up -d
   curl https://runner.example.com/healthz     # → ok
   ```
   Caddy gets the certificate on first start; give it a minute.
8. **Create a token for your computer.**
   ```sh
   docker compose exec runner shorts-runner token create "Clay's PC"
   ```
   It prints the token once. Each installation of the desktop app gets its own token.
9. **Connect the desktop.** Settings → Cloud runner → Runner address
   `https://runner.example.com` and the token → **Connect**. The token goes to Windows
   Credential Manager (`cloud_runner_token`); the app only accepts `https://` addresses.

To try it the first time: approve a publication for a test account a few minutes ahead,
choose **Post from the cloud**, quit the desktop app, and check the post appears on the
platform at its time. Open the app again: the Monitor shows it as published with its link.

## Day to day

- `docker compose logs -f runner` shows what the runner does. Logs never include tokens,
  credentials or request bodies.
- `docker compose exec runner shorts-runner token list` lists installations and when each
  was last seen; `token revoke <id>` locks one out (for example a lost laptop).
- Media is deleted from the server once its publication is published or taken back (renders
  are re-uploaded if you send them again).
- **Upgrades:** `git pull`, rebuild the image, then `docker compose up -d`. The runner applies
  database migrations on start; on stop it releases running jobs with their checkpoints and
  resumes them after the restart.

## Backups

The database holds the hand-overs, the publication history and the encrypted credentials.
Media doesn't need backing up.

1. Nightly dump, for example with `crontab -e`:
   ```cron
   15 3 * * * cd /opt/shorts-runner && docker compose exec -T postgres pg_dump -U shorts -Fc shorts_runner > /opt/shorts-runner/backups/runner-$(date +\%F).dump && find /opt/shorts-runner/backups -name 'runner-*.dump' -mtime +14 -delete
   ```
   (`mkdir /opt/shorts-runner/backups` first.)
2. Copy the dumps off the server, e.g. to DigitalOcean Spaces with `s3cmd`/`rclone`, or pull
   them to another machine with `rsync`.
3. Keep the master key separately from the dumps. A dump without the key exposes no
   credentials; a dump with the key does.

**Restoring.** Stop the runner, restore, and make the first start cautious, because an old
backup may not know about a post the runner made after the backup; the desktop would hand
that publication over again.

```sh
docker compose stop runner
docker compose exec -T postgres pg_restore -U shorts -d shorts_runner --clean --if-exists < runner-YYYY-MM-DD.dump
# In .env: SHORTS_RUNNER_OVERDUE=0, so anything already past its time waits for you.
docker compose up -d runner
```

Check the platforms for anything that should have gone out since the backup, take those
publications back on the desktop and record what happened, then set `SHORTS_RUNNER_OVERDUE`
back.

## Rotating the encryption key

Credentials are envelope-encrypted: each one has its own data key, and only the data keys are
encrypted with the master key, so rotation only re-wraps them.

1. `docker run --rm shorts-runner:latest generate-key` → the new key.
2. In `.env`: move the current key to `SHORTS_RUNNER_OLD_MASTER_KEYS=<old key>` and put the new
   one in `SHORTS_RUNNER_MASTER_KEY`. `docker compose up -d runner`.
3. `docker compose exec runner shorts-runner rotate-keys`. It reports how many credentials it
   re-wrapped and which keys are still in use; only the current one should be listed.
4. Remove `SHORTS_RUNNER_OLD_MASTER_KEYS` from `.env` and `docker compose up -d runner`.
5. Keep the old key (in your password manager) until every backup made before the rotation
   has expired, since those backups need it.

If you think the master key leaked, rotate it and also reconnect each account on the desktop
(new tokens), then revoke the old grants with the platforms.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `DATABASE_URL` | required | PostgreSQL connection string. |
| `SHORTS_RUNNER_MASTER_KEY` | required | Base64 of 32 random bytes (`shorts-runner generate-key`). |
| `SHORTS_RUNNER_OLD_MASTER_KEYS` | empty | Retired keys, comma-separated, only during a rotation. |
| `SHORTS_RUNNER_BIND` | `127.0.0.1:8787` (`0.0.0.0:8787` in the image) | Plain HTTP; always behind the TLS proxy. |
| `SHORTS_RUNNER_DATA_DIR` | `/var/lib/shorts-runner` | Uploaded media. |
| `SHORTS_RUNNER_OVERDUE` | `15` | Minutes a post may be late (runner was down) before it waits for you; `catch-up` posts however late. |
| `SHORTS_RUNNER_MAX_UPLOAD_BYTES` | 4 GiB | Largest video accepted. |
| `SHORTS_YOUTUBE_UPLOADS_PER_DAY`, `SHORTS_YOUTUBE_UNITS_PER_DAY`, `SHORTS_INSTAGRAM_API_VERSION`, `SHORTS_LINKEDIN_API_VERSION` | as on the desktop | Same meaning as for the desktop app. |
| `RUST_LOG` | `info,sqlx=warn` | Log level. |

## Things to know

- **YouTube quota** is per Google Cloud project, shared by the desktop and the runner, but
  each counts only its own spend. If one runs out, YouTube's `quotaExceeded` answer makes the
  other wait until midnight Pacific as well.
- **Rotating refresh tokens.** Snap (and sometimes Google) issue a new refresh token on each
  refresh and retire the old one. The runner stores the new one and the desktop fetches it on
  its next sync. If both refresh in the same moment, one may be refused and ask you to
  reconnect.
- **Instagram tokens** are renewed by the runner too while it has publications for the
  account; the desktop picks up the renewed token on its next sync.
- One runner process per database. Two would not double-publish (claims are locked), but
  there is no benefit.
