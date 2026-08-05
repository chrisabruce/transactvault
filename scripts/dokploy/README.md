# Dokploy provisioning + migration test kit

Two laptop-side scripts for standing up hardened Dokploy servers and
testing a **full restore** (config, apps, volumes) from one instance to
another. Nothing here runs on the server unattended — everything is
driven over SSH from your machine.

## Prerequisites (laptop)

- `ssh` + an SSH keypair (`ssh-keygen -t ed25519` if you have none)
- `sshpass` — `brew install sshpass` (fallback: `brew install esolitos/ipa/sshpass`)
- Two fresh Ubuntu servers (22.04/24.04) with root password SSH

## The restore test, end to end

```sh
# 1. Provision both servers (password prompted once each).
./provision.sh <ip-A>
./provision.sh <ip-B>

# 2. On A: open http://<ip-A>:3000, create the admin account, add a
#    project or two with apps + databases USING NAMED VOLUMES, deploy,
#    write some data you can recognize later.
#    (B just needs its Dokploy install — leave it untouched.)

# 3. Migrate everything A -> B:
./migrate.sh <ip-A> <ip-B>

# 4. Verify on B: log in with A's credentials, fix Settings -> Web
#    Server -> Server IP, redeploy each app, confirm your data is there.
```

## What provision.sh sets up

- apt upgrade + unattended security upgrades, fail2ban (sshd), 2G swap
  if the box has none
- ufw: only your SSH port, 80, 443
- SSH: installs your key, then key-only auth (`--keep-password-auth`
  to skip) — the key is verified working *before* passwords are
  disabled
- Dokploy via the official installer, `ADVERTISE_ADDR` pinned to the
  server IP
- Dokploy UI (`:3000`) restricted to your laptop's public IP in the
  `DOCKER-USER` iptables chain. This is deliberate: **Docker-published
  ports bypass ufw entirely**, so a ufw rule alone would leave the UI
  world-reachable — and whoever visits `:3000` first becomes the
  admin. SSH tunnels (`ssh -N -L 3000:127.0.0.1:3000 root@<ip>`)
  always work, from any network, because tunneled traffic enters via
  loopback which the `DOCKER-USER` chain never sees. If your laptop IP
  changes, either re-run provision.sh (rules are replaced, install is
  skipped) or use the tunnel.

## What migrate.sh moves, and what it can't

Moves: `/etc/dokploy` (Traefik config + certs, build contexts, SSH
keys), the Dokploy Postgres + Redis volumes (all projects, apps,
env, domains, users), and **every named docker volume** — i.e. app
data. Services are scaled to zero on both sides during the copy so
Postgres and volume files are quiescent, then restored.

Can't move: swarm state (node identity/certs are machine-bound) and
therefore the running services themselves — that's why step 4 is
"redeploy each app". The redeploy remounts the already-restored named
volumes, so data survives. Anonymous volumes are skipped on purpose:
redeploys mint fresh anonymous IDs, so restored copies would be
orphans. Version drift between the two installs is detected (swarm
pins image digests) and refused without `--force`.

## Notes

- Host keys for these throwaway servers live in
  `~/.ssh/dokploy_test_known_hosts` — delete that file when you
  rebuild a server at the same IP.
- If TransactVault ends up behind Dokploy's Traefik: Traefik v3
  defaults `respondingTimeouts.readTimeout` to 60s, which cuts long
  proxied uploads (the app's direct-to-storage path is unaffected).
  Raise it in the Traefik config under `/etc/dokploy/traefik` if
  proxy-path uploads matter.
