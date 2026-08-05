#!/usr/bin/env bash
#
# migrate.sh — copy a Dokploy instance's FULL state (config, apps,
# named volumes) from one server to another, from your laptop.
#
#   ./migrate.sh [options] <source-ip> <target-ip>
#
# Both servers should have been set up with provision.sh (key-based
# root SSH), and the TARGET should be a fresh Dokploy install of the
# same version — its own state is replaced wholesale.
#
# What travels:            what doesn't:
#   /etc/dokploy             swarm state (node identity, certs)
#   (traefik config, certs,  running services — you redeploy each app
#    app build contexts,     from the restored UI; deploys remount the
#    ssh keys)               restored volumes, so app data is already
#   the Dokploy Postgres     in place
#   + Redis volumes
#   every NAMED docker
#   volume (app data)
#
# Anonymous volumes (64-hex names) are skipped on purpose: a redeploy
# creates fresh anonymous IDs, so restored copies would sit orphaned.
# If an app must keep its data across servers, give it a named volume.
#
# Steps: quiesce source -> pull backup to laptop -> restart source ->
# quiesce target -> restore -> restart target. The backup directory is
# left on your laptop as an artifact of the run.
#
# Options:
#   -p <port>            SSH port for both servers (default 22)
#   --force              proceed even if Dokploy versions differ
#   --leave-source-down  don't restart services on the source

set -euo pipefail

log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mWARN:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

SSH_PORT=22
FORCE=0
LEAVE_SOURCE_DOWN=0
SRC=""
DST=""

while [ $# -gt 0 ]; do
    case "$1" in
        -p) SSH_PORT="$2"; shift 2 ;;
        --force) FORCE=1; shift ;;
        --leave-source-down) LEAVE_SOURCE_DOWN=1; shift ;;
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        -*) die "unknown option: $1" ;;
        *) if [ -z "$SRC" ]; then SRC="$1"; else DST="$1"; fi; shift ;;
    esac
done
[ -n "$SRC" ] && [ -n "$DST" ] || die "usage: $0 [options] <source-ip> <target-ip>"
[ "$SRC" != "$DST" ] || die "source and target are the same server"

KNOWN_HOSTS="$HOME/.ssh/dokploy_test_known_hosts"
touch "$KNOWN_HOSTS" && chmod 600 "$KNOWN_HOSTS"
SSH_OPTS=(-p "$SSH_PORT"
          -o StrictHostKeyChecking=accept-new
          -o UserKnownHostsFile="$KNOWN_HOSTS"
          -o BatchMode=yes
          -o ConnectTimeout=10)

src() { ssh "${SSH_OPTS[@]}" "root@${SRC}" "$@"; }
dst() { ssh "${SSH_OPTS[@]}" "root@${DST}" "$@"; }

# ---- Preflight -------------------------------------------------------
log "Preflight: checking both servers…"
src true || die "cannot SSH to source ${SRC} (key auth)"
dst true || die "cannot SSH to target ${DST} (key auth)"

# Swarm pins the resolved digest into the service spec, so comparing
# the Image field catches 'latest'-drift between install days — a
# schema-mismatched Postgres restore fails in confusing ways.
SRC_IMG="$(src "docker service inspect dokploy --format '{{.Spec.TaskTemplate.ContainerSpec.Image}}'" 2>/dev/null)" \
    || die "no dokploy service on source — is Dokploy installed?"
DST_IMG="$(dst "docker service inspect dokploy --format '{{.Spec.TaskTemplate.ContainerSpec.Image}}'" 2>/dev/null)" \
    || die "no dokploy service on target — provision it first"
if [ "$SRC_IMG" != "$DST_IMG" ]; then
    warn "Dokploy version mismatch:"
    warn "  source: ${SRC_IMG}"
    warn "  target: ${DST_IMG}"
    [ "$FORCE" = 1 ] || die "refusing to restore across versions (--force to override)"
fi

BACKUP_DIR="dokploy-backup-$(date +%Y%m%d-%H%M%S)"
mkdir -p "${BACKUP_DIR}/volumes"
log "Backup artifact directory: ${BACKUP_DIR}/"

