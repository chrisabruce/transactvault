//! California Association of REALTORS (CAR) forms library and the smart
//! checklist generator.
//!
//! Two responsibilities:
//!
//! 1. **Master form library** — the canonical 2026 CAR forms catalog with
//!    code, full name, and a short description. Every checklist item that
//!    represents a form points back into this catalog by `code`.
//!
//! 2. **Default checklist templates** — for a given
//!    (TransactionType, SpecialSalesCondition, SalesType) tuple, produce the
//!    grouped checklist the broker would normally start with: which group
//!    each form belongs to, and whether it's required to reach
//!    "Compliance Complete".
//!
//! Everything is `&'static` data — the library and the templates live in
//! the binary, no DB hydration step. This keeps the lookup zero-cost and
//! makes the canonical list trivial to audit in code review.

use crate::models::transaction::{SalesType, SpecialSalesCondition, TransactionType};

/// One CAR form. `allows_multiple` corresponds to the trailing `+` in the
/// printed checklists — meaning the brokerage may attach more than one
/// instance (e.g. multiple addenda, multiple counter offers).
#[derive(Debug, Clone, Copy)]
pub struct CarForm {
    pub code: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub allows_multiple: bool,
}

/// One section in a checklist — these match the headings in CAR's printed
/// transaction checklists exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormGroup {
    MlsDataSheets,
    ListingPurchasingContracts,
    MandatoryDisclosures,
    SpecialConditionsDisclosures,
    AdditionalDisclosures,
    DisclosuresIfApplicable,
    EscrowDocuments,
    ReportsCertificatesClearances,
    ReleaseDisclosures,
}

impl FormGroup {
    /// Render order — matches every printed CAR checklist.
    pub const ORDERED: [FormGroup; 9] = [
        FormGroup::MlsDataSheets,
        FormGroup::ListingPurchasingContracts,
        FormGroup::MandatoryDisclosures,
        FormGroup::SpecialConditionsDisclosures,
        FormGroup::AdditionalDisclosures,
        FormGroup::DisclosuresIfApplicable,
        FormGroup::EscrowDocuments,
        FormGroup::ReportsCertificatesClearances,
        FormGroup::ReleaseDisclosures,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FormGroup::MlsDataSheets => "MLS Data Sheets",
            FormGroup::ListingPurchasingContracts => "Listing / Purchasing Contracts",
            FormGroup::MandatoryDisclosures => "Mandatory Disclosures",
            FormGroup::SpecialConditionsDisclosures => "Special Conditions Disclosures",
            FormGroup::AdditionalDisclosures => "Additional Disclosures",
            FormGroup::DisclosuresIfApplicable => "Disclosures — If Applicable",
            FormGroup::EscrowDocuments => "Escrow Documents",
            FormGroup::ReportsCertificatesClearances => "Reports, Certificates & Clearances",
            FormGroup::ReleaseDisclosures => "Release Disclosures",
        }
    }

    /// Stable string slug used for storage and parameterised URLs.
    pub fn slug(self) -> &'static str {
        match self {
            FormGroup::MlsDataSheets => "mls",
            FormGroup::ListingPurchasingContracts => "contracts",
            FormGroup::MandatoryDisclosures => "mandatory",
            FormGroup::SpecialConditionsDisclosures => "special",
            FormGroup::AdditionalDisclosures => "additional",
            FormGroup::DisclosuresIfApplicable => "if_applicable",
            FormGroup::EscrowDocuments => "escrow",
            FormGroup::ReportsCertificatesClearances => "reports",
            FormGroup::ReleaseDisclosures => "release",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ORDERED.into_iter().find(|g| g.slug() == s)
    }
}

/// Smart-defaults entry: which form, in which group, required-or-not.
#[derive(Debug, Clone, Copy)]
pub struct DefaultItem {
    pub code: &'static str,
    pub group: FormGroup,
    pub required: bool,
}

