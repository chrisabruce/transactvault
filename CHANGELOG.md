# What's new

A plain-English summary of recent updates. For technical release notes, see
the commit history. The version number for the build you're using appears at
the bottom-right of every page.

---

## August 2026

### v0.8.3: Tidier comparison points

- **The homepage comparison bullets keep their shape.** In the "Better
  than SkySlope" strip, a point that ran onto a second line used to
  start that line all the way back under the check mark. Wrapped lines
  now continue neatly under the words instead.

### v0.8.2: The homepage now sounds like us

- **Plainer talk about why brokerages switch.** We rewrote the "Why
  brokerages are switching" cards so they read the way we actually
  speak: fewer buzzwords, more specifics, and the Fort Knox joke
  survived editing. The facts underneath didn't change.

### v0.8.1: Updates now look right immediately

- **No more "half-updated" pages.** After we shipped an update, your
  browser could keep using the previous visual styling for a while,
  which made new pages look plain or oddly arranged. The new homepage,
  for example, could appear without its speech bubbles. Every release
  now tells your browser to fetch the latest styling automatically.
  (If a page still looks plain right now, refresh it once while
  holding Shift and you're set.)

### v0.8.0: A clearer front door, with familiar faces

- **Real voices on the homepage.** Six of our earliest partners across
  the Antelope Valley, from Century 21 Doug Anderson, The Real Estate
  Place, and LeFebvre's East Valley Real Estate, now share how
  TransactVault changed their workflow, photos and all, in a fresh
  speech-bubble design. Thank you Glenda, Kirk, Maria, Amanda,
  Christie, and Jason.

- **Plainer words about what we do.** The homepage now says "real
  estate transaction management" right in the headline, spells out the
  full list of reasons brokerages switch (now eight, including how
  simple the site is to navigate and how your documents are protected),
  and gives the SkySlope and Dotloop comparisons more detail.

- **Better link previews.** Pasting transactvault.app into a text
  message or social post now shows a proper branded preview card
  instead of a blank space.

- **Small fixes.** A formatting glitch in the footer is gone, and our
  mailing address is now listed there. Behind the scenes, we also added
  tooling that makes rolling out updates to you smoother.

## July 2026

### v0.7.0: Whole-brokerage exports that actually finish

- **Export everything, no matter how big.** The old "Download brokerage
  ZIP" button built one giant file while you waited, and anything past
  400 MB was refused outright. There's now an Export center (Team →
  Brokerage archive → Open export center): click Start and the archive
  is built in the background — close the tab, keep working, and you'll
  get an email when it's ready. No size limit.

- **Archives arrive as tidy pieces instead of one monster file.** Each
  agent's transactions are packaged per year (busy years split by
  month), so even a very large brokerage becomes a set of reasonable
  downloads. Inside, files are organized by property and form code with
  a manifest listing exactly what's included — the same layout auditors
  see on the per-transaction export.

- **Interrupted downloads pick up where they left off.** Archives are
  served straight from secure storage rather than through the app, so a
  dropped connection resumes instead of starting over — the difference
  between "possible" and "hopeless" for multi-gigabyte files on
  ordinary internet. Grab archives one at a time, use "Download all",
  or take the download link list if you prefer a download manager.

- **You stay in control.** Watch the build progress live, cancel it
  mid-way, or delete finished archives early. Downloads stay available
  for 7 days and are then cleaned up automatically, and every export
  and download is recorded in the audit log.

### v0.6.1: Comments are conversation, not alarms

- **Leaving a comment no longer flags the transaction for review.**
  Previously every comment moved the deal onto the other person's
  "Needs attention" list — whoever spoke last put the ball in the
  other side's court, so a simple question or an FYI lit up dashboards
  across the brokerage. Now only real review events raise the flag: a
  denied item stays with the agent until it's fixed, and an uploaded
  document stays in the reviewer's queue until it's actually approved
  or denied. That last part also fixes a quirk where replying to an
  upload cleared it from your own review queue even though you hadn't
  reviewed it yet.

### v0.6.0: A faster, clearer upload experience

- **Large uploads no longer stall at a frozen percentage.** Two hidden
  ceilings were killing them: every request had to finish within 60
  seconds — not nearly enough to move a big file over an ordinary
  connection — and anything past 100 MB hit a wall the browser never
  explained, so the progress bar just stopped. Uploads now get the
  time they actually need, and the limits below are told to you up
  front instead of discovered mid-transfer.

