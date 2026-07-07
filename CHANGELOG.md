# What's new

A plain-English summary of recent updates. For technical release notes, see
the commit history. The version number for the build you're using appears at
the bottom-right of every page.

---

## July 2026

### v0.4.0 — Live search, real-time fixes, team exports, and the full CAR catalog

- **Search-as-you-type actually works now.** On the Transactions page,
  typing in the search box used to reload the whole page — which froze
  mid-word and threw your cursor out of the box. Results now update
  live beneath the toolbar while you type; the cursor never moves.
  The Search page gained the same live behavior (it previously
  required pressing Enter).
- **Real-time dashboard updates are fixed for real this time.** Two
  separate bugs stacked up: the page never actually opened its live
  connection (the browser library renamed the attribute that starts
  it, and the old name was silently ignored), and even when connected,
  updates arrived in a format the library couldn't fully read. Both
  are fixed, verified in a real browser against the exact library
  build we ship, and pinned by regression tests. Numbers update
  instantly as your team works — approvals, uploads, comments, new
  transactions, status changes.
- **All 10 missing CAR forms are in the Add-an-item list**, including
  the four forms CAR released in June 2026: PRBS-B, PRBS-S, SWPI-C,
  and SWPI-Q. The picker now shows the complete CAR catalog — forms
  already on the checklist are no longer hidden from the list (that
  read as "missing"); adding one twice is politely refused instead,
  and repeatable forms like Addenda and Counter Offers can still be
  added as many times as needed.
- **Your own form codes now show on checklists.** Forms you add to
  your brokerage library (e.g. RNTD, AD, R&R, CCR, SDR) display their
  code chip on the checklist just like CAR forms, and uploads against
  them are filed under that code instead of MISC.
- **Team page exports.** Each member row has an **Export ZIP** button
  that downloads every document across that agent's transactions,
  organized by property and form code with a manifest. A new
  **Brokerage archive** section (above the danger zone) downloads the
  entire brokerage in one ZIP, organized alphabetically by agent.
  Broker-only, capped at 400 MB per archive — export individual
  transactions if you're over.
- **Admin → Forms: local libraries can be renamed and deleted.** Open
  a local library for a "Rename library" control (e.g. fixing
  capitalization on an association name) or delete it — from the
  library page or the list — with a confirmation. Existing
  transactions keep their checklists and documents; the state
  (California) library is protected from both.
- **The Add-an-item picker is now managed from the app — no deploy
  needed for CAR catalog changes.** The picker reads your form
  library from the database: the full CAR catalog (~250 forms,
  including CLR — Cancellation of Lease or Rent) is loaded into
  **Admin → Forms → California** on first startup, where forms can be
  added, edited, deactivated, or deleted and the picker follows
  immediately. Deleted forms stay deleted across updates. Forms a
  brokerage adds at **Account → Forms** now appear in their picker
  too, and forms they hide disappear from it. Catalog forms start as
  picker-only — they never join default checklists unless an admin
  broadens a form's applicability on its Edit page. One heads-up for
  existing installs: the first startup after this update re-lists the
  complete catalog, so if you had deleted individual forms before,
  delete them once more — from then on it sticks.

## June 2026

### v0.3.4 — Fix: deleting a single form

Deleting an individual form from the admin forms library now works.
The Delete button was mis-wired (two buttons sharing a table cell
confused the browser, so the click hit the wrong action); it now
deletes exactly the form you clicked, after its own confirmation
prompt. Deactivate in the same row is unaffected.

### v0.3.3 — Fixes: live dashboard, team list, and member removal

- **Live dashboard updates work again.** The real-time stat cards
  depended on a script that was loading from a broken URL (a 404). The
  app now loads it from the correct location, so dashboard numbers
  update instantly as your team works.
- **Removing a teammate no longer orphans their deals.** When a broker
  removes someone from the team, that person's transactions are now
  **reassigned to the broker** instead of dropping into "Unassigned."
  The departing agent's name is kept on each transaction (shown as
  "Former agent") so you can still see who originally handled it.
- **Team list is cleaner.** Brokers are pinned to the top, everyone
  else is listed alphabetically, and the role dropdowns no longer get
  squeezed — the columns line up properly now.

### v0.3.2 — Full control over the forms library (admin)