# Scale every replicated service on a host to 0 and wait for tasks to
# drain, so the Postgres/volume copy sees quiescent files. Prints the
# pre-stop desired replica counts on stdout ("name count" lines) so
# the caller can restore them afterwards. Global services (if any)
# can't be scaled — they keep running, which is fine: they don't own
# the volumes we copy.
QUIESCE='
set -eu
# Trailing `_` absorbs suffixes like "(max 1 per node)" that would
# otherwise ride along in the replicas field.
docker service ls --format "{{.Name}} {{.Mode}} {{.Replicas}}" | while read -r name mode replicas _; do
    [ "$mode" = "replicated" ] || continue
    echo "$name ${replicas#*/}"
    docker service scale -d "$name=0" >/dev/null
done
for i in $(seq 1 40); do
    busy=$(docker service ls --format "{{.Mode}} {{.Replicas}}" \
        | awk "\$1==\"replicated\" && \$2!=\"0/0\"" | wc -l)
    [ "$busy" = 0 ] && break
    sleep 3
done
sleep 3   # let containers finish exiting after tasks report 0/0
'

# ---- Quiesce + pull from SOURCE -------------------------------------
log "Quiescing services on source (recording replica counts)…"
src "bash -s" <<<"$QUIESCE" > "${BACKUP_DIR}/source-replicas.txt"
log "Source services stopped: $(wc -l < "${BACKUP_DIR}/source-replicas.txt" | tr -d ' ') recorded"

log "Pulling /etc/dokploy…"
src "tar czf - -C /etc dokploy" > "${BACKUP_DIR}/etc-dokploy.tgz"

log "Enumerating named volumes…"
src "docker volume ls -q" | grep -Ev '^[0-9a-f]{64}$' > "${BACKUP_DIR}/volumes.txt" || true
[ -s "${BACKUP_DIR}/volumes.txt" ] || die "no named volumes found on source — nothing to migrate?"

while read -r vol; do
    log "  pulling volume: ${vol}"
    src "tar czf - -C /var/lib/docker/volumes/${vol}/_data ." \
        > "${BACKUP_DIR}/volumes/${vol}.tgz"
done < "${BACKUP_DIR}/volumes.txt"

if [ "$LEAVE_SOURCE_DOWN" = 1 ]; then
    warn "Leaving source services down (--leave-source-down)."
else
    log "Restarting services on source…"
    while read -r name count; do
        src "docker service scale -d ${name}=${count} >/dev/null"
    done < "${BACKUP_DIR}/source-replicas.txt"
fi

# ---- Restore onto TARGET --------------------------------------------
log "Quiescing services on target…"
dst "bash -s" <<<"$QUIESCE" > "${BACKUP_DIR}/target-replicas.txt"

log "Restoring /etc/dokploy (previous kept at /etc/dokploy.pre-restore)…"
dst "rm -rf /etc/dokploy.pre-restore \
     && { [ -e /etc/dokploy ] && mv /etc/dokploy /etc/dokploy.pre-restore || true; } \
     && tar xzf - -C /etc" < "${BACKUP_DIR}/etc-dokploy.tgz"

while read -r vol; do
    log "  restoring volume: ${vol}"
    # Create-if-missing, then empty it — the target's fresh Dokploy
    # already seeded its own Postgres data, which must not be merged
    # with the source's.
    dst "docker volume create ${vol} >/dev/null \
         && find /var/lib/docker/volumes/${vol}/_data -mindepth 1 -delete \
         && tar xzf - -C /var/lib/docker/volumes/${vol}/_data" \
        < "${BACKUP_DIR}/volumes/${vol}.tgz"
done < "${BACKUP_DIR}/volumes.txt"

log "Restarting Dokploy stack on target…"
while read -r name count; do
    dst "docker service scale -d ${name}=${count} >/dev/null"
done < "${BACKUP_DIR}/target-replicas.txt"

log "Waiting for target UI on :3000…"
for i in $(seq 1 60); do
    dst "curl -fsS -o /dev/null http://127.0.0.1:3000" 2>/dev/null && break
    [ "$i" = 60 ] && die "target Dokploy didn't come back — check: docker service ps dokploy"
    sleep 3
done

cat <<EOF

$(printf '\033[1;32m==>\033[0m') Restore complete. Backup artifact kept in ${BACKUP_DIR}/

  Verify on http://${DST}:3000 (or via SSH tunnel):
    1. Log in with the SOURCE instance's admin credentials — the whole
       config database moved, including users.
    2. Settings -> Web Server: update the Server IP field — it still
       holds the source's address (${SRC}).
    3. Redeploy each application/database. Swarm services don't
       migrate, but every named volume was restored first, so deploys
       come back up with the source's data.
    4. Point DNS at ${DST} before expecting domains + certificates to
       route; Traefik config and acme.json came across in /etc/dokploy.
EOF