- **Files now travel straight from your browser to secure storage.**
  Previously every document streamed through our server on its way to
  storage — the same bytes moved twice, and the progress bar could
  only guess at the second leg. The browser now asks the server for
  permission, sends the file directly to the storage provider, and the
  server verifies the result before adding it to your transaction. The
  percentage you watch is the real transfer. If anything about the
  direct route fails, the upload quietly falls back to the old path —
  slower, but it always works.

- **The 100 MB limit is now stated, not sprung on you.** Pick a file
  that's too big and you're told immediately — with the file's actual
  size — before any waiting. The limit is also enforced by the storage
  system itself, not just our code: an upload that doesn't match what
  was approved is refused outright.

- **Only transaction-relevant file types are accepted.** PDF, Word,
  Excel, CSV, TXT, RTF, and the image formats phones and scanners
  produce (JPG, PNG, GIF, TIFF, WEBP, HEIC). Anything else — archives,
  web pages, executables — is refused with a message naming what IS
  allowed. Web-page formats are excluded deliberately: they can carry
  scripts into the document preview.

### v0.5.5: Trial banner is broker-only

- **Invited teammates no longer see the green subscription bar.** The
  "pick a plan" nudge and the free-trial countdown are about billing,
  and only the broker can act on billing — but the bar was showing for
  everyone in the brokerage, so agents and compliance officers were
  greeted with a countdown they could do nothing about. The green bars
  now appear only for the broker. Red and amber notices — a failed
  payment, a scheduled cancellation, or an account in read-only
  wind-down — still show for everyone, because they explain why saving
  a change is being refused.

### v0.5.3: Subscription banner and a Stripe re-link button

- **The "pick a plan" bar no longer lingers after you subscribe.**
  Entering a card sent you straight back to the dashboard, but the app
  only learned about the new subscription from a separate message Stripe
  sends behind the scenes — which usually arrives a moment *after* your
  browser does. So the first page you saw still invited you to pick a
  plan, which reads as the payment not having worked. Stripe now returns
  you through a step that checks your subscription first, so the bar is
  already correct on the page you land on. (While you're in the free
  trial the bar stays, showing the countdown and days remaining — that's
  deliberate.)

- **"Clear all errors" in Admin → Errors.** The error screen is a
  diagnostic scratchpad that otherwise only empties itself after 30 days,
  so a noisy spell — a webhook retrying every few minutes, say — buries
  whatever fails next. Super-admins can now wipe it in one click. It asks
  first, removes every entry regardless of the status filter on screen,
  and cannot be undone. The clearing is itself written to the audit log
  (who, when, how many), so it can't be used to quietly erase a trail.

- **Rejected Stripe messages now explain themselves.** When Stripe sent
  us a billing update we refused, the admin error screen only showed "no
  detail", so the most common cause — the signing secret not matching the
  endpoint, which is exactly what happens when you switch Stripe from test
  to live — was invisible unless you read the server logs. Rejections now
  record the reason, and name the setting to check.

- **New "Re-link to Stripe" button in Admin → Tiers.** Stripe keeps test
  and live data completely separate, so switching from test keys to live
  leaves every plan pointing at products that the live account cannot
  see, and Subscribe fails with "No such price" for all of them. Nothing
  fixed that automatically: the startup setup skips whenever plans
  already exist, and re-saving a plan only talks to Stripe if you changed
  something, using the old reference. The button creates fresh products
  and prices in whichever Stripe account the current keys belong to and
  repoints every plan at them. Super-admins only, it asks for
  confirmation first, and existing subscriptions are unaffected.

### v0.5.2: Signup fixes and a deploy fix

- **Creating an account is no longer a waiting game.** The signup form
  runs a small puzzle in your browser to prove you're not a bot, and the
  "Create account" button stays disabled until it finishes. That puzzle
  was pausing for a timer after every 200 attempts, so its speed was set
  by how often the browser felt like waking it up rather than by how fast
  your computer can work — on a slow or backgrounded tab it could take
  minutes, with no way to tell whether it was stuck. It now works in
  batches without the pauses: measured at over a minute before the change
  and about half a second after, on the same machine. If it ever does
  fail, the page now says so instead of leaving the button dead forever.

