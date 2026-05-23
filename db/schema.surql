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
-- Migration sequencing — three phases. Two earlier prod incidents made this
-- order necessary (rows missing `approval_status`, then missing
-- `email_verified`, both crashing reads with deserialize errors):
--
--   Phase 1 (RELAX):    DEFINE every new-or-newly-required field as
--                       `option<...>` first so existing rows that lack the
--                       field don't violate SCHEMAFULL.
--   Phase 2 (MIGRATE):  UPDATE rows to backfill the new fields and rewrite
--                       legacy enum values onto the new vocabulary.
--   Phase 3 (LOCK):     re-DEFINE the same fields with their final
--                       non-option types, defaults, and ASSERTs.
--
-- DEFAULT clauses apply only to NEW inserts in SurrealDB — they do not
-- retroactively populate existing rows. Phase 2 is what makes those rows
-- pass Phase 3's tighter ASSERTs.
-- ============================================================================

-- ============================================================================
-- Phase 1 — relax: declare every newly-required column as `option<...>` so
-- the bare DEFINE doesn't reject pre-existing rows that lack the field.
-- ============================================================================

DEFINE TABLE OVERWRITE transaction SCHEMAFULL;
DEFINE FIELD OVERWRITE status                  ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE transaction_type        ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE special_sales_condition ON transaction TYPE option<string>;
DEFINE FIELD OVERWRITE sales_type              ON transaction TYPE option<string>;

DEFINE TABLE OVERWRITE checklist_item SCHEMAFULL;
DEFINE FIELD OVERWRITE group_slug      ON checklist_item TYPE option<string>;
DEFINE FIELD OVERWRITE required        ON checklist_item TYPE option<bool>;
DEFINE FIELD OVERWRITE approval_status ON checklist_item TYPE option<string>;
DEFINE FIELD OVERWRITE reviewed_at     ON checklist_item TYPE option<datetime>;
DEFINE FIELD OVERWRITE reviewed_by     ON checklist_item TYPE option<record<user>>;
-- Legacy fields kept temporarily so the MIGRATE phase can read them.
DEFINE FIELD OVERWRITE completed       ON checklist_item TYPE option<bool>;
DEFINE FIELD OVERWRITE completed_at    ON checklist_item TYPE option<datetime>;
DEFINE FIELD OVERWRITE completed_by    ON checklist_item TYPE option<record<user>>;

DEFINE TABLE OVERWRITE document SCHEMAFULL;
DEFINE FIELD OVERWRITE form_code ON document TYPE option<string>;
-- Legacy field kept temporarily for MIGRATE.
DEFINE FIELD OVERWRITE category  ON document TYPE option<string>;

DEFINE TABLE OVERWRITE user SCHEMAFULL;
DEFINE FIELD OVERWRITE email_verified ON user TYPE option<bool>;

DEFINE TABLE OVERWRITE brokerage SCHEMAFULL;
-- New super-admin-toggle field. Existing brokerage rows predate it,
-- so relax to `option<bool>` here, backfill to `false` in MIGRATE,
-- then lock to `bool DEFAULT false` in LOCK.
DEFINE FIELD OVERWRITE is_complimentary ON brokerage TYPE option<bool>;

-- ============================================================================
-- Phase 2 — migrate: rewrite legacy values onto the new vocabulary and
-- backfill every newly-required field for existing rows.
-- ============================================================================

-- transaction.status: old PoC enum -> new five-value enum.
UPDATE transaction SET status = 'active'   WHERE status = 'open';
UPDATE transaction SET status = 'pending'  WHERE status = 'under_contract';
UPDATE transaction SET status = 'sold'     WHERE status = 'closed';
UPDATE transaction SET status = 'canceled' WHERE status = 'cancelled';
UPDATE transaction SET status = 'active'   WHERE status = NONE;

-- transaction: new required dropdowns get sensible defaults.
UPDATE transaction SET transaction_type        = 'residential' WHERE transaction_type        = NONE;
UPDATE transaction SET special_sales_condition = 'none'        WHERE special_sales_condition = NONE;
UPDATE transaction SET sales_type              = 'listing'     WHERE sales_type              = NONE;