Super-admins can now fully manage the forms library from
**Admin → Forms → (a library)**:

- **Delete any form** in any group, with a confirmation prompt. This
  removes it from the library permanently so it's never offered on new
  transactions again. (Existing transactions keep the copies they were
  created with — deleting a library form never touches documents in
  active deals.)
- **Deactivate / reactivate any form** — a reversible alternative to
  delete. A deactivated form stays in the library but stops appearing
  when new transactions are created; reactivate it anytime.
- **Rename any group** inline — the new name shows up on every
  transaction created afterward.
- **Delete a whole group**, with confirmation, which also removes every
  form inside it.

Per-form **edit** (name, order, required flag, and applicability) was
already available and is unchanged.

### v0.3.1 — Switched email provider to Postmark

We've moved transactional email — verification links, welcome notes,
team invites, price-change notifications, and trial-ending reminders —
from Resend to **Postmark**. There's no visible change in the messages
themselves; the switch is for deliverability headroom (Postmark's
sole focus is transactional, and their inbox-placement rates have been
consistently better in our testing).

**For self-hosted deployments**: the environment variables changed.
`RESEND_API_KEY` → `POSTMARK_SERVER_TOKEN`; `RESEND_FROM` →
`POSTMARK_FROM`; `RESEND_REPLY_TO` → `POSTMARK_REPLY_TO`; and a new
optional `POSTMARK_MESSAGE_STREAM` (defaults to `outbound`). See the
README for the full set.

### v0.3.0 — New pricing model with worked examples

We've introduced a **three-tier pricing model** built around a simple
principle: every plan includes **unlimited team members**. Most competitors
charge per-user, which punishes brokerages for putting their compliance
officers, transaction coordinators, and admins on the system. We don't.

- **Solo — $79/month.** 15 transactions/month included. Built for indie
  shops and new teams up to about 15 agents. Overage at $4 per transaction.
- **Brokerage — $249/month.** 75 transactions/month. The sweet spot for
  established California brokerages (15–50 agents). Adds custom form sets,
  per-agent compliance scoring, and chat support. Overage at $3.
- **Office — $599/month.** 300 transactions/month. Multi-office and
  franchise operations (50+ agents). Adds SSO, API access, identity-
  verified e-signatures, and dedicated onboarding. Overage at $2.

The public pricing page now shows **a "What would I actually pay?"
expandable on every plan card**, with worked examples at half-limit, at
the limit, and over the limit — so prospects can confirm the math
matches their actual transaction volume before signing up. Each card
also carries a one-line comparison to Dotloop, SkySlope, or BrokerMint
at the same volume so the cost gap is visible.

**Annual billing** now saves you **two full months** (17% off) instead
of the previous 15%.

Existing brokerages on a custom plan are unaffected — these defaults
only seed on a fresh install.

### v0.2.1 — Changelog in the admin area

Super-admins now have a **Changelog** page under `/admin/changelog`. It shows
the running build version prominently at the top and renders the full release
history below — same content as this file, just inside the app so you don't
have to leave the admin area to see what shipped when. The "Changelog" tab
is in the admin sub-navigation alongside Users, Brokerages, Tiers, Forms, and
Audit log.

### v0.2.0 — Real-time dashboard

The numbers at the top of your dashboard (Total, **Needs Attention**, Active,
Pending, Sold) now update **the instant** something changes — no more waiting
on a refresh. The moment a teammate approves a file, denies one, leaves a
comment, uploads a document, reassigns a transaction, or marks a deal sold,
your numbers shift in place without reloading the page.

This is a real **server push** (not polling): your browser keeps a quiet
connection open to the server and the server speaks up only when something
moves. If your role on the brokerage changes mid-session — or someone removes
you from the team — the connection closes immediately so you stop seeing data
your new role isn't allowed to see.

### Version number on every page

