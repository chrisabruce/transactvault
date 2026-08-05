#!/usr/bin/env bash
#
# provision.sh — install Dokploy correctly and securely on a FRESH
# Ubuntu server, driven entirely from your laptop over SSH.
#
#   ./provision.sh [options] <server-ip>
#
# You'll be prompted once for the root password. The script:
#   1. installs your SSH public key (the only step that uses the password),
#   2. hardens the box: apt upgrade, unattended-upgrades, fail2ban,
#      ufw (22/80/443), 2G swap if none, key-only SSH,
#   3. runs the official Dokploy installer,
#   4. locks the Dokploy UI (:3000) to YOUR public IP via the
#      DOCKER-USER iptables chain — ufw cannot do this, because Docker
#      publishes ports around ufw entirely. SSH tunnels still work
#      (they enter via loopback, which DOCKER-USER never sees).
#
# Options:
#   -p <port>              SSH port (default 22)
#   --ui-allow <cidr>      allow this CIDR to reach :3000 instead of
#                          auto-detecting your laptop's public IP
#   --ui-open              leave :3000 open to the world (NOT advised;
#                          first visitor becomes the admin)
#   --keep-password-auth   skip the key-only SSH lockdown
#
# Local requirements: ssh, sshpass (brew install sshpass — if your
# Homebrew doesn't have it: brew install esolitos/ipa/sshpass), and an
# SSH keypair (ssh-keygen -t ed25519).
#
# Run it once per server; then use migrate.sh to test a full restore
# from one Dokploy to another.

set -euo pipefail

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

SSH_PORT=22
UI_ALLOW=""
UI_OPEN=0
KEEP_PW=0
SERVER_IP=""

while [ $# -gt 0 ]; do
    case "$1" in
        -p) SSH_PORT="$2"; shift 2 ;;
        --ui-allow) UI_ALLOW="$2"; shift 2 ;;
        --ui-open) UI_OPEN=1; shift ;;
        --keep-password-auth) KEEP_PW=1; shift ;;
        -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
        -*) die "unknown option: $1" ;;
        *) SERVER_IP="$1"; shift ;;
    esac
done
[ -n "$SERVER_IP" ] || die "usage: $0 [options] <server-ip>   (see --help)"

command -v ssh >/dev/null || die "ssh not found"
command -v sshpass >/dev/null \
    || die "sshpass not found — brew install sshpass (or: brew install esolitos/ipa/sshpass)"

# Prefer ed25519, fall back to RSA. We install the PUBLIC key only.
PUBKEY_FILE=""
for f in ~/.ssh/id_ed25519.pub ~/.ssh/id_rsa.pub; do
    [ -f "$f" ] && { PUBKEY_FILE="$f"; break; }
done
[ -n "$PUBKEY_FILE" ] || die "no SSH public key found — run: ssh-keygen -t ed25519"
PUBKEY="$(cat "$PUBKEY_FILE")"

# Dedicated known_hosts: test servers get rebuilt at the same IP over
# and over, and each rebuild changes the host key. Keeping them out of
# ~/.ssh/known_hosts avoids both pollution and hard failures — wipe
# this file whenever you rebuild a server.
KNOWN_HOSTS="$HOME/.ssh/dokploy_test_known_hosts"
touch "$KNOWN_HOSTS" && chmod 600 "$KNOWN_HOSTS"
SSH_OPTS=(-p "$SSH_PORT"
          -o StrictHostKeyChecking=accept-new
          -o UserKnownHostsFile="$KNOWN_HOSTS"
          -o ConnectTimeout=10)

# ---- UI allowlist ----------------------------------------------------
if [ "$UI_OPEN" = 1 ]; then
    UI_ALLOW=""
    warn "Dokploy UI will be open to the world. Create the admin account IMMEDIATELY."
