# Production deployment

Production runs **one container**: the app, built from the root
`Dockerfile`. Everything it depends on is managed independently and reached
over the network:

```
Internet ── Traefik / your proxy (TLS) ──▶ transactvault :37420
                                                │
                                                ├──▶ SurrealDB          (wherever you run it)
                                                ├──▶ S3-compatible store (documents)
                                                └──▶ Postmark           (email)
```

The app holds no local state. No volumes, no sidecars, no compose file —
scale it horizontally or redeploy it at will.

> `docker-compose.yml` in this repo is **development only** (app +
> SurrealDB + RustFS on one machine). Don't deploy it.

---

## 1. Build

Dokploy **Application**, source = this Git repo, build type = **Dockerfile**.
The bundled `Dockerfile` needs no changes: it builds a release binary and
copies `static/`, `templates/` and `db/` alongside it.

- **Port:** `37420` (`EXPOSE 37420`, also the `PORT` default)
- **Health check:** `GET /healthcheck` — unauthenticated, no storage
  round-trip, reports database reachability only
- **Domain:** your hostname with TLS at the proxy

### Build memory is the one real gotcha

The release profile is `lto = "thin"` with `codegen-units = 1` across ~640
crates including all of SurrealDB. **This OOMs on a default Docker
allocation** — confirmed on a dev machine, where the build was SIGKILLed
part-way through `surrealdb-core`. It surfaces as `ResourceExhausted` /
`cannot allocate memory`, or just a killed builder with no clear error.

Give the builder ~8 GB, or add swap on the VPS. If neither is practical,
build the image somewhere roomier and have Dokploy pull the tag instead of
building on the host. Reach for weakening the release profile last — it is
what makes the binary fast.

---

## 2. SurrealDB

Run it however you like — a container on the same host, a dedicated box, a
cluster behind a load balancer, or a managed/cloud instance. The app only
needs a URL and credentials, so you can move or re-platform it later
without touching the app.

What the app requires:

- **A reachable endpoint.** `SURREAL_URL` accepts `ws://`, `wss://`,
  `http://` and `https://`. Use **`wss://`** whenever the connection leaves
  a trusted network — credentials and every query travel over it. Anything
  else (`mem://`, `rocksdb://`, …) is treated as an *embedded* engine and
  skips authentication entirely, which is never what you want in
  production.
- **Root credentials.** The app signs in with `SURREAL_USER` /
  `SURREAL_PASS` at root level, then selects `SURREAL_NS` / `SURREAL_DB`.
  A namespace- or database-scoped user will fail to sign in — worth knowing
  before provisioning a locked-down account on a managed service.
- **Version 3.x**, matching the `surrealdb` crate in `Cargo.lock` (3.2.3
  today). In-major upgrades are data-compatible; export before bumping.
- **Not exposed to the internet.** Bind it to a private interface or put it
  behind a firewall/VPN. It is not designed to face the public web.

The app retries the initial connection with backoff and runs a heartbeat
afterwards, so a database that is briefly unavailable or restarting doesn't
require an app restart.

Schema is applied on **every** boot (`DEFINE` statements are additive), and
the form catalog and pricing tiers seed once. There is no separate
migration step, and redeploys — including several app instances starting at
the same time — are safe.

### Running it on Dokploy (the simple starting point)

If you don't have a database host yet, run SurrealDB as its own Dokploy
**Compose** service — separate from the app, so replacing it later with a
cluster or a managed instance is just a change to `SURREAL_URL`.

Dokploy → **Create Service** → **Compose**, and paste:

```yaml
services:
  surrealdb:
    image: surrealdb/surrealdb:v3.2.3
    container_name: tv-surrealdb
    restart: unless-stopped
    # REQUIRED with a named volume. Without it the image's non-root
    # default cannot write to /data and the container exits immediately
    # with "There was a problem with a transaction" — which reads like
    # corruption rather than a permissions problem. Verified against
    # v3.2.3. The container is the isolation boundary, not the UID.
    user: "0:0"
    command:
      - start
      - --user=${SURREAL_USER:?set SURREAL_USER}
      - --pass=${SURREAL_PASS:?set SURREAL_PASS}
      - --bind=0.0.0.0:8000
      - rocksdb:/data/tv.db
    # No `ports:` — deliberately. Publishing 8000 puts a root-credentialled
    # database on the public internet. The app reaches it over the shared
    # network by name.
    expose:
      - "8000"
    volumes:
      - tv-surreal-data:/data
    networks:
      - dokploy-network

volumes:
  tv-surreal-data:

networks:
  dokploy-network:
    external: true
```

