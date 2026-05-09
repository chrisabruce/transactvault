//! `transaction` table — one real estate deal, the unit of compliance work.
//!
//! Fields match the New Transaction page in `docs/New Transaction Page.pdf`.
//! Anything that page doesn't include (buyer/seller name, expected close)
//! is intentionally absent — the original PoC fields were dropped during
//! the CAR-forms refactor.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::{RecordId, SurrealValue};

use crate::record_key;

// ---------------------------------------------------------------------------
// Status (renamed from the old Open/Under Contract/... set to match the spec)
// ---------------------------------------------------------------------------

/// Lifecycle of a deal. Stored on the row as a lowercase slug via `as_str`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    Active,
    Pending,
    Sold,
    Canceled,
    Withdrawn,
}

impl TransactionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TransactionStatus::Active => "active",
            TransactionStatus::Pending => "pending",
            TransactionStatus::Sold => "sold",
            TransactionStatus::Canceled => "canceled",
            TransactionStatus::Withdrawn => "withdrawn",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TransactionStatus::Active => "Active",
            TransactionStatus::Pending => "Pending",
            TransactionStatus::Sold => "Sold",
            TransactionStatus::Canceled => "Canceled",
            TransactionStatus::Withdrawn => "Withdrawn",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "pending" => Some(Self::Pending),
            "sold" => Some(Self::Sold),
            "canceled" => Some(Self::Canceled),
            "withdrawn" => Some(Self::Withdrawn),
            _ => None,
        }
    }

    pub fn all() -> [Self; 5] {
        [
            Self::Active,
            Self::Pending,
            Self::Sold,
            Self::Canceled,
            Self::Withdrawn,
        ]
    }
}

// ---------------------------------------------------------------------------
// Transaction Type (drives smart checklist selection)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionType {
    Residential,
    Commercial,
    MultiFamily,
    VacantLotsLand,
    ManufacturedHome,
    BusinessOpportunity,
    CommercialLease,
    RentalLease,
}

impl TransactionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Residential => "residential",
            Self::Commercial => "commercial",
            Self::MultiFamily => "multi_family",
            Self::VacantLotsLand => "vacant_lots_land",
            Self::ManufacturedHome => "manufactured_home",
            Self::BusinessOpportunity => "business_opportunity",
            Self::CommercialLease => "commercial_lease",
            Self::RentalLease => "rental_lease",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Residential => "Residential",
            Self::Commercial => "Commercial",
            Self::MultiFamily => "Multi-Family (5+ Units)",
            Self::VacantLotsLand => "Vacant Lots & Land",
            Self::ManufacturedHome => "Manufactured / Mobile Home",
            Self::BusinessOpportunity => "Business Opportunity",
            Self::CommercialLease => "Commercial Lease",
            Self::RentalLease => "Rental / Lease",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "residential" => Some(Self::Residential),
            "commercial" => Some(Self::Commercial),
            "multi_family" => Some(Self::MultiFamily),
            "vacant_lots_land" => Some(Self::VacantLotsLand),
            "manufactured_home" => Some(Self::ManufacturedHome),
            "business_opportunity" => Some(Self::BusinessOpportunity),
            "commercial_lease" => Some(Self::CommercialLease),
            "rental_lease" => Some(Self::RentalLease),
            _ => None,
        }
    }

    pub fn all() -> [Self; 8] {
        [
            Self::Residential,
            Self::Commercial,
            Self::MultiFamily,
            Self::VacantLotsLand,
            Self::ManufacturedHome,
            Self::BusinessOpportunity,
            Self::CommercialLease,
            Self::RentalLease,
        ]
    }
}

// ---------------------------------------------------------------------------
// Special Sales Condition
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// REO ("Real Estate Owned") is the industry-standard acronym used on every
// CAR form — keeping it uppercase matches printed checklists and broker
// vocabulary, even though clippy's house style prefers Reo.
#[allow(clippy::upper_case_acronyms)]
pub enum SpecialSalesCondition {
    None,
    Probate,
    ShortSale,
    REO,
}

impl SpecialSalesCondition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Probate => "probate",
            Self::ShortSale => "short_sale",
            Self::REO => "reo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Probate => "Probate Sale",
            Self::ShortSale => "Short Sale",
            Self::REO => "REO (Bank Owned)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "probate" => Some(Self::Probate),
            "short_sale" => Some(Self::ShortSale),
            "reo" => Some(Self::REO),
            _ => None,
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::None, Self::Probate, Self::ShortSale, Self::REO]
    }
}