elif [ -z "$UI_ALLOW" ]; then
    log "Detecting your public IP for the UI allowlist…"
    MY_IP="$(curl -fsS --max-time 10 https://api.ipify.org || true)"
    if [ -z "$MY_IP" ]; then
        warn "Could not detect your public IP; leaving :3000 open. Pass --ui-allow <cidr> to restrict."
    else
        UI_ALLOW="${MY_IP}/32"
        log "Dokploy UI will accept connections only from ${UI_ALLOW} (plus SSH tunnels)."
    fi
fi

# ---- Step 1: install the SSH key (the only password-authed step) -----
log "Installing your SSH key on ${SERVER_IP} (root password prompt)…"
read -rs -p "root@${SERVER_IP} password: " SSHPASS; echo
export SSHPASS
# Force password auth here: a stale key in the agent would otherwise be
# offered first and eat the retry budget before the password is tried.
sshpass -e ssh "${SSH_OPTS[@]}" \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    "root@${SERVER_IP}" \
    "mkdir -p /root/.ssh && chmod 700 /root/.ssh \
     && touch /root/.ssh/authorized_keys && chmod 600 /root/.ssh/authorized_keys \
     && grep -qxF '${PUBKEY}' /root/.ssh/authorized_keys \
        || echo '${PUBKEY}' >> /root/.ssh/authorized_keys"
unset SSHPASS

# Never harden SSH before proving the key actually works.
log "Verifying key-based login…"
ssh "${SSH_OPTS[@]}" -o BatchMode=yes "root@${SERVER_IP}" true \
    || die "key auth failed — aborting before any lockdown"

# ---- Step 2: remote setup -------------------------------------------
log "Running server setup (this includes apt upgrade + the Dokploy installer; expect a few minutes)…"
# Args are single-quoted into the remote command string deliberately:
# ssh joins its argv with spaces and re-parses remotely, which silently
# DROPS empty arguments (an empty UI_ALLOW would shift every later
# positional). None of these values can contain a single quote.
ssh "${SSH_OPTS[@]}" "root@${SERVER_IP}" \
    "bash -s -- '$SERVER_IP' '$SSH_PORT' '$UI_ALLOW' '$KEEP_PW'" <<'REMOTE_SCRIPT'
set -euo pipefail
SERVER_IP="$1"; SSH_PORT="$2"; UI_ALLOW="$3"; KEEP_PW="$4"
export DEBIAN_FRONTEND=noninteractive

log() { printf '\033[1;34m  [remote]\033[0m %s\n' "$*"; }

[ "$(id -u)" = 0 ] || { echo "must run as root" >&2; exit 1; }
. /etc/os-release
[ "${ID:-}" = ubuntu ] || { echo "this script expects Ubuntu, found: ${ID:-unknown}" >&2; exit 1; }

log "apt update + upgrade…"
apt-get update -q
apt-get upgrade -yq

# iptables-persistent asks debconf questions; answer them up front. We
# save rules explicitly, so autosave stays off.
echo iptables-persistent iptables-persistent/autosave_v4 boolean false | debconf-set-selections
echo iptables-persistent iptables-persistent/autosave_v6 boolean false | debconf-set-selections
apt-get install -yq ca-certificates curl ufw fail2ban unattended-upgrades iptables-persistent

# Swap: Dokploy's minimum is 2 GB RAM and image builds spike memory —
# a modest swapfile keeps a small test VPS from OOM-killing builds.
if ! swapon --show | grep -q .; then
    log "No swap found — creating a 2G swapfile…"
    fallocate -l 2G /swapfile || dd if=/dev/zero of=/swapfile bs=1M count=2048
    chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
    grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

log "Enabling unattended security upgrades…"
cat > /etc/apt/apt.conf.d/20auto-upgrades <<'EOF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
EOF

log "Configuring fail2ban (sshd jail)…"
# backend=systemd: Ubuntu 24.04 ships no auth.log by default; the
# journal backend works on 22.04 too.
cat > /etc/fail2ban/jail.local <<EOF
[sshd]
enabled = true
port = ${SSH_PORT}
backend = systemd
maxretry = 5
bantime = 1h
EOF
systemctl enable --now fail2ban >/dev/null
systemctl restart fail2ban

