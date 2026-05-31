-- ============================================================================
-- TransactVault schema (CAR-forms revision)
--
-- Graph-first design:
--   (user)-[works_at]->(brokerage)
--   (user)-[owns]->(transaction)
--   (brokerage)-[has_transaction]->(transaction)
--   (transaction)-[has_item]->(checklist_item)
--   (transaction)-[has_document]->(document)
--   (checklist_item)-[has_document]->(document)         (per-item link)
--   (user)-[uploaded]->(document)
--   (document)-[version_of]->(document)                  (version history)
--
-- All tables SCHEMAFULL. RecordId is the canonical key type — never parse,
-- concatenate, or interpolate record IDs as strings.
--
-- Every DEFINE uses OVERWRITE so re-running this script in place updates
-- existing definitions (ASSERT clauses, defaults, types, etc.) instead of
-- being ignored. The boot loop in `src/db/mod.rs` runs this on every start.
--
-- This is a single-pass, fresh-load schema: the app is pre-production, so
-- there is no legacy data to migrate. Each table is DEFINED once with its
-- final types/defaults/ASSERTs. To wipe and reload, boot with
-- `DEV_RESET_ON_BOOT=yes-destroy-all-data` (drops every table, then this
-- script recreates them empty). If/when real data exists, reintroduce a
-- relax → backfill → lock migration pass before tightening any field.
-- ============================================================================

-- ----------------------------------------------------------------------------
-- Brokerage
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE brokerage SCHEMAFULL;
DEFINE FIELD OVERWRITE name       ON brokerage TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE city       ON brokerage TYPE option<string>;
DEFINE FIELD OVERWRITE state      ON brokerage TYPE string DEFAULT "CA";
-- `plan` carries the tier slug the brokerage is subscribed to (e.g.
-- "starter", "pro"). Tiers are admin-managed now, so the old strict
-- ASSERT is gone — validation lives at the app layer against the
-- `tier` table.
DEFINE FIELD OVERWRITE plan       ON brokerage TYPE string DEFAULT "trial";
-- Stripe wiring. Set at first subscribe; persists across re-subs so
-- we keep one Customer object per brokerage and Stripe's invoice
-- history stays continuous.
DEFINE FIELD OVERWRITE stripe_customer_id     ON brokerage TYPE option<string>;
DEFINE FIELD OVERWRITE stripe_subscription_id ON brokerage TYPE option<string>;
-- Mirror of the Stripe subscription state. Source of truth is Stripe;
-- webhooks keep this current.
--   trialing      — inside the 14-day free trial, no card charge yet
--   active        — paid, in good standing
--   past_due      — payment failed; Stripe will retry
--   canceling     — user clicked cancel, still inside the paid window
--                   (`cancel_at` set); read/write still allowed
--   wind_down     — paid window ended; read-only 60-day grace before
--                   admin-driven purge; `wind_down_purge_at` set
--   none          — never subscribed
DEFINE FIELD OVERWRITE subscription_status    ON brokerage TYPE option<string>;
DEFINE FIELD OVERWRITE current_period_end     ON brokerage TYPE option<datetime>;
DEFINE FIELD OVERWRITE cancel_at              ON brokerage TYPE option<datetime>;
DEFINE FIELD OVERWRITE wind_down_purge_at     ON brokerage TYPE option<datetime>;
-- Super-admin override. When true, the brokerage skips every billing
-- gate: no Stripe Subscribe is required, transaction/user limits are
-- ignored, and the read-only wind_down gate doesn't apply. Toggled
-- from /admin/brokerages by a super-admin; audited as
-- `brokerage_comp_granted` / `brokerage_comp_revoked`.
DEFINE FIELD OVERWRITE is_complimentary       ON brokerage TYPE bool DEFAULT false;

-- ----------------------------------------------------------------------------
-- Tier — pricing plan definitions managed from the super-admin UI.
-- Each tier mirrors a Stripe Product + Price pair; the slug is the
-- stable identifier referenced from `brokerage.plan`. Price changes
-- create a new Stripe Price (Prices are immutable); the old one stays
-- attached to existing subscribers until they next renew.
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE tier SCHEMAFULL;
DEFINE FIELD OVERWRITE slug                    ON tier TYPE string
    ASSERT string::len($value) >= 1 AND $value = string::lowercase($value);