-- checklist_item: legacy `completed*` columns -> new `approval_status` /
-- `reviewed_*`. Then drop the old columns so the LOCK phase can remove
-- them from the schema.
UPDATE checklist_item SET
    approval_status = IF completed = true THEN 'approved' ELSE 'pending' END
    WHERE approval_status = NONE;
UPDATE checklist_item SET reviewed_at = completed_at WHERE reviewed_at = NONE;
UPDATE checklist_item SET reviewed_by = completed_by WHERE reviewed_by = NONE;
UPDATE checklist_item UNSET completed, completed_at, completed_by;

-- checklist_item: backfill grouping + required flags for legacy items.
UPDATE checklist_item SET group_slug = 'additional' WHERE group_slug = NONE;
UPDATE checklist_item SET required   = false        WHERE required   = NONE;

-- document.form_code: derived from the old `category` field; default to
-- `MISC` when neither is set. Then drop `category`.
UPDATE document SET form_code = string::uppercase(category)
    WHERE form_code = NONE AND category != NONE;
UPDATE document SET form_code = 'MISC' WHERE form_code = NONE;
UPDATE document UNSET category;

-- user.email_verified: legacy users predate the verification flow. We
-- treat them as already verified — they've been using the system, so
-- bouncing them now would be a worse experience than a small data drift.
UPDATE user SET email_verified = true WHERE email_verified = NONE;

-- brokerage.is_complimentary: brand-new field; existing brokerages
-- default to "not complimentary" so paid + trial flows are unaffected.
UPDATE brokerage SET is_complimentary = false WHERE is_complimentary = NONE;

-- ============================================================================
-- Phase 3 — lock: re-DEFINE every relaxed field with its final type,
-- default, and ASSERT. Then DEFINE the rest of the schema.
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
DEFINE FIELD OVERWRITE created_at ON brokerage TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON brokerage TYPE datetime DEFAULT time::now() VALUE time::now();

-- ----------------------------------------------------------------------------
-- User
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE user SCHEMAFULL;
DEFINE FIELD OVERWRITE email         ON user TYPE string ASSERT string::is_email($value);
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
DEFINE FIELD OVERWRITE email      ON invitation TYPE string ASSERT string::is_email($value);
DEFINE FIELD OVERWRITE role       ON invitation TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD OVERWRITE token      ON invitation TYPE string ASSERT string::len($value) >= 16;
DEFINE FIELD OVERWRITE brokerage  ON invitation TYPE record<brokerage>;
DEFINE FIELD OVERWRITE invited_by ON invitation TYPE record<user>;
DEFINE FIELD OVERWRITE accepted   ON invitation TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE created_at ON invitation TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_invitation_token ON invitation FIELDS token UNIQUE;

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
DEFINE FIELD OVERWRITE group_slug   ON checklist_item TYPE string DEFAULT "additional"
    ASSERT $value IN [
        "mls",
        -- New (May 2026) contract groups, split from the legacy
        -- "contracts" bucket. Singular vs plural carries semantics:
        -- plural variants are used by checklists that bundle two main
        -- contracts (Residential Purchase = RPA + RIPA, Manufactured
        -- Home = RLA + MHLA and RPA + MHPA).
        "listing_contract", "listing_contracts",
        "purchase_contract", "purchase_contracts",
        "mandatory",
        "additional",
        "escrow", "reports", "release",
        -- Legacy slugs from before the May-2026 refactor — kept in the
        -- ASSERT so old persisted rows still satisfy the schema until a
        -- prod migration sweeps them. New writes must not use these.
        "contracts", "special", "if_applicable"
    ];
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
        "invite_accepted", "admin_view",
        "document_deleted",
        "profile_updated", "password_changed", "avatar_updated",
        "brokerage_deleted",
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

-- ----------------------------------------------------------------------------
-- Migrations log
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE _migrations SCHEMAFULL;
DEFINE FIELD OVERWRITE name       ON _migrations TYPE string;
DEFINE FIELD OVERWRITE applied_at ON _migrations TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_migration_name ON _migrations FIELDS name UNIQUE;
