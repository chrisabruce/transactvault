-- ============================================================================
-- TransactVault schema
--
-- Graph-first design:
--   (user)-[works_at]->(brokerage)
--   (user)-[owns]->(transaction)
--   (brokerage)-[has_transaction]->(transaction)
--   (transaction)-[has_item]->(checklist_item)
--   (transaction)-[has_document]->(document)
--   (user)-[completed]->(checklist_item)       (audit trail edge)
--   (user)-[uploaded]->(document)              (audit trail edge)
--   (document)-[version_of]->(document)        (version history, self-relation)
--
-- All tables are SCHEMAFULL. RecordId is the canonical key type everywhere;
-- never parse, concatenate, or interpolate record IDs as strings.
-- ============================================================================

-- ----------------------------------------------------------------------------
-- Brokerage — the tenant/account
-- ----------------------------------------------------------------------------
DEFINE TABLE brokerage SCHEMAFULL;
DEFINE FIELD name       ON brokerage TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD city       ON brokerage TYPE option<string>;
DEFINE FIELD state      ON brokerage TYPE string DEFAULT "CA";
DEFINE FIELD plan       ON brokerage TYPE string DEFAULT "trial"
    ASSERT $value IN ["trial", "starter", "growth", "scale", "enterprise"];
DEFINE FIELD created_at ON brokerage TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at ON brokerage TYPE datetime DEFAULT time::now() VALUE time::now();

-- ----------------------------------------------------------------------------
-- User — authenticated principal, scoped to a brokerage via `works_at`
-- ----------------------------------------------------------------------------
DEFINE TABLE user SCHEMAFULL;
DEFINE FIELD email         ON user TYPE string ASSERT string::is_email($value);
DEFINE FIELD name          ON user TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD password_hash ON user TYPE string;
DEFINE FIELD photo_url     ON user TYPE option<string>;
DEFINE FIELD created_at    ON user TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at    ON user TYPE datetime DEFAULT time::now() VALUE time::now();
DEFINE INDEX idx_user_email ON user FIELDS email UNIQUE;

-- ----------------------------------------------------------------------------
-- works_at — user belongs to a brokerage with a role
-- ----------------------------------------------------------------------------
DEFINE TABLE works_at SCHEMAFULL TYPE RELATION FROM user TO brokerage;
DEFINE FIELD role      ON works_at TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD joined_at ON works_at TYPE datetime DEFAULT time::now();

-- ----------------------------------------------------------------------------
-- Invitation — pending member invite, resolved to a user on acceptance
-- ----------------------------------------------------------------------------
DEFINE TABLE invitation SCHEMAFULL;
DEFINE FIELD email      ON invitation TYPE string ASSERT string::is_email($value);
DEFINE FIELD role       ON invitation TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD token      ON invitation TYPE string ASSERT string::len($value) >= 16;
DEFINE FIELD brokerage  ON invitation TYPE record<brokerage>;
DEFINE FIELD invited_by ON invitation TYPE record<user>;
DEFINE FIELD accepted   ON invitation TYPE bool DEFAULT false;
DEFINE FIELD created_at ON invitation TYPE datetime DEFAULT time::now();
DEFINE INDEX idx_invitation_token ON invitation FIELDS token UNIQUE;

-- ----------------------------------------------------------------------------
-- Transaction — a real estate deal
-- ----------------------------------------------------------------------------
DEFINE TABLE transaction SCHEMAFULL;
DEFINE FIELD property_address ON transaction TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD city             ON transaction TYPE string DEFAULT "";
DEFINE FIELD state            ON transaction TYPE string DEFAULT "CA";
DEFINE FIELD postal_code      ON transaction TYPE option<string>;
DEFINE FIELD price_cents      ON transaction TYPE int DEFAULT 0 ASSERT $value >= 0;
DEFINE FIELD buyer_name       ON transaction TYPE option<string>;
DEFINE FIELD seller_name      ON transaction TYPE option<string>;
DEFINE FIELD expected_close   ON transaction TYPE option<datetime>;
DEFINE FIELD status           ON transaction TYPE string DEFAULT "open"
    ASSERT $value IN ["open", "under_contract", "closed", "cancelled"];
DEFINE FIELD created_at       ON transaction TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at       ON transaction TYPE datetime DEFAULT time::now() VALUE time::now();

-- Graph relations for a transaction
DEFINE TABLE owns           SCHEMAFULL TYPE RELATION FROM user      TO transaction;
DEFINE TABLE has_transaction SCHEMAFULL TYPE RELATION FROM brokerage TO transaction;

-- ----------------------------------------------------------------------------
-- Checklist item — one compliance step on a transaction
-- ----------------------------------------------------------------------------
DEFINE TABLE checklist_item SCHEMAFULL;
DEFINE FIELD title        ON checklist_item TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD category     ON checklist_item TYPE string DEFAULT "general"
    ASSERT $value IN ["contract","disclosures","inspection","appraisal","title","closing","general"];
DEFINE FIELD position     ON checklist_item TYPE int DEFAULT 0;
DEFINE FIELD completed    ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD completed_at ON checklist_item TYPE option<datetime>;
DEFINE FIELD completed_by ON checklist_item TYPE option<record<user>>;
DEFINE FIELD created_at   ON checklist_item TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at   ON checklist_item TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE TABLE has_item SCHEMAFULL TYPE RELATION FROM transaction TO checklist_item;

-- ----------------------------------------------------------------------------
-- Document — a file attached to a transaction
-- ----------------------------------------------------------------------------
DEFINE TABLE document SCHEMAFULL;
DEFINE FIELD filename     ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD category     ON document TYPE string DEFAULT "general"
    ASSERT $value IN ["contract","disclosures","inspection","appraisal","title","closing","general"];
DEFINE FIELD storage_key  ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD size_bytes   ON document TYPE int DEFAULT 0 ASSERT $value >= 0;
DEFINE FIELD content_type ON document TYPE string DEFAULT "application/octet-stream";
DEFINE FIELD signed       ON document TYPE bool DEFAULT false;
DEFINE FIELD version      ON document TYPE int DEFAULT 1 ASSERT $value >= 1;
DEFINE FIELD created_at   ON document TYPE datetime DEFAULT time::now();

DEFINE TABLE has_document SCHEMAFULL TYPE RELATION FROM transaction TO document;
DEFINE TABLE uploaded     SCHEMAFULL TYPE RELATION FROM user        TO document;
DEFINE TABLE version_of   SCHEMAFULL TYPE RELATION FROM document    TO document;

-- ----------------------------------------------------------------------------
-- Migrations log — tracks what's been applied for idempotency
-- ----------------------------------------------------------------------------
DEFINE TABLE _migrations SCHEMAFULL;
DEFINE FIELD name       ON _migrations TYPE string;
DEFINE FIELD applied_at ON _migrations TYPE datetime DEFAULT time::now();
DEFINE INDEX idx_migration_name ON _migrations FIELDS name UNIQUE;