DEFINE FIELD OVERWRITE name                    ON tier TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE description             ON tier TYPE string DEFAULT "";
DEFINE FIELD OVERWRITE feature_bullets         ON tier TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE price_cents             ON tier TYPE int DEFAULT 0 ASSERT $value >= 0;
DEFINE FIELD OVERWRITE stripe_product_id       ON tier TYPE option<string>;
DEFINE FIELD OVERWRITE stripe_price_id         ON tier TYPE option<string>;
DEFINE FIELD OVERWRITE stripe_overage_price_id ON tier TYPE option<string>;
-- `-1` means unlimited; `0` and positive ints are real caps. Limits
-- reset monthly with the billing period (transactions); user_limit
-- is a steady-state cap.
DEFINE FIELD OVERWRITE user_limit              ON tier TYPE int DEFAULT -1;
DEFINE FIELD OVERWRITE transaction_limit       ON tier TYPE int DEFAULT -1;
-- `None` means hard-block at the transaction limit; `Some(n)` means
-- allow over and bill `n` cents per overage transaction via Stripe
-- metered usage.
DEFINE FIELD OVERWRITE overage_fee_cents_per_tx ON tier TYPE option<int>
    ASSERT $value = NONE OR $value >= 0;
DEFINE FIELD OVERWRITE is_active               ON tier TYPE bool DEFAULT true;
DEFINE FIELD OVERWRITE is_archived             ON tier TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE sort_order              ON tier TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE created_at              ON tier TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at              ON tier TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE INDEX OVERWRITE idx_tier_slug   ON tier FIELDS slug UNIQUE;
DEFINE INDEX OVERWRITE idx_tier_active ON tier FIELDS is_active, is_archived;

-- ----------------------------------------------------------------------------
-- Forms engine — admin-managed CA/local form library + per-brokerage config.
--
-- Graph-modeled so a transaction's checklist resolves in one traversal:
--   brokerage ->(uses_state, uses_locality)-> form_set
--             -> has_group -> form_group -> has_form -> form
-- minus  brokerage -> hides_form -> form
-- plus   brokerage -> owns_form  -> form   (custom)
--
-- `form_set.scope` is "state" (e.g. California) or "local" (a city /
-- municipality / board). Localities hang off their state via
-- `has_locality`. Groups + forms carry their own sort order so the UI
-- presents them grouped + ordered. `form.applies_*` are the dependency
-- rules (which transaction type / side / special condition includes the
-- form); empty array = applies to all.
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE form_set SCHEMAFULL;
DEFINE FIELD OVERWRITE scope      ON form_set TYPE string ASSERT $value IN ["state", "local"];
DEFINE FIELD OVERWRITE name       ON form_set TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE is_active  ON form_set TYPE bool DEFAULT true;
DEFINE FIELD OVERWRITE created_at ON form_set TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_form_set_scope ON form_set FIELDS scope;

DEFINE TABLE OVERWRITE form_group SCHEMAFULL;
DEFINE FIELD OVERWRITE name       ON form_group TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE sort_order ON form_group TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE created_at ON form_group TYPE datetime DEFAULT time::now();

DEFINE TABLE OVERWRITE form SCHEMAFULL;
DEFINE FIELD OVERWRITE code               ON form TYPE string;
DEFINE FIELD OVERWRITE name               ON form TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE description         ON form TYPE string DEFAULT "";
DEFINE FIELD OVERWRITE includes            ON form TYPE string DEFAULT "";
DEFINE FIELD OVERWRITE form_order          ON form TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE required            ON form TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE allows_multiple     ON form TYPE bool DEFAULT false;
-- Custom (brokerage-owned) forms carry their group inline rather than
-- via a `has_form` edge from a set's group, since they don't belong to
-- a shared form_set. Master/library forms leave these NONE and get
-- their group through the `form_group -> has_form -> form` edge.
DEFINE FIELD OVERWRITE group_name          ON form TYPE option<string>;
DEFINE FIELD OVERWRITE group_order         ON form TYPE option<int>;
-- Dependency rules. Empty array = applies regardless of that dimension.
DEFINE FIELD OVERWRITE applies_types       ON form TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE applies_sides       ON form TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE applies_conditions  ON form TYPE array<string> DEFAULT [];
DEFINE FIELD OVERWRITE is_active           ON form TYPE bool DEFAULT true;
DEFINE FIELD OVERWRITE created_at          ON form TYPE datetime DEFAULT time::now();