/// Canonical PDF-order index for a form code within its group. The order
/// here mirrors the printed CAR transaction checklists exactly so the UI
/// always renders items in the expected sequence — independent of when
/// each item was attached to the transaction.
///
/// Codes not in this map sort after every known code, in alphabetical
/// order by their slug. Custom (non-CAR) items sort at the very end.
pub fn canonical_position(code: &str) -> u32 {
    // The order this match arm lists codes in IS the canonical PDF order.
    // The integer returned is each code's rank — assign sequentially from
    // 0 within each group's section so the sort is deterministic.
    match code {
        // MLS Data Sheets
        "ACT" => 0,
        "PEND" => 1,
        "SOLD" => 2,

        // Listing / Purchasing Contracts (residential, commercial, lots,
        // mobile, business). Order intentionally interleaved to match the
        // PDFs, with residential first since it's the most common.
        "RPA" => 100,
        "RIPA" => 101,
        "RLA" => 102,
        "CPA" => 103,
        "CLA" => 104,
        "VLPA" => 105,
        "VLL" => 106,
        "MHPA" => 107,
        "MHLA" => 108,
        "BPA" => 109,
        "BLA" => 110,
        "LR" => 111,
        "LL" => 112,

        // Mandatory Disclosures
        "AVID-1" => 200,
        "AVID-2" => 201,
        "FHDS" => 202,
        "LPD" => 203,
        "RGM" => 204,
        "SBSA" => 205,
        "SPQ" => 206,
        "TDS" => 207,
        "WCMD" => 208,
        "WFDA" => 209,
        "WHSD" => 210,
        "VP" => 211,
        "CSPQ" => 212,
        "MHDA" => 213,
        "MHTDS" => 214,
        "VLQ" => 215,
        "BDS" => 216,

        // Special Conditions Disclosures
        "PLA" => 300,
        "PA" => 301,
        "SSA" => 302,
        "SSLA" => 303,
        "REO" => 304,
        "REOL" => 305,

        // Additional Disclosures
        "AVAA" => 400,
        "BCA" => 401,
        "BRBC" => 402,
        "EQ" => 403,
        "EQ-R" => 404,
        "HID" => 405,
        "MCA" => 406,
        "QUAL" => 407,
        "POF" => 408,
        "BP-FFE" => 409,

        // Disclosures — If Applicable
        "ADM" => 500,
        "CO" => 501,
        "COP" => 502,
        "CR" => 503,
        "ESD" => 504,
        "ETA" => 505,
        "FVAC" => 506,
        "HOA-IR" => 507,
        "MT" => 508,
        "NTP" => 509,
        "RCSD" => 510,
        "RR" => 511,
        "RRRR" => 512,
        "SPRP" => 513,
        "SWPI" => 514,
        "TA" => 515,

        // Escrow Documents
        "APRL" => 600,
        "CC&R" => 601,
        "CLSD" => 602,
        "COMM" => 603,
        "EMD" => 604,
        "EA" => 605,
        "EI" => 606,
        "HOA" => 607,
        "NET" => 608,
        "NHD" => 609,
        "NHDS" => 610,
        "PREL" => 611,

        // Reports, Certificates & Clearances
        "BIW" => 700,
        "CHIM" => 701,
        "HOME" => 702,
        "HPP" => 703,
        "POOL" => 704,
        "ROOF" => 705,
        "SEPT" => 706,
        "SOLAR" => 707,
        "TERM" => 708,
        "WELL" => 709,

        // Release Disclosures
        "CC" => 800,
        "COL" => 801,
        "WOO" => 802,

        // Catch-all bucket
        "MISC" => 900,

        // Unknown CAR form code: drop after every known one but before
        // free-text custom items (which return u32::MAX from the caller).
        _ => 950,
    }
}

// ---------------------------------------------------------------------------
// Master CAR forms library — every code referenced anywhere below MUST appear
// here. Keep alphabetised by code within each thematic block for maintenance.
// ---------------------------------------------------------------------------

/// Look up a form by its code, returning `None` for unknown codes.
pub fn lookup(code: &str) -> Option<&'static CarForm> {
    LIBRARY.iter().find(|f| f.code.eq_ignore_ascii_case(code))
}