// ---------------------------------------------------------------------------
// Sales Type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SalesType {
    Listing,
    Purchase,
    ListingAndPurchase,
    LeaseTenant,
    LeaseLandlord,
    LeaseTenantAndLandlord,
    Referral,
}

impl SalesType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listing => "listing",
            Self::Purchase => "purchase",
            Self::ListingAndPurchase => "listing_and_purchase",
            Self::LeaseTenant => "lease_tenant",
            Self::LeaseLandlord => "lease_landlord",
            Self::LeaseTenantAndLandlord => "lease_tenant_landlord",
            Self::Referral => "referral",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Listing => "Listing",
            Self::Purchase => "Purchase",
            Self::ListingAndPurchase => "Listing & Purchase",
            Self::LeaseTenant => "Lease (Tenant)",
            Self::LeaseLandlord => "Lease (Landlord)",
            Self::LeaseTenantAndLandlord => "Lease (Tenant & Landlord)",
            Self::Referral => "Referral",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "listing" => Some(Self::Listing),
            "purchase" => Some(Self::Purchase),
            "listing_and_purchase" => Some(Self::ListingAndPurchase),
            "lease_tenant" => Some(Self::LeaseTenant),
            "lease_landlord" => Some(Self::LeaseLandlord),
            "lease_tenant_landlord" => Some(Self::LeaseTenantAndLandlord),
            "referral" => Some(Self::Referral),
            _ => None,
        }
    }

    pub fn all() -> [Self; 7] {
        [
            Self::Listing,
            Self::Purchase,
            Self::ListingAndPurchase,
            Self::LeaseTenant,
            Self::LeaseLandlord,
            Self::LeaseTenantAndLandlord,
            Self::Referral,
        ]
    }
}

// ---------------------------------------------------------------------------
// Persisted record
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, SurrealValue)]
pub struct Transaction {
    pub id: RecordId,
    pub property_address: String,
    pub city: String,
    pub apn: Option<String>,
    pub postal_code: Option<String>,
    pub price_cents: i64,
    pub client_name: Option<String>,
    pub mls_number: Option<String>,
    pub office_file_number: Option<String>,
    pub status: String,
    pub transaction_type: String,
    pub special_sales_condition: String,
    pub sales_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Transaction {
    pub fn status_enum(&self) -> TransactionStatus {
        TransactionStatus::parse(&self.status).unwrap_or(TransactionStatus::Active)
    }

    pub fn type_enum(&self) -> TransactionType {
        TransactionType::parse(&self.transaction_type).unwrap_or(TransactionType::Residential)
    }

    pub fn condition_enum(&self) -> SpecialSalesCondition {
        SpecialSalesCondition::parse(&self.special_sales_condition)
            .unwrap_or(SpecialSalesCondition::None)
    }

    pub fn sales_enum(&self) -> SalesType {
        SalesType::parse(&self.sales_type).unwrap_or(SalesType::Listing)
    }

    /// Human-friendly price for templates. We store cents as `i64` to avoid
    /// float drift; formatting is a view concern.
    pub fn price_display(&self) -> String {
        if self.price_cents <= 0 {
            return "—".into();
        }
        let dollars = self.price_cents / 100;
        let s = dollars.to_string();
        let chars: Vec<char> = s.chars().collect();
        let len = chars.len();
        let mut out = String::with_capacity(len + len / 3 + 1);
        out.push('$');
        chars.iter().enumerate().for_each(|(i, c)| {
            out.push(*c);
            let remaining = len - i - 1;
            if remaining > 0 && remaining.is_multiple_of(3) {
                out.push(',');
            }
        });
        out
    }

    pub fn status_label(&self) -> &'static str {
        self.status_enum().label()
    }

    pub fn type_label(&self) -> &'static str {
        self.type_enum().label()
    }

    pub fn url_key(&self) -> String {
        record_key(&self.id)
    }

    /// CSS hook for status colouring.
    pub fn status_class(&self) -> &'static str {
        match self.status_enum() {
            TransactionStatus::Active => "status-active",
            TransactionStatus::Pending => "status-pending",
            TransactionStatus::Sold => "status-sold",
            TransactionStatus::Canceled => "status-canceled",
            TransactionStatus::Withdrawn => "status-withdrawn",
        }
    }
}

// ---------------------------------------------------------------------------
// Insert shape
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, SurrealValue)]
pub struct NewTransaction {
    pub property_address: String,
    pub city: String,
    pub apn: Option<String>,
    pub postal_code: Option<String>,
    pub price_cents: i64,
    pub client_name: Option<String>,
    pub mls_number: Option<String>,
    pub office_file_number: Option<String>,
    pub status: String,
    pub transaction_type: String,
    pub special_sales_condition: String,
    pub sales_type: String,
}