-- Form-engine graph edges.
DEFINE TABLE OVERWRITE has_locality  SCHEMAFULL TYPE RELATION FROM form_set   TO form_set;
DEFINE TABLE OVERWRITE has_group     SCHEMAFULL TYPE RELATION FROM form_set   TO form_group;
DEFINE TABLE OVERWRITE has_form      SCHEMAFULL TYPE RELATION FROM form_group TO form;
DEFINE TABLE OVERWRITE uses_state    SCHEMAFULL TYPE RELATION FROM brokerage  TO form_set;
DEFINE TABLE OVERWRITE uses_locality SCHEMAFULL TYPE RELATION FROM brokerage  TO form_set;
DEFINE TABLE OVERWRITE hides_form    SCHEMAFULL TYPE RELATION FROM brokerage  TO form;
DEFINE TABLE OVERWRITE owns_form     SCHEMAFULL TYPE RELATION FROM brokerage  TO form;

DEFINE FIELD OVERWRITE created_at ON brokerage TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON brokerage TYPE datetime DEFAULT time::now() VALUE time::now();

-- ----------------------------------------------------------------------------
-- User
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE user SCHEMAFULL;
-- Lowercased + trimmed at the schema level so equality lookups
-- (login, "is this user already registered?", invite-recipient match)
-- don't have to remember to normalize. The application layer also
-- lowercases for clarity; this is defense in depth.
DEFINE FIELD OVERWRITE email         ON user TYPE string
    VALUE string::lowercase(string::trim($value))
    ASSERT string::is_email($value);
DEFINE FIELD OVERWRITE name          ON user TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE password_hash ON user TYPE string;
-- Object-storage key for the user's avatar PNG. Served from
-- /app/users/{key}/avatar; absent means "no avatar uploaded yet".
DEFINE FIELD OVERWRITE avatar_storage_key ON user TYPE option<string>;

-- Email verification — accounts start unverified; the welcome email is only
-- sent AFTER the user clicks the verification link, proving they own the
-- inbox. The verify-token is cleared on success.
DEFINE FIELD OVERWRITE email_verified       ON user TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE verification_token   ON user TYPE option<string>;
DEFINE FIELD OVERWRITE verification_expires ON user TYPE option<datetime>;

-- Forensics: who signed up, from where. Helpful for spotting bulk abuse.
DEFINE FIELD OVERWRITE signup_ip         ON user TYPE option<string>;
DEFINE FIELD OVERWRITE signup_user_agent ON user TYPE option<string>;
DEFINE FIELD OVERWRITE last_login_at     ON user TYPE option<datetime>;

DEFINE FIELD OVERWRITE created_at ON user TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON user TYPE datetime DEFAULT time::now() VALUE time::now();
DEFINE INDEX OVERWRITE idx_user_email             ON user FIELDS email UNIQUE;
DEFINE INDEX OVERWRITE idx_user_verification_token ON user FIELDS verification_token;

DEFINE TABLE OVERWRITE works_at SCHEMAFULL TYPE RELATION FROM user TO brokerage;
DEFINE FIELD OVERWRITE role      ON works_at TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD OVERWRITE joined_at ON works_at TYPE datetime DEFAULT time::now();

-- ----------------------------------------------------------------------------
-- Invitation
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE invitation SCHEMAFULL;
-- Same normalization rule as `user.email` — schema-enforced so a
-- broker pasting `Alice@Example.com` and `alice@example.com` produces
-- one invitation row, not two.
DEFINE FIELD OVERWRITE email      ON invitation TYPE string
    VALUE string::lowercase(string::trim($value))
    ASSERT string::is_email($value);
DEFINE FIELD OVERWRITE role       ON invitation TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD OVERWRITE token      ON invitation TYPE string ASSERT string::len($value) >= 16;
DEFINE FIELD OVERWRITE brokerage  ON invitation TYPE record<brokerage>;
DEFINE FIELD OVERWRITE invited_by ON invitation TYPE record<user>;
DEFINE FIELD OVERWRITE accepted   ON invitation TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE declined   ON invitation TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE created_at ON invitation TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_invitation_token ON invitation FIELDS token UNIQUE;