Every page now shows the build version in the bottom-right corner (small and
faded, so it doesn't get in the way). Include it when reporting an issue and
support can tell at a glance which build you were on.

### Smarter "Needs Attention" *(v0.1.0)*

Needs Attention now follows a clear **"ball in your court"** rule — at any
moment, a file is in **either** the agent's court **or** the compliance side's
court, never both. When someone takes an action, the ball moves to the other
side:

- Agent uploads a file → compliance is flagged.
- Compliance comments asking for a correction → agent is flagged, compliance
  is no longer flagged.
- Agent uploads the correction → compliance is flagged again.
- Compliance approves the file → nobody is flagged for that file anymore.

A few specific clean-ups:

- **General transaction comments no longer trigger Needs Attention.** Those
  are your own notes — they shouldn't badge you.
- **Approving a file clears its flag** for both sides immediately.
- **When every file on a transaction is approved**, the transaction goes
  quiet for everyone, no matter how many comments are added afterward.
- **Closed deals (Sold / Canceled / Withdrawn)** still show up in Needs
  Attention if a teammate uploads or comments on them — useful for catching
  late activity on a "done" file.

### Checklist groups behave differently per role

- **Agents**: every group is expanded when you open a transaction, so your
  full checklist is visible at a glance.
- **Compliance & broker**: only groups that **need your attention** open
  automatically — typically the files an agent just uploaded for review.

### Your collapse picks now stick

If you collapse a group while looking for the next category to upload into,
that group stays collapsed across uploads. Earlier the page kept springing
back to its defaults; now it remembers what you closed.

The one thing that won't stay hidden: a group flagged for your attention. If
new activity arrives that needs your eyes, that group will reopen even if you
collapsed it earlier in the session.

### Larger, easier-to-see expand triangles

The little arrows on each checklist group are bigger and dark green now —
much easier to spot at a glance.

### Single-click Deny

Click **Deny** and the file is denied immediately. A small box pops up so you
can leave a reason if you want; the button label switches between **"No
comment"** and **"Save comment"** as you type. The reason (if you write one)
gets posted into the file's comment thread so the agent sees exactly what to
fix.

### Re-uploads automatically un-deny

When an agent uploads a corrected file to replace one that was denied, the
file flips back to "Pending review" on its own. Compliance no longer has to
manually un-deny anything to see the new version.

---

## May 2026

### Account & team management

- **Remove a teammate without deleting their account.** Brokers can remove
  agents from the team — the agent's account stays, they just lose access
  until they're re-invited. Any transactions they owned stay on the team's
  dashboard and the broker can reassign them.
- **No more duplicate invites.** Inviting someone who already has a pending
  invite at your brokerage is now a no-op with a friendly notice telling you
  to use **Resend email** instead. Same goes for inviting someone who's
  already at another brokerage.
- **Case-insensitive emails.** `Alice@Example.com` and `alice@example.com`
  are now treated as the same person across login, signup, and invites.
- **Friendly "no brokerage" landing.** If your account is between
  brokerages (just got removed, or your brokerage closed), signing in takes
  you to a clear page that lists any pending invitations you can accept or
  decline.
- **Decline an invitation.** You can now decline an invite directly from the
  no-brokerage page instead of just ignoring it.

### Transaction management

- **Unassigned transactions view.** Brokers get a new page showing every
  transaction in the brokerage that has no owning agent (typical after
  removing someone). Tick the boxes, pick an assignee, hit Reassign.
- **Reassign any transaction.** A broker can move a transaction from one
  agent to another at any time.
- **Address OR APN** is now required when creating a transaction. Land deals
  without a street address work — just enter an APN.

### Compliance forms

- **Restrict forms to specific deal types.** When adding a form to the
  library (admin) or as a custom form (broker), checkboxes let you scope the
  form to specific transaction types (Residential, Commercial, etc.), sides
  (Listing / Purchase), or sales conditions (Standard, Probate, Short Sale,
  **REO / Foreclosure**). The form will only appear on transactions matching
  those criteria.
- **Drag-and-drop reordering.** Admins can drag form groups (and the forms
  inside them) into the order they prefer instead of typing sort numbers.
- **Edit existing library forms.** Admins now have a per-form edit page to
  tweak name, order, required flag, and applicability without recreating
  the form.

### Small UX polish

- **"Just now"** replaces the awkward "now ago" on freshly-created records.
  Future-dated records correctly say "in 5 minutes" rather than "5 minutes
  ago".
- **Safer confirmation dialogs.** The "Are you sure?" prompts that show
  member names, brokerage names, or filenames are now safe regardless of
  what those values contain.

---

## Licensing

TransactVault is now under a **proprietary license**. The source code is
public on GitHub for transparency and security review, but is not
open-source. See [LICENSE.md](./LICENSE.md) for full terms.
