# What's new

A plain-English summary of recent updates. For technical release notes, see
the commit history. The version number for the build you're using appears at
the bottom-right of every page.

---

## June 2026

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