-- Defense-in-depth against duplicate-pending invitations. The
-- application's `members::invite` handler dedupes against existing
-- pending rows before calling `create_invitation`, but if a future
-- caller bypasses that check (or two requests race past it) this
-- event will reject the second CREATE at the DB layer. Accepted /
-- declined rows stay for history — they only block when *another*
-- row is still pending for the same (brokerage, email).
DEFINE EVENT OVERWRITE invitation_no_duplicate_pending ON invitation
    WHEN $event = "CREATE" THEN {
        LET $dups = (
            SELECT count() FROM invitation
            WHERE brokerage = $after.brokerage
              AND email = $after.email
              AND accepted = false
              AND declined = false
              AND id != $after.id
            GROUP ALL
        );
        IF array::len($dups) > 0 AND $dups[0].count > 0 {
            THROW "duplicate pending invitation for this brokerage + email";
        };
    };

-- ----------------------------------------------------------------------------
-- Transaction (CAR-forms revision)
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE transaction SCHEMAFULL;
DEFINE FIELD OVERWRITE property_address        ON transaction TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE city                    ON transaction TYPE string DEFAULT "";
DEFINE FIELD OVERWRITE apn                     ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE postal_code             ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE price_cents             ON transaction TYPE int DEFAULT 0
    ASSERT $value >= 0;
DEFINE FIELD OVERWRITE client_name             ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE mls_number              ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE office_file_number      ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE status                  ON transaction TYPE string DEFAULT "active"
    ASSERT $value IN ["active", "pending", "sold", "canceled", "withdrawn"];
DEFINE FIELD OVERWRITE transaction_type        ON transaction TYPE string DEFAULT "residential"
    ASSERT $value IN [
        "residential", "commercial", "multi_family", "vacant_lots_land",
        "manufactured_home", "business_opportunity",
        "commercial_lease", "rental_lease"
    ];
DEFINE FIELD OVERWRITE special_sales_condition ON transaction TYPE string DEFAULT "none"
    ASSERT $value IN ["none", "probate", "short_sale", "reo"];
DEFINE FIELD OVERWRITE sales_type              ON transaction TYPE string DEFAULT "listing"
    ASSERT $value IN [
        "listing", "purchase", "listing_and_purchase",
        "lease_tenant", "lease_landlord", "lease_tenant_landlord", "referral"
    ];
DEFINE FIELD OVERWRITE created_at              ON transaction TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at              ON transaction TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE TABLE OVERWRITE owns            SCHEMAFULL TYPE RELATION FROM user      TO transaction;
DEFINE TABLE OVERWRITE has_transaction SCHEMAFULL TYPE RELATION FROM brokerage TO transaction;

-- Sort/filter indexes. The transactions list is the hottest read path
-- in the app (every signed-in agent lands here several times a day)
-- and supports sorting by any column. Without these indexes the
-- planner has to scan + sort the whole transaction set per request.
DEFINE INDEX OVERWRITE idx_transaction_created_at        ON transaction FIELDS created_at;
DEFINE INDEX OVERWRITE idx_transaction_status            ON transaction FIELDS status;
DEFINE INDEX OVERWRITE idx_transaction_property_address  ON transaction FIELDS property_address;
DEFINE INDEX OVERWRITE idx_transaction_price_cents       ON transaction FIELDS price_cents;
DEFINE INDEX OVERWRITE idx_transaction_transaction_type  ON transaction FIELDS transaction_type;

-- ----------------------------------------------------------------------------
-- Checklist item (grouped + form-aware)
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE checklist_item SCHEMAFULL;
DEFINE FIELD OVERWRITE title        ON checklist_item TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE form_code    ON checklist_item TYPE option<string>;
-- Data-driven grouping: the resolved form's group name (e.g.
-- "Mandatory Disclosures", or a locality-labeled "Los Angeles —
-- Mandatory Disclosures") plus its sort order, snapshotted onto the
-- item at creation so the transaction view groups + orders without
-- re-resolving the form library.
DEFINE FIELD OVERWRITE group_name   ON checklist_item TYPE string DEFAULT "Additional Disclosures";
DEFINE FIELD OVERWRITE group_order  ON checklist_item TYPE int DEFAULT 4;
DEFINE FIELD OVERWRITE position        ON checklist_item TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE required        ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE approval_status ON checklist_item TYPE string DEFAULT "pending"
    ASSERT $value IN ["pending", "approved", "denied"];
