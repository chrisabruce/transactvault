# Stripe: from sandbox to real credit cards

The cutover, in order. Steps 1 and 2 happen in the Stripe Dashboard, 3 and 4
on the server, 5 through 7 in the app. Total hands-on time is about an hour,
most of it Stripe's activation form. Nothing here touches the test-mode data
you've been using — test mode keeps working for local dev afterwards.

What actually changes: two environment variables (`STRIPE_SECRET_KEY`,
`STRIPE_WEBHOOK_SECRET`), one press of "Re-link to Stripe" in Admin → Tiers,
and one cleanup query for brokerages that subscribed during testing.

---

## 1. Activate the live account

Dashboard → the "Activate payments" prompt (top banner, or Settings →
Account status).

Stripe will ask for:

- Business type + EIN — TransactVault, LLC, the Sheridan WY details.
- Your identity (SSN last-4 or full, depending on structure) — required by
  KYC rules, applies to whoever controls the account.
- A bank account for payouts.
- **Statement descriptor** — what brokers see on their card statement.
  Set it to `TRANSACTVAULT` (and the shortened one to `TVAULT` or similar).
  A descriptor people don't recognize is the #1 cause of disputes.
- Customer support email/phone — use hello@transactvault.app.

Approval is usually minutes, occasionally a day if they want documents.

## 2. Configure the live-mode pieces that do NOT carry over from test

Everything below is per-mode in Stripe. Having set it in test mode does
nothing for live. With the Dashboard toggled to **Live mode**:

1. **Customer portal** — Settings → Billing → Customer portal. Click through
   and **Save** (even if you change nothing). Until a live portal
   configuration exists, the app's "Manage subscription" button errors with
   "No configuration provided". Recommended settings: allow updating payment
   methods, allow cancellation (at period end), show invoice history.
2. **Customer emails** — Settings → Emails. Turn ON "Successful payments"
   (receipts) and "Failed payments". They default off; brokers expect
   receipts for a business expense.
3. **Branding** — Settings → Branding. Upload the logo and set the teal
   (#0f766e) so the Checkout page and receipts look like TransactVault, not
   generic Stripe.
4. **Payment retries** — Settings → Billing → Revenue recovery. Leave Smart
   Retries on. The app already shows the past-due banner off the
   `invoice.payment_failed` webhook; Stripe retrying quietly in the
   background is what usually fixes it.

## 3. Live API key + live webhook endpoint

Still in **Live mode**:

1. **Developers → API keys** → copy the **Secret key** (`sk_live_…`).
   The app needs one server key; the publishable key is unused (Checkout is
   hosted by Stripe). Optional hardening: create a **restricted key**
   instead, with write on Customers, Checkout Sessions, Products, Prices,
   Subscriptions, Billing Portal, and Usage Records.
2. **Developers → Webhooks** (newer dashboards call it Event destinations)
   → Add endpoint:
   - URL: `https://transactvault.app/webhooks/stripe`
   - Events — the app handles exactly these five; select them rather than
     "all events" so the log stays readable:
     - `customer.subscription.created`
     - `customer.subscription.updated`
     - `customer.subscription.deleted`
     - `customer.subscription.trial_will_end`
     - `invoice.payment_failed`
   - Copy the endpoint's **Signing secret** (`whsec_…`). This is per
     endpoint: the live secret is different from the test one, and a
     mismatch is the classic silent breaker. (If it ever happens, the app
     records the reason in Admin → Errors rather than dropping it silently.)

## 4. Flip the environment on the server

In Dokploy, on the app's Environment tab:

```
STRIPE_SECRET_KEY=sk_live_…
STRIPE_WEBHOOK_SECRET=whsec_…   # the LIVE endpoint's secret from step 3
# STRIPE_TRIAL_DAYS stays as is (14)
```

Redeploy/restart. This is a one-minute blip; if you want zero weirdness for
anyone mid-session, set the heads-up banner from Admin → Ops beforehand, or
flip maintenance mode on for the minute.

## 5. Re-link the tiers (in the app)

The three tiers still point at **test-mode** product/price IDs, which the
live account cannot see — every Subscribe would fail with "No such price".

Admin → Tiers → **Re-link to Stripe** (super-admin, asks for confirmation).
It creates fresh Products and Prices (including the metered overage prices)
in the live account and repoints every tier. Verify in the Stripe Dashboard
(Live → Product catalog) that the tiers appeared, with the right amounts.

## 6. Clean up sandbox-era brokerage links — BEFORE anyone subscribes live

Any brokerage that went through Checkout during testing has a test-mode
`cus_…` stored on its row. The app reuses a stored customer ID as-is, so in
live mode those brokerages would hit "No such customer" the moment they try
to subscribe. At cutover time every stored ID is a test ID, so it's safe to
clear them all — run once against the production database:

```sql
UPDATE brokerage SET
    stripe_customer_id = NONE,
    stripe_subscription_id = NONE,
    subscription_status = NONE,
    current_period_end = NONE,
    cancel_at = NONE
WHERE stripe_customer_id != NONE;
```

Do NOT run this again after real subscriptions exist — it would orphan them.
While you're in there: the Antelope Valley design partners can skip billing
entirely via Admin → Brokerages → comp toggle if that's the arrangement.

## 7. Prove it end to end with a real card

Because every subscription starts with the 14-day trial, the live test
costs $0 today — Checkout validates the card without charging it.

1. Sign in as a broker (a scratch brokerage is fine) and hit Subscribe.
2. The Checkout page should NOT show the orange "TEST" badge, and
   `4242 4242 4242 4242` should be declined — both prove you're live. Use a
   real card; it will be authenticated, not charged.
3. After the redirect, the app should show the trial banner with the
   countdown (that's the webhook + return-flow working).
4. Stripe Dashboard → the customer and subscription exist, status
   `trialing`; Developers → Webhooks → the endpoint shows 200s.
5. Admin → Errors in the app: no rejected-webhook rows.
6. "Manage subscription" opens the live portal (proves step 2.1).
7. Cancel from the portal, confirm the app shows the canceling state, then
   delete the scratch brokerage if you used one.

## 8. First weeks of live traffic

- Stripe emails you when webhook deliveries fail repeatedly; don't filter
  it out.
- `invoice.payment_failed` → the broker sees the red banner; Smart Retries
  usually clears it without you doing anything.
- Overage billing: metered usage the app reports shows up on the upcoming
  invoice line for the overage price — glance at the first over-limit
  brokerage's invoice to see it flowing.
- Disputes land in Dashboard → Payments → Disputes; respond within the
  deadline or they auto-lose.
- Sales tax is out of scope here: if/when it matters, Stripe Tax can be
  turned on per-mode, but talk to the accountant first.

## Reference: what lives where

| Thing | Where |
| --- | --- |
| Secret key / webhook secret | Dokploy env → `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET` |
| Trial length | `STRIPE_TRIAL_DAYS` env (default 14) |
| Tier ↔ Stripe linkage | Admin → Tiers (+ Re-link button) |
| Comp access (skip billing) | Admin → Brokerages |
| Rejected webhook diagnostics | Admin → Errors |
| Webhook endpoint | `POST /webhooks/stripe`, signature-verified |