log "Configuring ufw (${SSH_PORT}/tcp, 80, 443)…"
# NOTE: ufw does NOT govern Docker-published ports (Docker programs
# iptables ahead of it). It still protects host services — sshd above
# all. :3000 is handled separately in DOCKER-USER below.
ufw allow "${SSH_PORT}/tcp" >/dev/null
ufw allow 80/tcp  >/dev/null
ufw allow 443/tcp >/dev/null
ufw default deny incoming  >/dev/null
ufw default allow outgoing >/dev/null
ufw --force enable >/dev/null

if [ "$KEEP_PW" != 1 ]; then
    log "Locking SSH to key-only auth…"
    mkdir -p /etc/ssh/sshd_config.d
    cat > /etc/ssh/sshd_config.d/50-dokploy-hardening.conf <<'EOF'
PermitRootLogin prohibit-password
PasswordAuthentication no
KbdInteractiveAuthentication no
MaxAuthTries 4
EOF
    # Refuse to apply a config sshd can't parse — a bad reload here
    # would lock us out of the box entirely.
    sshd -t
    systemctl reload ssh
fi

if docker service inspect dokploy >/dev/null 2>&1; then
    log "Dokploy already installed — skipping installer."
else
    log "Installing Dokploy (official installer)…"
    # ADVERTISE_ADDR pins the swarm address explicitly instead of
    # trusting interface auto-detection on multi-IP VPSes.
    curl -sSL https://dokploy.com/install.sh | ADVERTISE_ADDR="${SERVER_IP}" sh
fi

if [ -n "$UI_ALLOW" ]; then
    log "Restricting Dokploy UI (:3000) to ${UI_ALLOW} via DOCKER-USER…"
    # Docker-published ports skip ufw, so the restriction lives in the
    # DOCKER-USER chain — the hook Docker guarantees to consult first.
    # --ctorigdstport matches the port as the client dialed it (the
    # packet is already DNATed by the time FORWARD sees it).
    # Re-runs (e.g. your laptop IP changed) first drop our old rules,
    # found by their comment marker. `|| true` because on first run
    # grep finds nothing and its exit 1 would kill the script under
    # pipefail.
    old_rules="$(iptables -S DOCKER-USER 2>/dev/null | grep -- 'dokploy-ui-guard' || true)"
    while read -r line; do
        [ -n "$line" ] && iptables ${line/#-A/-D}
    done <<< "$old_rules"
    iptables -I DOCKER-USER -p tcp -m conntrack --ctorigdstport 3000 --ctdir ORIGINAL \
        ! -s "${UI_ALLOW}" -m comment --comment dokploy-ui-guard -j DROP
    netfilter-persistent save >/dev/null
fi

log "Waiting for Dokploy to answer on :3000…"
for i in $(seq 1 60); do
    curl -fsS -o /dev/null http://127.0.0.1:3000 && { log "Dokploy is up."; exit 0; }
    sleep 3
done
echo "Dokploy did not come up within 3 minutes — check: docker service ps dokploy" >&2
exit 1
REMOTE_SCRIPT

# ---- Done ------------------------------------------------------------
cat <<EOF

$(printf '\033[1;32m==>\033[0m') Server ready.

  Dokploy UI:   http://${SERVER_IP}:3000
$( [ -n "$UI_ALLOW" ] && echo "                (reachable only from ${UI_ALLOW})" )
  SSH:          ssh -p ${SSH_PORT} root@${SERVER_IP}   (key-only$( [ "$KEEP_PW" = 1 ] && echo " lockdown SKIPPED" ))
  Tunnel:       ssh -p ${SSH_PORT} -N -L 3000:127.0.0.1:3000 root@${SERVER_IP}
                then browse http://localhost:3000 — works from any network.

  IMPORTANT: open the UI now and create the admin account — the first
  visitor to :3000 owns the instance.

  Next: provision a second server the same way, set both up, then test
  a full restore with:  ./migrate.sh <source-ip> <target-ip>
EOF