Then in that service's **Environment** tab:

```
SURREAL_USER=root
SURREAL_PASS=<openssl rand -base64 36>
```

The `:?` markers make the deploy fail loudly rather than quietly starting a
database with blank credentials.

Point the app at it with:

```
SURREAL_URL=ws://tv-surrealdb:8000
```

`ws://` (not `wss://`) is correct here — the traffic never leaves the
Docker network. Switch to `wss://` the moment the database moves to
another host.

**Name it uniquely.** Short-name DNS is not project-scoped across a shared
external network, so a service called plain `surrealdb` can collide with
any other SurrealDB already running on that Dokploy host. `tv-surrealdb`
is unambiguous; the `container_name` and the service name agree so either
resolves.

**Keep the named volume.** Without it RocksDB writes into the container
layer and every redeploy silently starts from an empty database. With it,
data survives destroying and recreating the container — worth confirming
yourself once, before there is anything in there you care about.


---

## 3. Object storage

Any S3-compatible service. Requests are always **path-style**
(`endpoint/bucket/key`), which is what non-AWS providers expect.

Two values to copy exactly from your provider's console:

- **`S3_REGION`** is part of the SigV4 signature, not just routing. A wrong
  region fails as `SignatureDoesNotMatch`, which reads like a bad secret
  key and sends you debugging the wrong thing.
- **`S3_ENDPOINT`** — include the scheme, no trailing slash, no bucket.

**Set `S3_AUTO_CREATE_BUCKET=false`** when the bucket already exists.
Otherwise the app issues `CreateBucket` at startup (which is how the local
dev bucket gets made), and keys scoped to a single bucket answer `403
AccessDenied` — *not* the "already exists" response the startup check
forgives — so the app retries ten times and then refuses to boot against a
perfectly good bucket.

### Verify credentials before deploying

Separates "storage is misconfigured" from "the app is broken":

```bash
AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... \
aws --endpoint-url "$S3_ENDPOINT" --region "$S3_REGION" s3 ls "s3://$S3_BUCKET/"
```

An empty listing is success. `SignatureDoesNotMatch` → region or secret is
wrong. `AccessDenied` → the key lacks rights to that bucket.

---

## 4. Environment

```bash
# --- identity ---------------------------------------------------------
APP_NAME=TransactVault
BASE_URL=https://app.example.com     # exact public origin, https, no trailing slash
HOST=0.0.0.0
PORT=37420
PRETTY_LOGS=true                     # false for JSON, if logs feed an aggregator

# --- database ---------------------------------------------------------
SURREAL_URL=wss://db.internal:8000   # ws:// only on a trusted network
SURREAL_USER=root
SURREAL_PASS=<long random>
SURREAL_NS=transactvault
SURREAL_DB=app

# --- secrets ----------------------------------------------------------
JWT_SECRET=<openssl rand -base64 48>
SUPERADMIN_EMAILS=you@example.com    # comma-separated; grants /admin

# --- proxy ------------------------------------------------------------
TRUSTED_PROXY_HOPS=1                 # see §5

# --- documents --------------------------------------------------------
S3_ENDPOINT=https://<endpoint>
S3_REGION=<region>
S3_ACCESS_KEY=<key>
S3_SECRET_KEY=<secret>
S3_BUCKET=<bucket>
S3_AUTO_CREATE_BUCKET=false
# Optional: browser-facing endpoint for presigned direct uploads, when
# it differs from S3_ENDPOINT (private networking, internal DNS). Most
# managed providers need only S3_ENDPOINT. The app also tries to set
# the bucket's CORS rule (PUT from BASE_URL) at boot; if your provider
# rejects that API, add the rule once in their console — until then
# uploads silently fall back to proxying through the app.
# S3_PUBLIC_ENDPOINT=

# --- email ------------------------------------------------------------
POSTMARK_SERVER_TOKEN=<token>
POSTMARK_FROM=TransactVault <no-reply@example.com>
POSTMARK_MESSAGE_STREAM=outbound

# --- billing (optional; omit to run without Stripe) --------------------
STRIPE_SECRET_KEY=
STRIPE_WEBHOOK_SECRET=

# --- notifications ----------------------------------------------------
# Who gets emailed when someone sends feedback or uses the contact form.
# Comma-separated; empty disables the email (messages are still stored
# and visible in Admin -> Feedback). Reply-To is set to the sender.
NOTIFY_EMAILS=jason@transactvault.app,chris@transactvault.app

# --- operations (optional) --------------------------------------------
# MAINTENANCE_MODE=true              # boot straight into maintenance; see §8

# --- MUST NOT BE SET --------------------------------------------------
# DEV_RESET_ON_BOOT
```

