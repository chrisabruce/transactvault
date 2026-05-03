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
-- ============================================================================

-- ----------------------------------------------------------------------------
-- Brokerage
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
-- User
-- ----------------------------------------------------------------------------
DEFINE TABLE user SCHEMAFULL;
DEFINE FIELD email         ON user TYPE string ASSERT string::is_email($value);
DEFINE FIELD name          ON user TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD password_hash ON user TYPE string;
DEFINE FIELD photo_url     ON user TYPE option<string>;
DEFINE FIELD created_at    ON user TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at    ON user TYPE datetime DEFAULT time::now() VALUE time::now();
DEFINE INDEX idx_user_email ON user FIELDS email UNIQUE;

DEFINE TABLE works_at SCHEMAFULL TYPE RELATION FROM user TO brokerage;
DEFINE FIELD role      ON works_at TYPE string
    ASSERT $value IN ["broker", "agent", "coordinator"];
DEFINE FIELD joined_at ON works_at TYPE datetime DEFAULT time::now();

-- ----------------------------------------------------------------------------
-- Invitation
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
-- Transaction (CAR-forms revision)
-- ----------------------------------------------------------------------------
DEFINE TABLE transaction SCHEMAFULL;
DEFINE FIELD property_address        ON transaction TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD city                    ON transaction TYPE string DEFAULT "";
DEFINE FIELD apn                     ON transaction TYPE option<string>;
DEFINE FIELD postal_code             ON transaction TYPE option<string>;
DEFINE FIELD price_cents             ON transaction TYPE int DEFAULT 0
    ASSERT $value >= 0;
DEFINE FIELD client_name             ON transaction TYPE option<string>;
DEFINE FIELD mls_number              ON transaction TYPE option<string>;
DEFINE FIELD office_file_number      ON transaction TYPE option<string>;
DEFINE FIELD status                  ON transaction TYPE string DEFAULT "active"
    ASSERT $value IN ["active", "pending", "sold", "canceled", "withdrawn"];
DEFINE FIELD transaction_type        ON transaction TYPE string DEFAULT "residential"
    ASSERT $value IN [
        "residential", "commercial", "multi_family", "vacant_lots_land",
        "manufactured_home", "business_opportunity",
        "commercial_lease", "rental_lease"
    ];
DEFINE FIELD special_sales_condition ON transaction TYPE string DEFAULT "none"
    ASSERT $value IN ["none", "probate", "short_sale", "reo"];
DEFINE FIELD sales_type              ON transaction TYPE string DEFAULT "listing"
    ASSERT $value IN [
        "listing", "purchase", "listing_and_purchase",
        "lease_tenant", "lease_landlord", "lease_tenant_landlord", "referral"
    ];
DEFINE FIELD created_at              ON transaction TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at              ON transaction TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE TABLE owns            SCHEMAFULL TYPE RELATION FROM user      TO transaction;
DEFINE TABLE has_transaction SCHEMAFULL TYPE RELATION FROM brokerage TO transaction;

-- ----------------------------------------------------------------------------
-- Checklist item (now grouped + form-aware)
-- ----------------------------------------------------------------------------
DEFINE TABLE checklist_item SCHEMAFULL;
DEFINE FIELD title        ON checklist_item TYPE string
    ASSERT string::len($value) >= 1;
DEFINE FIELD form_code    ON checklist_item TYPE option<string>;
DEFINE FIELD group_slug   ON checklist_item TYPE string DEFAULT "additional"
    ASSERT $value IN [
        "mls", "contracts", "mandatory", "special",
        "additional", "if_applicable", "escrow", "reports", "release"
    ];
DEFINE FIELD position     ON checklist_item TYPE int DEFAULT 0;
DEFINE FIELD required     ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD completed    ON checklist_item TYPE bool DEFAULT false;
DEFINE FIELD completed_at ON checklist_item TYPE option<datetime>;
DEFINE FIELD completed_by ON checklist_item TYPE option<record<user>>;
DEFINE FIELD created_at   ON checklist_item TYPE datetime DEFAULT time::now();
DEFINE FIELD updated_at   ON checklist_item TYPE datetime DEFAULT time::now() VALUE time::now();

DEFINE TABLE has_item SCHEMAFULL TYPE RELATION FROM transaction TO checklist_item;

-- ----------------------------------------------------------------------------
-- Document (now keyed by CAR form code, attachable to checklist items)
-- ----------------------------------------------------------------------------
DEFINE TABLE document SCHEMAFULL;
DEFINE FIELD filename     ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD form_code    ON document TYPE string DEFAULT "MISC";
DEFINE FIELD storage_key  ON document TYPE string ASSERT string::len($value) >= 1;
DEFINE FIELD size_bytes   ON document TYPE int DEFAULT 0 ASSERT $value >= 0;
DEFINE FIELD content_type ON document TYPE string DEFAULT "application/octet-stream";
DEFINE FIELD signed       ON document TYPE bool DEFAULT false;
DEFINE FIELD version      ON document TYPE int DEFAULT 1 ASSERT $value >= 1;
DEFINE FIELD created_at   ON document TYPE datetime DEFAULT time::now();

DEFINE TABLE has_document SCHEMAFULL TYPE RELATION FROM transaction    TO document;
DEFINE TABLE for_item     SCHEMAFULL TYPE RELATION FROM document       TO checklist_item;
DEFINE TABLE uploaded     SCHEMAFULL TYPE RELATION FROM user           TO document;
DEFINE TABLE version_of   SCHEMAFULL TYPE RELATION FROM document       TO document;

-- ----------------------------------------------------------------------------
-- Migrations log
-- ----------------------------------------------------------------------------
DEFINE TABLE _migrations SCHEMAFULL;
DEFINE FIELD name       ON _migrations TYPE string;
DEFINE FIELD applied_at ON _migrations TYPE datetime DEFAULT time::now();
DEFINE INDEX idx_migration_name ON _migrations FIELDS name UNIQUE;