DEFINE FIELD OVERWRITE reviewed_at     ON checklist_item TYPE option<datetime>;
DEFINE FIELD OVERWRITE reviewed_by     ON checklist_item TYPE option<record<user>>;
DEFINE FIELD OVERWRITE created_at      ON checklist_item TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at      ON checklist_item TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE TABLE OVERWRITE has_item SCHEMAFULL TYPE RELATION FROM transaction TO checklist_item;

-- ----------------------------------------------------------------------------
-- Document (keyed by CAR form code, attachable to checklist items)
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE document SCHEMAFULL;
DEFINE FIELD OVERWRITE filename     ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE form_code    ON document TYPE string DEFAULT "MISC";
DEFINE FIELD OVERWRITE storage_key  ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE size_bytes   ON document TYPE int DEFAULT 0 ASSERT $value >= 0;
DEFINE FIELD OVERWRITE content_type ON document TYPE string DEFAULT "application/octet-stream";
DEFINE FIELD OVERWRITE signed       ON document TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE version      ON document TYPE int DEFAULT 1 ASSERT $value >= 1;
DEFINE FIELD OVERWRITE created_at   ON document TYPE datetime DEFAULT time::now();

DEFINE TABLE OVERWRITE has_document SCHEMAFULL TYPE RELATION FROM transaction    TO document;
DEFINE TABLE OVERWRITE for_item     SCHEMAFULL TYPE RELATION FROM document       TO checklist_item;
DEFINE TABLE OVERWRITE uploaded     SCHEMAFULL TYPE RELATION FROM user           TO document;
DEFINE TABLE OVERWRITE version_of   SCHEMAFULL TYPE RELATION FROM document       TO document;

-- ----------------------------------------------------------------------------
-- Comments — free-text notes attached to either a transaction or a
-- single checklist_item. The polymorphic `target` field carries the
-- record link so a single thread query can hydrate either parent type.
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE comment SCHEMAFULL;
DEFINE FIELD OVERWRITE body       ON comment TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE target     ON comment TYPE record
    ASSERT meta::tb($value) IN ["transaction", "checklist_item"];
DEFINE FIELD OVERWRITE author     ON comment TYPE record<user>;
-- Optional pointer to a document this comment refers back to. Set by the
-- system when a new version is uploaded so the prior version stays
-- one click away even after newer ones land at the top of the doc list.
DEFINE FIELD OVERWRITE references_document ON comment TYPE option<record<document>> DEFAULT NONE;
DEFINE FIELD OVERWRITE created_at ON comment TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_comment_target ON comment FIELDS target;

-- ----------------------------------------------------------------------------
-- Audit log — append-only record of security-relevant events. Every signup,
-- login (success/failure), invite, verification, blocked attempt, and admin
-- action lands here for forensic review.
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE audit_event SCHEMAFULL;
DEFINE FIELD OVERWRITE kind ON audit_event TYPE string
    ASSERT $value IN [
        "signup_pending", "signup_blocked_honeypot", "signup_blocked_pow",
        "signup_blocked_rate_limit", "signup_blocked_blacklist",
        "signup_blocked_duplicate", "verify_success", "verify_failure",
        "login_success", "login_failure", "login_blocked_unverified",
        "logout", "invite_sent", "invite_resent", "invite_cancelled",
        "invite_accepted", "invite_declined", "admin_view",
        "document_deleted",
        "profile_updated", "password_changed", "avatar_updated",
        "brokerage_deleted", "transaction_deleted",
        "transaction_reassigned",
        "member_removed",
        "tier_created", "tier_updated",
        "brokerage_comp_granted", "brokerage_comp_revoked"
    ];
DEFINE FIELD OVERWRITE actor_email ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE actor       ON audit_event TYPE option<record<user>>;
DEFINE FIELD OVERWRITE ip          ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE user_agent  ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE detail      ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE at          ON audit_event TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_audit_at   ON audit_event FIELDS at;
DEFINE INDEX OVERWRITE idx_audit_kind ON audit_event FIELDS kind;
DEFINE INDEX OVERWRITE idx_audit_ip   ON audit_event FIELDS ip;