`BASE_URL` is load-bearing beyond cosmetics: `https://` is what turns on
`Secure` session cookies and HSTS, and it is the origin the CSRF fallback
compares against.

The app refuses to start if `JWT_SECRET` is under 32 characters, contains
`change-me`, or matches a known dev value — and if `DEV_RESET_ON_BOOT` is
set on an `https://` deployment, since that wipes every user, transaction
and document.

The legacy `RUSTFS_*` names still work as fallbacks for the `S3_*` ones. If
both are set, `S3_*` wins.

---

## 5. `TRUSTED_PROXY_HOPS`

Decides which `X-Forwarded-For` entry is treated as the real client,
counting **from the right**. Every rate limit and the audit log key off it,
so a wrong value either lets one attacker exhaust everyone's quota or lets
them dodge limits by spoofing the header.

- One reverse proxy (Traefik alone): **1**
- Cloudflare in front of Traefik: **2**
- App reachable directly, no proxy: **0**

If the count exceeds the real chain, the lookup falls back to the socket
peer rather than trusting client-supplied data — it fails closed, so when
unsure start low and raise it.

---

## 6. First boot

Expected log lines: `schema applied`, then `bucket ready` *or* `skipping
bucket creation`, then `listening`.

Then:

1. `GET /healthcheck` → `200`.
2. Sign up. The account is created **unverified**, and the verification
   link deliberately does not sign you in — you'll land on the login page.
3. Confirm the email arrived. With `POSTMARK_SERVER_TOKEN` empty, mail is
   logged instead of sent and nothing will show up.
4. Visit `/admin` to confirm `SUPERADMIN_EMAILS` matched (compared
   lowercase against your signed-in address).
5. Upload a document and download it back — the only check that proves the
   credentials, path-style addressing and region are all correct together.

---

## 7. Backups

The database holds everything that isn't document bytes:

```bash
surreal export --endpoint "$SURREAL_URL" --user "$SURREAL_USER" \
  --pass "$SURREAL_PASS" --ns transactvault --db app backup.surql
```

Documents live in object storage and are **not** covered by that — enable
versioning or a lifecycle policy on the bucket if you want protection
against deletion.

---

## 8. Maintenance mode

While maintenance mode is on, every app route answers `503` with a calm
"back in a few minutes" page (plus `Retry-After`, so search engines treat
it as temporary). The marketing pages, `/login`, `/healthcheck`, and
`/admin/*` stay reachable — the last two so the platform keeps the
container alive and a super-admin can reach the switch. Everything that
writes (the app, signup, verify/reset/invite links, Stripe webhooks) is
gated; Stripe retries 5xx for days, so webhook events queue up instead of
landing in a database that is mid-restore.

Two ways to turn it on:

- **Planned, app stays up** (database restore in place): flip the switch
  at `/admin/ops`. It takes effect immediately, is written to
  `system_setting:main` so a restart with a healthy database resumes in
  maintenance, and is audited.
- **Server move / database unreachable**: set `MAINTENANCE_MODE=true` in
  the environment and (re)start. The gate never touches the database, so
  the page serves even with no database behind the app. Remove the
  variable once done — while it is set, every boot re-enters maintenance
  regardless of the admin switch.

**Restore runbook:** announce it first with the notice banner
(`/admin/ops`, a day ahead) → flip maintenance on → take the backup /
restore / move → verify → flip it off. The notice banner is separate from
the gate, so you can announce without gating and gate without announcing.