/// Full master library, alphabetised. Use [`lookup`] for code-based lookup.
pub const LIBRARY: &[CarForm] = &[
    // MLS data sheets
    CarForm {
        code: "ACT",
        name: "Active Status MLS Full Report",
        description: "Active MLS listing report",
        allows_multiple: true,
    },
    CarForm {
        code: "PEND",
        name: "Pending Status MLS Report",
        description: "Pending MLS listing report",
        allows_multiple: true,
    },
    CarForm {
        code: "SOLD",
        name: "Sold, Canceled or Withdrawn Status MLS Report",
        description: "Sold MLS listing report",
        allows_multiple: true,
    },
    // Listing / Purchasing contracts
    CarForm {
        code: "RPA",
        name: "Residential Purchase Agreement",
        description: "Includes AD, FRR-PA, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "RIPA",
        name: "Residential Income Property Purchase Agreement",
        description: "Includes AD, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "RLA",
        name: "Residential Listing Agreement",
        description: "Includes AD, MLSA, BCA, PRBS, FHDA, SA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "CPA",
        name: "Commercial Property Purchase Agreement",
        description: "Includes AD, FRR-PA, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "CLA",
        name: "Commercial and Residential Income Listing Agreement",
        description: "Includes AD, MLSA, BCA, PRBS & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "VLPA",
        name: "Vacant Land Purchase Agreement",
        description: "Includes AD, FRR-PA, BVLIA, PRBS, FHDA, WFA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "VLL",
        name: "Vacant Land Listing Agreement",
        description: "Includes AD, SLVA, MLSA, BCA, PRBS & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "MHPA",
        name: "Manufactured or Mobile Home Purchase Addendum",
        description: "Mobile-home-specific purchase addendum",
        allows_multiple: false,
    },
    CarForm {
        code: "MHLA",
        name: "Manufactured or Mobile Home Listing Addendum",
        description: "Mobile-home-specific listing addendum",
        allows_multiple: false,
    },
    CarForm {
        code: "BPA",
        name: "Business Purchase Agreement",
        description: "Includes AD, PRBS, FHDA, BHIA, WFA & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "BLA",
        name: "Business Listing Agreement",
        description: "Includes AD, PRBS & CCPA",
        allows_multiple: false,
    },
    CarForm {
        code: "LR",
        name: "Residential Lease or Month-to-Month Rental Agreement",
        description: "Standard residential lease",
        allows_multiple: false,
    },
    CarForm {
        code: "LL",
        name: "Residential Listing Agreement (Lease)",
        description: "Listing agreement for a rental property",
        allows_multiple: false,
    },
    // Mandatory disclosures
    CarForm {
        code: "AVID-1",
        name: "Agent Visual Inspection Disclosure — Listing Agent",
        description: "Listing agent's required visual inspection disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "AVID-2",
        name: "Agent Visual Inspection Disclosure — Selling Agent",
        description: "Selling agent's required visual inspection disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "FHDS",
        name: "Fire Hardening and Defensible Space Advisory",
        description: "Disclosure and addendum for fire-prone areas",
        allows_multiple: false,
    },
    CarForm {
        code: "LPD",
        name: "Lead-Based Paint Hazards Disclosure",
        description: "Required for properties built before 1978",
        allows_multiple: false,
    },
    CarForm {
        code: "RGM",
        name: "Radon Gas and Mold Notice and Release",
        description: "Buyer-only acknowledgement",
        allows_multiple: false,
    },
    CarForm {
        code: "SBSA",
        name: "Statewide Buyer and Seller Advisory",
        description: "Joint statewide disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "SPQ",
        name: "Seller Property Questionnaire",
        description: "Detailed seller-completed property questionnaire",
        allows_multiple: false,
    },
    CarForm {
        code: "TDS",
        name: "Real Estate Transfer Disclosure Statement",
        description: "Required transfer disclosure statement",
        allows_multiple: false,
    },
    CarForm {
        code: "WCMD",
        name: "Water-Conserving Plumbing & Carbon Monoxide Notice",
        description: "Statutory water and CO notice",
        allows_multiple: false,
    },
    CarForm {
        code: "WFDA",
        name: "Wildfire Disaster Advisory",
        description: "Buyer-only wildfire disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "WHSD",
        name: "Water Heater & Smoke Detector Statement of Compliance",
        description: "Statutory compliance statement",
        allows_multiple: false,
    },
    CarForm {
        code: "VP",
        name: "Verification of Property Condition",
        description: "Pre-close property condition verification",
        allows_multiple: false,
    },
    CarForm {
        code: "CSPQ",
        name: "Commercial Seller Property Questionnaire",
        description: "Commercial-property variant of the SPQ",
        allows_multiple: false,
    },
    CarForm {
        code: "MHDA",
        name: "Manufactured Home Dealer Addendum",
        description: "Required when a licensed dealer is involved",
        allows_multiple: false,
    },
    CarForm {
        code: "MHTDS",
        name: "Manufactured Home & Mobile Home Transfer Disclosure Statement",
        description: "Mobile-home specific TDS",
        allows_multiple: false,
    },
    CarForm {
        code: "VLQ",
        name: "Seller Vacant Land Questionnaire",
        description: "Vacant-land specific seller questionnaire",
        allows_multiple: false,
    },
    CarForm {
        code: "BDS",
        name: "Business Disclosure Statement",
        description: "Business-sale specific disclosure",
        allows_multiple: false,
    },
    // Special conditions
    CarForm {
        code: "PLA",
        name: "Probate Listing Addendum",
        description: "Required when listing is part of a probate sale",
        allows_multiple: false,
    },
    CarForm {
        code: "PA",
        name: "Probate Advisory",
        description: "Buyer-side advisory for probate sales",
        allows_multiple: false,
    },
    CarForm {
        code: "SSA",
        name: "Short Sale Addendum",
        description: "Required for short-sale purchase contracts",
        allows_multiple: false,
    },
    CarForm {
        code: "SSLA",
        name: "Short Sale Listing Addendum",
        description: "Required for short-sale listings",
        allows_multiple: false,
    },
    CarForm {
        code: "REO",
        name: "REO Advisory",
        description: "Bank-owned property advisory (purchase side)",
        allows_multiple: false,
    },
    CarForm {
        code: "REOL",
        name: "REO Listing Advisory",
        description: "Bank-owned property advisory (listing side)",
        allows_multiple: false,
    },
    // Additional disclosures
    CarForm {
        code: "AVAA",
        name: "Antelope Valley Disclosure",
        description: "Local Antelope Valley disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "BCA",
        name: "Broker Compensation Advisory",
        description: "Compensation arrangement advisory",
        allows_multiple: false,
    },
    CarForm {
        code: "BRBC",
        name: "Buyer Representation and Broker Compensation Agreement",
        description: "Buyer-side representation contract",
        allows_multiple: false,
    },
    CarForm {
        code: "EQ",
        name: "Earthquake Questionnaire",
        description: "Seismic property questionnaire",
        allows_multiple: false,
    },
    CarForm {
        code: "EQ-R",
        name: "Earthquake Booklet Receipt",
        description: "Buyer receipt for earthquake booklet",
        allows_multiple: false,
    },
    CarForm {
        code: "HID",
        name: "For Your Protection: Get a Home Inspection",
        description: "Buyer-only inspection advisory",
        allows_multiple: false,
    },
    CarForm {
        code: "MCA",
        name: "Market Conditions Advisory",
        description: "Advisory on volatile market conditions",
        allows_multiple: false,
    },
    CarForm {
        code: "QUAL",
        name: "Pre-Qualified / Pre-Approval Letter",
        description: "Lender pre-qual or pre-approval letter",
        allows_multiple: false,
    },
    CarForm {
        code: "POF",
        name: "Proof of Funds",
        description: "Buyer's proof of available funds",
        allows_multiple: false,
    },
    CarForm {
        code: "BP-FFE",
        name: "Business Purchase — Furniture, Fixtures, and Equipment",
        description: "FF&E inventory addendum",
        allows_multiple: true,
    },
    // Disclosures — if applicable
    CarForm {
        code: "ADM",
        name: "Addendum",
        description: "Generic addendum",
        allows_multiple: true,
    },
    CarForm {
        code: "CO",
        name: "Counter Offer",
        description: "Counter-offer to a purchase agreement",
        allows_multiple: true,
    },
    CarForm {
        code: "COP",
        name: "Contingency for Sale of Buyer's Property",
        description: "Sale-of-property contingency",
        allows_multiple: false,
    },
    CarForm {
        code: "CR",
        name: "Contingency Removal",
        description: "Removal of one or more contingencies",
        allows_multiple: true,
    },
    CarForm {
        code: "ESD",
        name: "Exempt Seller Disclosure",
        description: "Seller's exempt-status disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "ETA",
        name: "Extension of Time Amendment",
        description: "Extension of contract timeline",
        allows_multiple: true,
    },
    CarForm {
        code: "FVAC",
        name: "FHA / VA Amendatory Clause",
        description: "Required addendum for FHA/VA financed deals",
        allows_multiple: false,
    },
    CarForm {
        code: "HOA-IR",
        name: "Homeowner Association Information Request",
        description: "HOA document request",
        allows_multiple: false,
    },
    CarForm {
        code: "MT",
        name: "Modification of Terms",
        description: "Mid-contract modification",
        allows_multiple: true,
    },
    CarForm {
        code: "NTP",
        name: "Notice to Perform",
        description: "Notice that other party must perform",
        allows_multiple: true,
    },
    CarForm {
        code: "RCSD",
        name: "Representative Capacity Signature Disclosure",
        description: "Disclosure when signing on behalf of a trust/estate",
        allows_multiple: false,
    },
    CarForm {
        code: "RR",
        name: "Request for Repair",
        description: "Buyer's request to seller for repairs",
        allows_multiple: true,
    },
    CarForm {
        code: "RRRR",
        name: "Seller Response and Buyer Reply to Request for Repair",
        description: "Repair-request reply round",
        allows_multiple: true,
    },
    CarForm {
        code: "SPRP",
        name: "Seller's Purchase of Replacement Property",
        description: "Seller's contingent purchase of a replacement property",
        allows_multiple: false,
    },
    CarForm {
        code: "SWPI",
        name: "Septic, Well, Property Monument & Propane Allocation of Cost",
        description: "Inspection cost allocation addendum",
        allows_multiple: false,
    },
    CarForm {
        code: "TA",
        name: "Trust Advisory",
        description: "Trust-sale advisory",
        allows_multiple: false,
    },
    // Escrow
    CarForm {
        code: "APRL",
        name: "Appraisal Report",
        description: "Lender's appraisal of the property",
        allows_multiple: false,
    },
    CarForm {
        code: "CC&R",
        name: "Covenants, Conditions & Restrictions",
        description: "CC&Rs for the property",
        allows_multiple: false,
    },
    CarForm {
        code: "CLSD",
        name: "Closing Statement / Settlement Sheet",
        description: "Final settlement sheet",
        allows_multiple: false,
    },
    CarForm {
        code: "COMM",
        name: "Commission Instructions",
        description: "Commission disbursement instructions",
        allows_multiple: false,
    },
    CarForm {
        code: "EMD",
        name: "EMD Escrow Receipt",
        description: "Earnest-money deposit receipt",
        allows_multiple: false,
    },
    CarForm {
        code: "EA",
        name: "Escrow Amendments",
        description: "Amendments to escrow instructions",
        allows_multiple: true,
    },
    CarForm {
        code: "EI",
        name: "Escrow Instructions",
        description: "Initial escrow instructions",
        allows_multiple: false,
    },
    CarForm {
        code: "HOA",
        name: "Home Owner Association Documents",
        description: "HOA disclosure packet",
        allows_multiple: false,
    },
    CarForm {
        code: "NET",
        name: "Seller NET Sheet",
        description: "Seller's net proceeds estimate",
        allows_multiple: false,
    },
    CarForm {
        code: "NHD",
        name: "Natural Hazard Disclosure Report",
        description: "Required NHD report",
        allows_multiple: false,
    },
    CarForm {
        code: "NHDS",
        name: "NHD Report Signature Page",
        description: "Signature page for NHD report",
        allows_multiple: false,
    },
    CarForm {
        code: "PREL",
        name: "Preliminary Title Report",
        description: "Title company's preliminary report",
        allows_multiple: false,
    },
    // Reports, certificates & clearances
    CarForm {
        code: "BIW",
        name: "Buyer Investigation Waiver",
        description: "Waiver of investigation contingency",
        allows_multiple: false,
    },
    CarForm {
        code: "CHIM",
        name: "Chimney Inspection Report",
        description: "Chimney/fireplace inspection",
        allows_multiple: false,
    },
    CarForm {
        code: "HOME",
        name: "Home Inspection Report",
        description: "General home inspection report",
        allows_multiple: false,
    },
    CarForm {
        code: "HPP",
        name: "Home Protection Plan",
        description: "Home warranty/protection plan",
        allows_multiple: false,
    },
    CarForm {
        code: "POOL",
        name: "Pool / Spa Inspection",
        description: "Pool or spa inspection report",
        allows_multiple: false,
    },
    CarForm {
        code: "ROOF",
        name: "Roof Inspection / Certification",
        description: "Roof inspection or certification",
        allows_multiple: false,
    },
    CarForm {
        code: "SEPT",
        name: "Septic System Inspection / Certification",
        description: "Septic inspection or certification",
        allows_multiple: false,
    },
    CarForm {
        code: "SOLAR",
        name: "Solar Advisory and Questionnaire",
        description: "Solar-system advisory + questionnaire",
        allows_multiple: false,
    },
    CarForm {
        code: "TERM",
        name: "Termite Inspection Report",
        description: "Termite/wood-destroying organism report",
        allows_multiple: false,
    },
    CarForm {
        code: "WELL",
        name: "Well Inspection Report",
        description: "Domestic well inspection report",
        allows_multiple: false,
    },
    // Release disclosures
    CarForm {
        code: "CC",
        name: "Cancellation of Contract & Release of Deposit",
        description: "Mutual contract cancellation",
        allows_multiple: false,
    },
    CarForm {
        code: "COL",
        name: "Cancellation of Listing",
        description: "Listing cancellation",
        allows_multiple: false,
    },
    CarForm {
        code: "WOO",
        name: "Withdrawal of Offer",
        description: "Buyer's withdrawal of an offer",
        allows_multiple: false,
    },
    // Catch-all
    CarForm {
        code: "MISC",
        name: "Miscellaneous",
        description: "Free-form supporting document",
        allows_multiple: true,
    },
];

