# TransactVault

Modern real estate transaction management for California brokerages. A Rust + Axum + SurrealDB v3 + Askama + Datastar SaaS PoC that demonstrates the full compliance workflow — brokerage signup, invitation-based onboarding, transaction lifecycle, checklist-driven compliance, versioned documents, and one-click audit-ready export.

## Stack

- **Rust 2024** with Axum 0.8, Tokio, Tower
- **SurrealDB v3** (graph-first; `RecordId` everywhere, no string IDs)
- **RustFS** for S3-compatible object storage (documents, versioned uploads)
- **Resend** for transactional email (welcome + invitation messages)
- **Stripe** (`async-stripe`) for subscription billing — admin-managed tiers sync as Stripe Products/Prices
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

### Billing (Stripe)

Pricing tiers are managed entirely in-app under `/admin/tiers` — admins
set the name, price, user/transaction limits, and optional per-tx
overage fee, and the controller syncs each tier as a Stripe Product +
recurring Price on save. The app is the source of truth; Stripe holds
the payment rail.

`STRIPE_SECRET_KEY` is empty by default — tier CRUD still works, but
the Stripe sync step is skipped and a warning banner is shown in the
admin UI. To turn on real sync, set:

- `STRIPE_SECRET_KEY` — test- or live-mode secret (`sk_test_…` /
  `sk_live_…`)
- `STRIPE_WEBHOOK_SECRET` — `whsec_…` from the Dashboard, used to
  verify incoming webhook payloads (Phase 2)
- `STRIPE_TRIAL_DAYS` — free-trial length on Checkout, default `14`,
  set `0` to disable

Tier mechanics:

- **Mutable Product, immutable Price.** Editing a tier's name or
  description updates the existing Stripe Product in place. Editing
  the price always creates a fresh Stripe Price (Stripe Prices are
  immutable); existing subscribers stay on their old Price until the
  next billing cycle.
- **Unlimited via `-1`.** `user_limit` and `transaction_limit` use `-1`
  as a sentinel for "no cap", so DB indexes work uniformly.
- **Optional metered overage.** If `overage_fee_cents_per_tx` is set,
  a second Stripe Price (metered, monthly) is created alongside the
  recurring Price; the subscribe + usage-reporting flow uses it to
  bill per-transaction overage at the end of each cycle.
- **Archive ≠ delete.** Flipping `is_archived` flips
  `Product.active=false` in Stripe so the tier disappears from the
  public Subscribe flow, but existing subscriptions keep working.

## Project layout

```
db/schema.surql           SCHEMAFULL tables + graph relations (incl. tier + brokerage.stripe_*)
src/
├── main.rs               startup, logging, listener
├── config.rs             env-driven config (Stripe, RustFS, Resend, JWT…)
├── router.rs             routes: public marketing + /app + /admin + /healthcheck
├── state.rs              AppState (db + config + Stripe + Mailer + storage)
├── error.rs              AppError + IntoResponse
├── stripe.rs             async-stripe wrapper (sync_tier, archive_product)
├── db/                   connect + apply schema
├── auth/                 JWT, Argon2, cookie extractor
├── models/               User, Brokerage, Transaction, Checklist, Document, Tier, Comment, …
├── controllers/          handlers grouped by feature (incl. admin/tiers)
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
- `/admin/tiers` — super-admins create / edit / archive pricing tiers; each save round-trips to Stripe to keep Products + Prices aligned (see [Billing (Stripe)](#billing-stripe))

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
- Stripe billing is partially shipped:
  - **Done.** Admin-managed tiers, Product/Price sync, archive flow, schema fields on `brokerage` for subscription state.
  - **Pending.** Public Subscribe button → Checkout Session, customer-portal link, `/webhooks/stripe` handler, read-only gate during cancel-grace, usage enforcement + metered overage reporting.