- **The cursor no longer jumps out of "Brokerage name".** Typing the
  first character could throw you back to the password box. Those last
  two fields didn't tell the browser what they were, so its autofill had
  to guess — inside a form that also contains a new-password field, which
  is what prompts a password manager to grab the cursor. Both fields now
  identify themselves.

- **Fixed a failing production build.** Deploying from the Dockerfile
  stopped with "couldn't read CHANGELOG.md". This page is compiled into
  the application (that's how the in-app version of it works), so it has
  to be present when the app is built — and the build recipe wasn't
  including it. It is now, with a check that fails immediately if any
  similar file is ever left out again.

- **Dev and production setups are now cleanly separated.** The Docker
  Compose file is development-only and works again on a fresh machine
  (it referenced a network that only exists on the server, and didn't
  publish the ports its own instructions described). Production runs the
  application alone and connects to a database and file storage you
  manage separately, so either can be moved, clustered or swapped for a
  hosted service without touching the app. Setup instructions live in
  DEPLOY.md.

### v0.5.1: Fixes for the v0.5.0 hardening

The browser-hardening rules added in v0.5.0 turned out to be too strict
in three places, each of which quietly broke something. Those are fixed,
along with two older bugs found while chasing them:

- **Choosing a profile photo did nothing.** The crop window never
  opened, because the new rules stopped the browser displaying the file
  you had just picked — and the cropper only opens once that image has
  loaded. (The cropper's styling was missing for the same reason.)
- **PDF previews showed nothing.** Document previews open in a frame
  inside the page, and the new rules told browsers to refuse being
  framed — including by us. Previews may now be framed by TransactVault
  itself and nowhere else.
- **A safety measure on previews was being discarded.** The preview
  screen sets its own, stricter rules for image files, and the
  site-wide rules were overwriting them. Since a file's type is
  declared by whoever uploads it, that protection matters: it is what
  stops a booby-trapped image running code as you if opened directly.
  Page-specific rules now take precedence over the site-wide default.
- **Changing your profile photo appeared to revert to the old one.**
  Your photo lives at a web address that never changes, and browsers
  were told they could reuse it for a minute without checking, so the
  refresh right after saving re-displayed the picture you had just
  replaced — and trying again looked like it kept snapping back to the
  first one. Your new photo was uploading correctly the whole time; you
  were looking at a cached copy. Browsers now check for a newer version
  each time, and that check is answered without re-sending the image
  when nothing has changed, so pages full of avatars stay fast. (Also
  fixed: after cropping a second photo, the small preview beside the
  button stopped updating.) This one predates v0.5.0.
- **A validation error on the transaction form no longer wipes it.**
  Saving a new transaction with neither a property address nor an APN
  returned a plain error page and threw away everything else you had
  entered, so one blank field meant retyping a dozen. The message now
  appears on the form itself with your entries — including the
  dropdown selections — still in place. The same applies when editing
  an existing transaction, and when a plan limit blocks the save.

### v0.5.0: Security hardening, password reset, and the 500s fix

- **Fixed the intermittent "something went wrong" errors (500s).**
  Root cause: SurrealDB's v3 driver changed what cloning the client
  means — every clone opens its own database session, registered in
  the background — and the app was unknowingly creating one per
  request, occasionally racing its own session setup ("Session not
  found"). The app now shares a single session for its lifetime,
  which is the driver's documented recommendation, and a background
  heartbeat verifies the database link every 45 seconds so any
  degradation is detected and healed between requests, with a log
  trail.

- **Password reset.** There was no way to recover a forgotten password
  — a user had to be deleted and re-invited, which detached them from
  their transactions. There's now a "Forgot your password?" link on the
  sign-in page: enter your email, get a link, choose a new password.
  Links last one hour and work once. Completing a reset signs you out
  everywhere else, so it doubles as the recovery path if you think
  someone else has access to your account. The request page says the
  same thing whether or not an account exists, so it can't be used to
  discover who has one.

- **Large exports no longer strain the server.** ZIP exports used to be
  assembled entirely in memory — every document, plus the finished
  archive, plus a copy held for the download — so a big brokerage
  export could consume well over a gigabyte and risk taking the site
  down. Documents now stream from storage through the compressor and
  out to your browser, so memory stays flat no matter how large the
  archive is. Verified with a 114 MB export: server memory didn't rise
  at all.

**Security hardening.** A full security audit was performed on this
release and every finding is fixed. Nothing below was known to have
been exploited; several were serious enough to fix before shipping.

- **Two exploitable flaws closed.** A crafted link to the transactions
  page could run arbitrary code in the browser of whoever clicked it,
  acting as that user; and a hand-built upload request could attach a
  document to — and clear the review status of — a checklist item in
  *another* brokerage. Both are fixed and covered by tests.
- **Sessions can now be revoked.** Signing out, and changing your
  password, immediately invalidate every other session for your
  account. Previously a session that had been copied stayed usable
  until it expired on its own.
- **Session cookies are marked `Secure`** on any HTTPS deployment, so
  they can't leak over an unencrypted connection.
- **Cross-site request forgery is blocked** for every action that
  changes data, including from sibling subdomains.
- **Browser hardening headers** are now sent on every page
  (anti-clickjacking, content policy, referrer policy, and HSTS on
  HTTPS).
- **Rate limits actually hold.** They keyed off a header a caller
  could forge, which made the login and signup limits bypassable;
  IP detection now trusts only the reverse proxy. Password changes and
  team invitations are rate-limited too.
- **Invitations expire** after 14 days — the invite email always said
  they did, and now that's true.
- **Exports are bounded.** A single-transaction export had no size
  limit, so a large one could exhaust server memory and take the site
  down for everyone.
- **Smaller fixes:** login no longer reveals whether an email is
  registered (via response timing); the public health endpoint no
  longer discloses build and host details; document downloads carry
  stricter headers; the error log no longer stores search terms; and
  the app refuses to start with a default/weak `JWT_SECRET`, or with
  the destructive `DEV_RESET_ON_BOOT` flag set on an HTTPS deployment.

A second audit pass went back over the fixes above, looking specifically
for text that reaches somewhere it isn't escaped. It found no way to
inject anything into a page, and these further issues:

- **Rate limits could be wiped by flooding them.** Past a ceiling, the
  limiter used to reset itself — handing every account a fresh
  allowance — and the "forgot password" form let anyone fill it up on
  demand. In effect the brute-force protection on sign-in could be
  switched off from a public form. The limiter now discards its
  least-restricted entries instead of resetting, so an exhausted limit
  is the last thing to go.
- **Verification links no longer sign you in.** Clicking one confirmed
  your address *and* started a session, which meant a link sent to you
  by someone else could quietly sign you into **their** account — and
  anything you filed next would land in their brokerage. Verifying now
  takes you to the sign-in page.
- **Signing out always ends every session.** For a user not currently
  attached to a brokerage — someone just removed, or waiting to accept
  an invitation — signing out cleared the browser but left the session
  itself alive, and it regained full access as soon as they joined a
  brokerage.
- **Reset and invitation links are no longer written to logs.** These
  links carry a single-use secret in the address, and every request
  address was being recorded — including into the admin error screen,
  which keeps 30 days of history. A live password-reset link could sit
  there, readable and usable.
- **Names and addresses can't forge log or export entries.** Text
  fields flow into server logs and into the `MANIFEST.txt` inside an
  export, both of which are line-based. A deliberately placed line
  break let someone add convincing-looking lines of their own — a fake
  document count in a compliance export, or a fabricated log entry.
  Invisible and direction-reversing characters are now refused on the
  way in; a filename using one to disguise its file type (showing
  `.jpg` while actually being `.exe`) no longer survives into an
  export.
- **Exports no longer drop look-alike files.** Two documents whose
  names differed only by punctuation could end up sharing a name inside
  the ZIP, and most unzip tools keep only the last — so an export could
  quietly contain fewer documents than its own manifest listed.
  Duplicates now get numbered.
- **Error and timeout pages carry the security headers too.** They were
  the only responses served without them.
- **A neighbouring domain could pass the cross-site check.** The
  fallback check for non-browser clients compared addresses loosely
  enough that a lookalike domain (`…vault.co` against `…vault.com`)
  was accepted. It's now an exact match.
- **The "forgot password" page no longer reveals who has an account.**
  It always showed the same message, but took noticeably longer when
  the address existed, because it waited for the email to send. It
  no longer waits.
- **Third-party scripts and styles are locked to a known version.**
  The two files loaded from a public CDN are now checked against a
  fingerprint, so a change at the CDN can't alter what runs in your
  browser.

### v0.4.0: Live search, real-time fixes, team exports, and the full CAR catalog

- **New "Referral" transaction type.** Referral-fee deals no longer
  have to masquerade as Residential with a 60-item property checklist.
  Per the printed data sheet, the checklist is a single **Referral
  Contract** section holding the required RFA — Referral Fee
  Agreement, the same regardless of which side the client was
  referred from. Admins can put more forms on referrals via the new
  Referral checkbox in every form's applicability picker.
- **Lease transactions get the real lease checklist.** Rental / Lease
  and Commercial Lease deals previously received the residential or
  commercial *sale* checklists. They now follow the printed Commercial
  Lease + Rental/Lease data sheet: MLS sheets (including the new
  RNTD — Rented Status report), the **Lease Listing Contract** (LL,
  landlord side) and **Rental Contract** (RLMM) sections, mandatory AD
  and LCA (WFDA on the tenant side only), the full
  "Disclosures — If Applicable" list, a required **Application,
  Receipts & Reports** section (CCR, LRA, SDR), **Governing
  Documents** (CC&Rs, HOA docs, R&R — Rules & Regulations), and CLR
  under Release Disclosures. AD, RNTD, CCR, SDR, and R&R are now
  library forms — brokerages that added them as custom forms should
  delete their copies (Account → Forms) to avoid doubled checklist
  lines. Existing databases update themselves on first startup: lease
  applicability is recomputed, so lease deals stop pulling sale forms
  without touching any admin edits, custom forms, or live
  transactions.

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
- **The transaction list itself updates live too — not just the stat
  cards.** When anything changes in your brokerage (a transaction
  created or deleted, a status change, an approval), the visible rows
  quietly re-fetch and update in place, respecting whatever you've
  typed in the search box and your current filters. If you've
  scrolled deep into a long list, the refresh politely waits rather
  than yanking you back to the top. This works across roles — an
  agent's window follows changes a broker makes, scoped to what the
  agent is allowed to see.
- **Readable production logs.** Human-readable log output
  (`PRETTY_LOGS=true`) is now the compose default and no longer
  spews ANSI color codes when running in a container — Dokploy's log
  viewer shows clean text. JSON output remains available for log
  aggregators via `PRETTY_LOGS=false`.
- **Forms keep what you typed when something's wrong.** A validation
  error on the signup form used to wipe every field; now your name,
  email, brokerage, and city stay put and you only re-enter the
  password. Same treatment for the login form (email stays) and the
  invitation-accept form (name stays — and its errors now show on the
  invite page itself instead of a bare error screen). Passwords are
  never carried back into the page.
- **New Admin → Errors screen.** Server errors (5xx) and meaningful
  request errors (400/409/422) are now captured to the database with
  their full internal error detail, the request, the signed-in user,
  and the IP — so production failures can be diagnosed from the admin
  panel instead of shelling into the host for logs. Deliberately NOT
  recorded: 404/401/403, so vulnerability-scanner noise (wp-admin
  probes and friends) can't flood it. Rows are kept for 30 days, and
  capturing is fully detached from request handling — a struggling
  database can't make error handling itself fail.
- **Signed-out visitors land on the login page, not an error.**
  Opening an app link without a session — incognito window, new
  device, expired login — used to show a bare "401 — Please sign in
  to continue" error page. Browser navigations to any app or admin
  page now redirect straight to the login screen.
- **Long-lived tabs no longer go quietly deaf.** Previously, if the
  server was unreachable for a couple of minutes (a deploy, a
  restart), an open tab would stop retrying its live connection and
  never update again until you reloaded. The page now detects the
  give-up and reopens the connection itself, indefinitely.
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

### v0.3.4: Fix: deleting a single form

Deleting an individual form from the admin forms library now works.
The Delete button was mis-wired (two buttons sharing a table cell
confused the browser, so the click hit the wrong action); it now
deletes exactly the form you clicked, after its own confirmation
prompt. Deactivate in the same row is unaffected.

### v0.3.3: Fixes: live dashboard, team list, and member removal

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

### v0.3.2: Full control over the forms library (admin)

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

### v0.3.1: Switched email provider to Postmark

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

### v0.3.0: New pricing model with worked examples

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

### v0.2.1: Changelog in the admin area

Super-admins now have a **Changelog** page under `/admin/changelog`. It shows
the running build version prominently at the top and renders the full release
history below — same content as this file, just inside the app so you don't
have to leave the admin area to see what shipped when. The "Changelog" tab
is in the admin sub-navigation alongside Users, Brokerages, Tiers, Forms, and
Audit log.

### v0.2.0: Real-time dashboard

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