// ---------------------------------------------------------------------------
// Default checklists per transaction type. Each tuple = (code, group, required).
// Lifted directly from the official 2026 CAR transaction checklists.
// ---------------------------------------------------------------------------

/// Resolve a sales type to the buyer/listing side(s) it represents. The
/// per-combo defaults below split into "Listing" and "Purchase" arrays;
/// [`SalesSide::Both`] unions them.
#[derive(Clone, Copy)]
enum SalesSide {
    Listing,
    Purchase,
    Both,
}

fn sales_side(sales: SalesType) -> SalesSide {
    match sales {
        // Pure listing-side deals
        SalesType::Listing | SalesType::LeaseLandlord => SalesSide::Listing,
        // Pure buyer/tenant-side deals
        SalesType::Purchase | SalesType::LeaseTenant | SalesType::Referral => SalesSide::Purchase,
        // Dual-representation deals
        SalesType::ListingAndPurchase | SalesType::LeaseTenantAndLandlord => SalesSide::Both,
    }
}

const fn item(code: &'static str, group: FormGroup, required: bool) -> DefaultItem {
    DefaultItem {
        code,
        group,
        required,
    }
}

// ---------------------------------------------------------------------------
// Per-(TransactionType × SalesType) checklist defaults.
//
// Required flags below match the red/green colour-coding in the printed CAR
// checklists under `docs/updated sales type/`. Listing-side and Purchase-side
// arrays are kept separate so each deal type pulls the correct contract,
// disclosures, and escrow paperwork; dual-side deals (Listing & Purchase,
// Lease Tenant & Landlord) merge the two lists with required = (L || P).
// ---------------------------------------------------------------------------

