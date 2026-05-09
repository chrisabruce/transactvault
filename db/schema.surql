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
-- ============================================================================

-- Migrate any data still on the pre-CAR-forms schema. Old `transaction.status`
-- values get mapped onto the new enum BEFORE the new ASSERT is installed,
-- otherwise the OVERWRITE fails on rows that violate the new constraint.
UPDATE transaction SET status = 'active'   WHERE status = 'open';
UPDATE transaction SET status = 'pending'  WHERE status = 'under_contract';
UPDATE transaction SET status = 'sold'     WHERE status = 'closed';
UPDATE transaction SET status = 'canceled' WHERE status = 'cancelled';

-- Pre-fill new required fields on existing rows so OVERWRITE+ASSERT doesn't
-- reject anything currently in the table.
UPDATE transaction SET
    transaction_type        = 'residential',
    special_sales_condition = 'none',
    sales_type              = 'listing'
    WHERE transaction_type IS NONE OR transaction_type = NONE;

UPDATE checklist_item SET
    group_slug = 'additional',
    required   = false
    WHERE group_slug IS NONE OR group_slug = NONE;

-- Migrate legacy `completed`/`completed_at`/`completed_by` to the new
-- approval workflow. `completed=true` rows become approved (with the same
-- timestamp + reviewer); everything else starts as pending. We do this
-- BEFORE the OVERWRITE+ASSERT below so existing rows don't get rejected.
UPDATE checklist_item SET
    approval_status = IF completed = true THEN 'approved' ELSE 'pending' END,
    reviewed_at     = completed_at,
    reviewed_by     = completed_by
    WHERE approval_status IS NONE OR approval_status = NONE;
-- Drop the old columns so SCHEMAFULL doesn't reject inserts that omit them.
UPDATE checklist_item UNSET completed, completed_at, completed_by;

-- Documents previously had a `category` field. Map it onto `form_code`,
-- which is the new canonical name.
UPDATE document SET form_code = string::uppercase(category)
    WHERE form_code IS NONE OR form_code = NONE;

-- ----------------------------------------------------------------------------
-- Brokerage
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE brokerage SCHEMAFULL;
DEFINE FIELD OVERWRITE name       ON brokerage TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE city       ON brokerage TYPE option<string>;
DEFINE FIELD OVERWRITE state      ON brokerage TYPE string DEFAULT "CA";
DEFINE FIELD OVERWRITE plan       ON brokerage TYPE string DEFAULT "trial"
    ASSERT $value IN ["trial", "starter", "growth", "scale", "enterprise"];
DEFINE FIELD OVERWRITE created_at ON brokerage TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at ON brokerage TYPE datetime DEFAULT time::now() VALUE time::now();

-- ----------------------------------------------------------------------------
-- User
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE user SCHEMAFULL;
DEFINE FIELD OVERWRITE email         ON user TYPE string ASSERT string::is_email($value);
DEFINE FIELD OVERWRITE name          ON user TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE password_hash ON user TYPE string;
DEFINE FIELD OVERWRITE photo_url     ON user TYPE option<string>;

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

-- ----------------------------------------------------------------------------
-- Checklist item (grouped + form-aware)
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE checklist_item SCHEMAFULL;
DEFINE FIELD OVERWRITE title        ON checklist_item TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD OVERWRITE form_code    ON checklist_item TYPE option<string>;
DEFINE FIELD OVERWRITE group_slug   ON checklist_item TYPE string DEFAULT "additional"
    ASSERT $value IN [
        "mls", "contracts", "mandatory", "special",
        "additional", "if_applicable", "escrow", "reports", "release"
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
        "logout", "invite_sent", "invite_accepted", "admin_view"
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
