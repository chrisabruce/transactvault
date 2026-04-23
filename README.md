# TransactVault

Modern real estate transaction management for California brokerages. A Rust + Axum + SurrealDB v3 + Askama + Datastar SaaS PoC that demonstrates the full compliance workflow — brokerage signup, invitation-based onboarding, transaction lifecycle, checklist-driven compliance, versioned documents, and one-click audit-ready export.

## Stack

- **Rust 2024** with Axum 0.8, Tokio, Tower
- **SurrealDB v3** (graph-first; `RecordId` everywhere, no string IDs)
- **RustFS** for S3-compatible object storage (documents, versioned uploads)
- **Resend** for transactional email (welcome + invitation messages)
- **Askama 0.14** server-side templating
- **Datastar** CDN for progressive enhancement (loaded in `base.html`)
- **Argon2id** password hashing, **JWT** cookie sessions
- Pure **CSS custom properties** design system — no frameworks, no inline styles

## Quick start

Easiest path — everything wired up with one command:

```bash
cp .env-example .env          # edit JWT_SECRET before anything serious
docker compose up --build
```

Ports are deliberately unusual so the stack coexists with other local
dockers (no clashes on 3000/8000/9000):

| Service            | Host port | Purpose                       |
|--------------------|-----------|-------------------------------|
| TransactVault app  | `37420`   | <http://localhost:37420>      |
| RustFS S3 API      | `37421`   | Used by the app               |
| SurrealDB HTTP/WS  | `37422`   | <http://localhost:37422>      |
| RustFS console     | `37423`   | <http://localhost:37423>      |

For `cargo run` workflows the app still needs a RustFS instance, because
uploads go through the S3 API rather than the local filesystem:

```bash
docker compose up -d surrealdb rustfs   # backing services only
cargo run                               # app on :37420
```

### Email delivery

`RESEND_API_KEY` is empty by default — outbound messages (welcome + team
invites) are logged at INFO level instead of delivered. To turn on real
delivery, set `RESEND_API_KEY` and `RESEND_FROM` (a domain you've verified
with Resend). `RESEND_REPLY_TO` is optional.

### Object storage

`RUSTFS_*` configures the S3-compatible client. The bucket is created on
first boot if it doesn't exist. Any S3 provider (MinIO, AWS S3) works —
just point `RUSTFS_ENDPOINT` at it. With AWS S3 proper, leave
`RUSTFS_ENDPOINT` set to the regional endpoint (`https://s3.us-east-1.amazonaws.com`).

## Project layout

```
db/schema.surql           SCHEMAFULL tables + graph relations
src/
├── main.rs               startup, logging, listener
├── config.rs             env-driven config
├── router.rs             routes: public marketing + /app + /healthcheck
├── state.rs              AppState (db + config)
├── error.rs              AppError + IntoResponse
├── db/                   connect + apply schema
├── auth/                 JWT, Argon2, cookie extractor
├── models/               User, Brokerage, Transaction, Checklist, Document
├── controllers/          handlers grouped by feature
└── templates.rs          Askama template structs
templates/                HTML — pages/, partials/, components/
static/css/main.css       single stylesheet, CSS custom properties
```

## Key workflows

- `/signup` — first user creates the brokerage and becomes the broker
- `/app/team` — brokers invite agents / transaction coordinators by email; share the generated link
- `/app/transactions/new` — creates a transaction, seeds the California default checklist, wires `brokerage→has_transaction→tx` and `user→owns→tx`
- Checklist items toggle via plain form POST; completing all items flips the transaction into a calm green **Compliance Complete** state
- Documents upload via multipart, auto-version by filename, and link back via `tx→has_document→doc` plus an `uploaded` edge for the audit trail
- `/app/transactions/:id/export` zips every document with a MANIFEST cover sheet

## Graph model

```
(user) -works_at-> (brokerage)
(user) -owns-> (transaction)
(brokerage) -has_transaction-> (transaction)
(transaction) -has_item-> (checklist_item)
(transaction) -has_document-> (document)
(user) -completed-> (checklist_item)   # stored on checklist_item.completed_by
(user) -uploaded-> (document)
(document) -version_of-> (document)    # previous version
```

## Healthcheck

`GET /healthcheck` returns JSON with version, DB status, and system metrics.

## Security notes

- Every authenticated route runs through the `CurrentUser` extractor, which pulls the JWT, looks up the user, and confirms brokerage membership on every request.
- Every transaction handler calls `authorize_transaction` before doing anything, which verifies the caller's brokerage owns the record and (for agents) that they own it.
- SurrealQL is always parameterized — user input never string-interpolates into queries.
- Argon2id default parameters are OWASP-aligned.
- Cookies are HTTP-only, `SameSite=Lax`, and scoped to the app lifetime (`JWT_EXPIRY_HOURS`).

## Not yet included (scoped for the PoC)

- Google SSO is stubbed in the UI — the working flow is email/password.
- Stripe/billing integration — the plan field exists on `brokerage` and the pricing page is real, but no payments are taken.
