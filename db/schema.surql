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
DEFINE FIELD OVERWRITE created_at    ON user TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at    ON user TYPE datetime DEFAULT time::now() VALUE time::now();
DEFINE INDEX OVERWRITE idx_user_email ON user FIELDS email UNIQUE;

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
DEFINE FIELD OVERWRITE position     ON checklist_item TYPE int DEFAULT 0;
DEFINE FIELD OVERWRITE required     ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE completed    ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD OVERWRITE completed_at ON checklist_item TYPE option<datetime>;
DEFINE FIELD OVERWRITE completed_by ON checklist_item TYPE option<record<user>>;
DEFINE FIELD OVERWRITE created_at   ON checklist_item TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE updated_at   ON checklist_item TYPE datetime DEFAULT time::now() VALUE time::now();

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
-- Migrations log
-- ----------------------------------------------------------------------------
DEFINE TABLE OVERWRITE _migrations SCHEMAFULL;
DEFINE FIELD OVERWRITE name       ON _migrations TYPE string;
DEFINE FIELD OVERWRITE applied_at ON _migrations TYPE datetime DEFAULT time::now();
DEFINE INDEX OVERWRITE idx_migration_name ON _migrations FIELDS name UNIQUE;