// Residential — Listing
const RESIDENTIAL_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("RLA", FormGroup::ListingPurchasingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("TDS", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("HOA", FormGroup::EscrowDocuments, false),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CHIM", FormGroup::ReportsCertificatesClearances, false),
    item("HOME", FormGroup::ReportsCertificatesClearances, false),
    item("HPP", FormGroup::ReportsCertificatesClearances, false),
    item("POOL", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Residential — Purchase. RPA is the standard contract; RIPA is an
// alternative for income properties (PDF heading: "ONLY ONE IS MANDATORY").
// We mark RPA required and RIPA optional — brokers handling income deals
// can flip RIPA's required flag in the UI.
const RESIDENTIAL_PURCHASE: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("RPA", FormGroup::ListingPurchasingContracts, true),
    item("RIPA", FormGroup::ListingPurchasingContracts, false),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("RGM", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("TDS", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("HID", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("HOA", FormGroup::EscrowDocuments, false),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CHIM", FormGroup::ReportsCertificatesClearances, false),
    item("HOME", FormGroup::ReportsCertificatesClearances, false),
    item("HPP", FormGroup::ReportsCertificatesClearances, false),
    item("POOL", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Commercial — Listing
const COMMERCIAL_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("CLA", FormGroup::ListingPurchasingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("CSPQ", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Commercial — Purchase
const COMMERCIAL_PURCHASE: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("CPA", FormGroup::ListingPurchasingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("CSPQ", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Lots & Land — Listing
const LOTS_LAND_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("VLL", FormGroup::ListingPurchasingContracts, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VLQ", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Lots & Land — Purchase
const LOTS_LAND_PURCHASE: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("VLPA", FormGroup::ListingPurchasingContracts, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VLQ", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Mobile/Manufactured Home — Listing. Both RLA and MHLA are required (the
// listing addendum supplements the standard agreement).
const MOBILE_HOME_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("RLA", FormGroup::ListingPurchasingContracts, true),
    item("MHLA", FormGroup::ListingPurchasingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("MHDA", FormGroup::MandatoryDisclosures, false),
    item("MHTDS", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("HOA", FormGroup::EscrowDocuments, false),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CHIM", FormGroup::ReportsCertificatesClearances, false),
    item("HOME", FormGroup::ReportsCertificatesClearances, false),
    item("HPP", FormGroup::ReportsCertificatesClearances, false),
    item("POOL", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Mobile/Manufactured Home — Purchase. RPA + MHPA (both required as a pair).
const MOBILE_HOME_PURCHASE: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("RPA", FormGroup::ListingPurchasingContracts, true),
    item("MHPA", FormGroup::ListingPurchasingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("RGM", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("MHDA", FormGroup::MandatoryDisclosures, false),
    item("MHTDS", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("HID", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("HOA", FormGroup::EscrowDocuments, false),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CHIM", FormGroup::ReportsCertificatesClearances, false),
    item("HOME", FormGroup::ReportsCertificatesClearances, false),
    item("HPP", FormGroup::ReportsCertificatesClearances, false),
    item("POOL", FormGroup::ReportsCertificatesClearances, false),
    item("ROOF", FormGroup::ReportsCertificatesClearances, false),
    item("SEPT", FormGroup::ReportsCertificatesClearances, false),
    item("SOLAR", FormGroup::ReportsCertificatesClearances, false),
    item("TERM", FormGroup::ReportsCertificatesClearances, false),
    item("WELL", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Business Opportunity — Listing
const BUSINESS_OP_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("BLA", FormGroup::ListingPurchasingContracts, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BDS", FormGroup::AdditionalDisclosures, false),
    item("BP-FFE", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Business Opportunity — Purchase
const BUSINESS_OP_PURCHASE: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("BPA", FormGroup::ListingPurchasingContracts, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BDS", FormGroup::AdditionalDisclosures, false),
    item("BP-FFE", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Multi-Family — no separate Listing/Purchase PDFs yet; falls back to a
// combined checklist similar to the original Multi-Family default. This
// gets used regardless of sales side until per-side PDFs land.
const MULTI_FAMILY_FALLBACK: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("SOLD", FormGroup::MlsDataSheets, true),
    item("RIPA", FormGroup::ListingPurchasingContracts, true),
    item("CLA", FormGroup::ListingPurchasingContracts, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("RGM", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("APRL", FormGroup::EscrowDocuments, true),
    item("CC&R", FormGroup::EscrowDocuments, false),
    item("CLSD", FormGroup::EscrowDocuments, true),
    item("COMM", FormGroup::EscrowDocuments, true),
    item("EMD", FormGroup::EscrowDocuments, true),
    item("EA", FormGroup::EscrowDocuments, false),
    item("EI", FormGroup::EscrowDocuments, true),
    item("HOA", FormGroup::EscrowDocuments, false),
    item("NET", FormGroup::EscrowDocuments, false),
    item("NHD", FormGroup::EscrowDocuments, true),
    item("NHDS", FormGroup::EscrowDocuments, true),
    item("PREL", FormGroup::EscrowDocuments, true),
    item("BIW", FormGroup::ReportsCertificatesClearances, false),
    item("CC", FormGroup::ReleaseDisclosures, false),
    item("COL", FormGroup::ReleaseDisclosures, false),
    item("WOO", FormGroup::ReleaseDisclosures, false),
];

// Special-condition extras vary by side: listing-side gets the listing-
// specific addenda (PLA, SSLA, REOL); purchase-side gets the buyer-side
// advisories (PA, SSA, REO). Dual-side merges both.
const PROBATE_LISTING: &[DefaultItem] =
    &[item("PLA", FormGroup::SpecialConditionsDisclosures, true)];
const PROBATE_PURCHASE: &[DefaultItem] =
    &[item("PA", FormGroup::SpecialConditionsDisclosures, true)];
const SHORT_SALE_LISTING: &[DefaultItem] = &[
    item("SSA", FormGroup::SpecialConditionsDisclosures, true),
    item("SSLA", FormGroup::SpecialConditionsDisclosures, true),
];
const SHORT_SALE_PURCHASE: &[DefaultItem] =
    &[item("SSA", FormGroup::SpecialConditionsDisclosures, true)];
const REO_LISTING: &[DefaultItem] = &[
    item("REO", FormGroup::SpecialConditionsDisclosures, true),
    item("REOL", FormGroup::SpecialConditionsDisclosures, true),
];
const REO_PURCHASE: &[DefaultItem] = &[item("REO", FormGroup::SpecialConditionsDisclosures, true)];

fn special_condition_items(c: SpecialSalesCondition, side: SalesSide) -> Vec<DefaultItem> {
    let listing: &[DefaultItem] = match c {
        SpecialSalesCondition::None => &[],
        SpecialSalesCondition::Probate => PROBATE_LISTING,
        SpecialSalesCondition::ShortSale => SHORT_SALE_LISTING,
        SpecialSalesCondition::REO => REO_LISTING,
    };
    let purchase: &[DefaultItem] = match c {
        SpecialSalesCondition::None => &[],
        SpecialSalesCondition::Probate => PROBATE_PURCHASE,
        SpecialSalesCondition::ShortSale => SHORT_SALE_PURCHASE,
        SpecialSalesCondition::REO => REO_PURCHASE,
    };
    match side {
        SalesSide::Listing => listing.to_vec(),
        SalesSide::Purchase => purchase.to_vec(),
        SalesSide::Both => merge_sides(listing, purchase),
    }
}

/// Pick the per-(type, side) checklist, returning a fresh `Vec` so the
/// caller can mutate without affecting the static arrays.
fn defaults_for(t: TransactionType, side: SalesSide) -> Vec<DefaultItem> {
    match (t, side) {
        (TransactionType::Residential | TransactionType::RentalLease, SalesSide::Listing) => {
            RESIDENTIAL_LISTING.to_vec()
        }
        (TransactionType::Residential | TransactionType::RentalLease, SalesSide::Purchase) => {
            RESIDENTIAL_PURCHASE.to_vec()
        }
        (TransactionType::Residential | TransactionType::RentalLease, SalesSide::Both) => {
            merge_sides(RESIDENTIAL_LISTING, RESIDENTIAL_PURCHASE)
        }

        (TransactionType::Commercial | TransactionType::CommercialLease, SalesSide::Listing) => {
            COMMERCIAL_LISTING.to_vec()
        }
        (TransactionType::Commercial | TransactionType::CommercialLease, SalesSide::Purchase) => {
            COMMERCIAL_PURCHASE.to_vec()
        }
        (TransactionType::Commercial | TransactionType::CommercialLease, SalesSide::Both) => {
            merge_sides(COMMERCIAL_LISTING, COMMERCIAL_PURCHASE)
        }

        (TransactionType::VacantLotsLand, SalesSide::Listing) => LOTS_LAND_LISTING.to_vec(),
        (TransactionType::VacantLotsLand, SalesSide::Purchase) => LOTS_LAND_PURCHASE.to_vec(),
        (TransactionType::VacantLotsLand, SalesSide::Both) => {
            merge_sides(LOTS_LAND_LISTING, LOTS_LAND_PURCHASE)
        }

        (TransactionType::ManufacturedHome, SalesSide::Listing) => MOBILE_HOME_LISTING.to_vec(),
        (TransactionType::ManufacturedHome, SalesSide::Purchase) => MOBILE_HOME_PURCHASE.to_vec(),
        (TransactionType::ManufacturedHome, SalesSide::Both) => {
            merge_sides(MOBILE_HOME_LISTING, MOBILE_HOME_PURCHASE)
        }

        (TransactionType::BusinessOpportunity, SalesSide::Listing) => BUSINESS_OP_LISTING.to_vec(),
        (TransactionType::BusinessOpportunity, SalesSide::Purchase) => {
            BUSINESS_OP_PURCHASE.to_vec()
        }
        (TransactionType::BusinessOpportunity, SalesSide::Both) => {
            merge_sides(BUSINESS_OP_LISTING, BUSINESS_OP_PURCHASE)
        }

        // Multi-Family: no per-side PDFs yet; serve the combined fallback
        // for every sales type so the broker still gets a usable checklist.
        (TransactionType::MultiFamily, _) => MULTI_FAMILY_FALLBACK.to_vec(),
    }
}

/// Combine two side-specific checklists for dual-representation deals.
/// A code that appears on both sides keeps the listing-side group + the
/// OR of both required flags (so anything mandatory on either side stays
/// mandatory in the merged output).
fn merge_sides(listing: &[DefaultItem], purchase: &[DefaultItem]) -> Vec<DefaultItem> {
    let mut out: Vec<DefaultItem> = listing.to_vec();
    for p in purchase {
        if let Some(existing) = out.iter_mut().find(|d| d.code == p.code) {
            existing.required = existing.required || p.required;
        } else {
            out.push(*p);
        }
    }
    out
}

/// Build the full default checklist for a transaction, including the
/// special-condition addenda layered on top.
pub fn build_default_checklist(
    t: TransactionType,
    cond: SpecialSalesCondition,
    sales: SalesType,
) -> Vec<DefaultItem> {
    let side = sales_side(sales);
    let mut out = defaults_for(t, side);
    out.extend(special_condition_items(cond, side));
    out
}
