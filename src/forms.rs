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
    /// Short hint describing what the form is for. Carried on the
    /// struct so callers can surface it as a tooltip or aria-description
    /// in the future; currently unused after we removed the inline
    /// subtitle on the transaction show page.
    #[allow(dead_code)]
    pub description: &'static str,
    pub allows_multiple: bool,
}

impl CarForm {
    /// For the main contract forms only, the mandatory disclosures CAR
    /// bundles automatically. Rendered as a second line under the
    /// contract on the checklist so agents know those forms are
    /// already covered. Empty for every non-contract form — the
    /// template only shows the line when this is non-empty.
    pub fn includes(&self) -> &'static str {
        match self.code {
            "RLA" => "Includes AD, MLSA, BCA, PRBS, FHDA, SA & CCPA",
            "RPA" => "Includes AD, FRR-PA, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
            "RIPA" => "Includes AD, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
            "VLL" => "Includes AD, SLVA, MLSA, BCA, PRBS & CCPA",
            "VLPA" => "Includes AD, FRR-PA, BVLIA, PRBS, FHDA, WFA & CCPA",
            "CLA" => "Includes AD, MLSA, BCA, PRBS & CCPA",
            "CPA" => "Includes AD, FRR-PA, BIA, PRBS, FHDA, BHIA, WFA & CCPA",
            "BLA" => "Includes AD, PRBS & CCPA",
            "BPA" => "Includes AD, PRBS, FHDA, BHIA, WFA & CCPA",
            "LL" => "Includes RPOD, RPOA, FHDA & CCPA",
            "RLMM" => "Includes BBD, TFHD, RCJC, TRPR & FHDA",
            "LRA" => "Includes BIRN",
            _ => "",
        }
    }
}

/// One section in a checklist — these match the headings in CAR's printed
/// transaction checklists exactly.
///
/// The contracts section splits into four variants: a singular pair
/// (`ListingContract` / `PurchaseContract`) for checklists with a single
/// main contract form, and a plural pair (`ListingContracts` /
/// `PurchaseContracts`) for those that bundle two (Residential Purchase =
/// RPA + RIPA, Manufactured Home Listing = RLA + MHLA, Manufactured Home
/// Purchase = RPA + MHPA). Special-condition addenda (PLA, PA, SSA, SSLA,
/// REO, REOL) now live in the same contract group rather than in a
/// dedicated section — they become part of the contract once triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormGroup {
    MlsDataSheets,
    ListingContract,
    ListingContracts,
    PurchaseContract,
    PurchaseContracts,
    /// Lease checklists split their contracts differently from sales:
    /// the landlord-side listing agreement (LL) and the lease itself
    /// (RLMM) each get their own printed section.
    LeaseListingContract,
    RentalContract,
    /// The referral data sheet is a single section holding the RFA.
    ReferralContract,
    MandatoryDisclosures,
    AdditionalDisclosures,
    EscrowDocuments,
    /// Lease checklists: the application / credit / deposit paperwork
    /// ("Application, Receipts & Reports" on the printed sheet).
    ApplicationReceiptsReports,
    /// Lease checklists: CC&Rs, HOA docs, and Rules & Regulations.
    /// Note CC&R/HOA live under Escrow Documents on SALE checklists —
    /// the DB seed keys forms by (code, group) so both placements
    /// coexist as separate rows with disjoint applicability.
    GoverningDocuments,
    ReportsCertificatesClearances,
    ReleaseDisclosures,
}

impl FormGroup {
    /// Render order — matches every printed CAR checklist. The four
    /// contract variants occupy the same slot; only the one(s) used by a
    /// given checklist will have items, so the empties drop out of the
    /// rendered output.
    pub const ORDERED: [FormGroup; 15] = [
        FormGroup::MlsDataSheets,
        FormGroup::ListingContract,
        FormGroup::ListingContracts,
        FormGroup::PurchaseContract,
        FormGroup::PurchaseContracts,
        FormGroup::LeaseListingContract,
        FormGroup::RentalContract,
        FormGroup::ReferralContract,
        FormGroup::MandatoryDisclosures,
        FormGroup::AdditionalDisclosures,
        FormGroup::EscrowDocuments,
        FormGroup::ApplicationReceiptsReports,
        FormGroup::GoverningDocuments,
        FormGroup::ReportsCertificatesClearances,
        FormGroup::ReleaseDisclosures,
    ];

    /// Canonical (group name, sort order) for the DB forms engine. The
    /// in-memory engine splits contracts into singular/plural variants
    /// (a presentational nicety that depends on transaction type, so a
    /// form can't belong to a fixed one); the DB model collapses each
    /// pair into a single "Listing Contracts" / "Purchase Contracts"
    /// group so every form maps to exactly one `form_group`.
    pub fn seed_group(self) -> (&'static str, i64) {
        match self {
            FormGroup::MlsDataSheets => ("MLS Data Sheets", 0),
            FormGroup::ListingContract | FormGroup::ListingContracts => ("Listing Contracts", 1),
            FormGroup::PurchaseContract | FormGroup::PurchaseContracts => ("Purchase Contracts", 2),
            // Lease + referral contract sections. Orders collide with the
            // sale contract groups on purpose — a checklist only ever
            // renders one family, so the slot is shared.
            FormGroup::LeaseListingContract => ("Lease Listing Contract", 1),
            FormGroup::RentalContract => ("Rental Contract", 2),
            FormGroup::ReferralContract => ("Referral Contract", 1),
            FormGroup::MandatoryDisclosures => ("Mandatory Disclosures", 3),
            FormGroup::AdditionalDisclosures => ("Additional Disclosures", 4),
            FormGroup::EscrowDocuments => ("Escrow Documents", 5),
            FormGroup::ApplicationReceiptsReports => ("Application, Receipts & Reports", 5),
            FormGroup::GoverningDocuments => ("Governing Documents", 6),
            FormGroup::ReportsCertificatesClearances => ("Reports, Certificates & Clearances", 6),
            FormGroup::ReleaseDisclosures => ("Release Disclosures", 7),
        }
    }

    // Note: the old `label()` / `slug()` / `parse()` display helpers
    // were removed in the DB-forms-engine cutover — checklist items now
    // carry their group's display name + order directly (set by the
    // seeder / resolver), so `FormGroup` survives only as the seed
    // source (`seed_group`, `ORDERED`, `contract_group_for`).
}

/// Smart-defaults entry: which form, in which group, required-or-not.
#[derive(Debug, Clone, Copy)]
pub struct DefaultItem {
    pub code: &'static str,
    pub group: FormGroup,
    pub required: bool,
}

/// Best-guess of which checklist group a form belongs in, given only
/// its code. Used by the manual Add-an-item path (where the sales side
/// is unknown, so listing-side codes go to `ListingContract` and
/// purchase-side codes to `PurchaseContract`) and by the catalog
/// backfill seeder (placing picker-only forms into the master set's
/// standard groups). Anything that doesn't match a known bucket falls
/// into Additional Disclosures — the catch-all for optional supporting
/// forms.
pub fn infer_group_from_code(code: &str) -> FormGroup {
    match code {
        // Listing-side contracts
        "RLA" | "MHLA" | "VLL" | "CLA" | "BLA" => FormGroup::ListingContract,

        // Purchase-side contracts
        "RPA" | "RIPA" | "MHPA" | "VLPA" | "CPA" | "BPA" | "LR" => FormGroup::PurchaseContract,

        // Lease + referral contract sections
        "LL" => FormGroup::LeaseListingContract,
        "RLMM" => FormGroup::RentalContract,
        "RFA" => FormGroup::ReferralContract,

        // Mandatory disclosures
        "AD" | "AVID-1" | "AVID-2" | "FHDS" | "LCA" | "LPD" | "RGM" | "SBSA" | "SPQ" | "TDS"
        | "WCMD" | "WFDA" | "WHSD" | "VP" | "CSPQ" | "MHDA" | "MHTDS" | "VLQ" => {
            FormGroup::MandatoryDisclosures
        }

        // Special-condition addenda — part of the contract section.
        // Listing-side: under the listing contract. Purchase-side: under
        // the purchase contract.
        "PLA" | "SSLA" | "REOL" => FormGroup::ListingContract,
        "PA" | "SSA" | "REO" => FormGroup::PurchaseContract,

        // MLS sheets
        "ACT" | "PEND" | "SOLD" | "RNTD" => FormGroup::MlsDataSheets,

        // Lease application / deposit paperwork + governing docs
        "CCR" | "LRA" | "SDR" => FormGroup::ApplicationReceiptsReports,
        "R&R" => FormGroup::GoverningDocuments,

        // Escrow
        "APRL" | "CC&R" | "CLSD" | "COMM" | "EMD" | "EA" | "EI" | "HOA" | "NET" | "NHD"
        | "NHDS" | "PREL" => FormGroup::EscrowDocuments,

        // Reports & clearances
        "BIW" | "CHIM" | "HOME" | "HPP" | "POOL" | "ROOF" | "SEPT" | "SOLAR" | "TERM" | "WELL" => {
            FormGroup::ReportsCertificatesClearances
        }

        // Release
        "CC" | "CLR" | "COL" | "WOO" => FormGroup::ReleaseDisclosures,

        // Everything else lands in Additional Disclosures (the catch-all
        // for optional supporting forms — ADM, BCA, BDS, BP-FFE, BRBC,
        // CO, CR, EQ, EQ-R, ETA, FVAC, HID, MCA, MT, NTP, POF, QUAL,
        // RCSD, RR, RRRR, SWPI, TA, plus any custom code).
        _ => FormGroup::AdditionalDisclosures,
    }
}

/// Canonical PDF-order index for a form code within its group. The order
/// here mirrors the printed CAR transaction checklists exactly so the UI
/// always renders items in the expected sequence — independent of when
/// each item was attached to the transaction.
///
/// Codes not in this map sort after every known code, in alphabetical
/// order by their slug. Custom (non-CAR) items sort at the very end.
pub fn canonical_position(code: &str) -> u32 {
    // The integer returned is the code's rank for sort order. Items
    // sharing a group are sorted by these numbers, so within each section
    // we assign sequential values in the order the user should see them.
    //
    // Per-section ordering rationale:
    // - **Contracts (100–119, 150–159):** main contract forms occupy the
    //   100s; special-condition addenda (PLA/SSLA/REOL/PA/SSA/REO)
    //   occupy 150+ so they always render directly below the main
    //   contract within the same group.
    // - **Mandatory & Additional (200s, 400s):** strict alphabetical so
    //   agents can scan visually. This satisfies the printed-checklist
    //   notes "CSPQ underneath AVID-2" and "MHDA & MHTDS underneath LPD"
    //   naturally — they fall in the right slot when alphabetized.
    match code {
        // MLS Data Sheets
        "ACT" => 0,
        "PEND" => 1,
        "SOLD" => 2,
        "RNTD" => 3,

        // Contracts (listing-side main forms — 100–109)
        "RLA" => 100,
        "MHLA" => 101,
        "VLL" => 102,
        "CLA" => 103,
        "BLA" => 104,
        "LL" => 105,
        // Contracts (purchase-side main forms — 110–119)
        "RPA" => 110,
        "RIPA" => 111,
        "MHPA" => 112,
        "VLPA" => 113,
        "CPA" => 114,
        "BPA" => 115,
        "LR" => 116,
        // Lease + referral contract sections (each form is alone in its
        // group, so the exact value only needs to be stable).
        "RLMM" => 120,
        "RFA" => 121,
        // Special-condition addenda — always render right under the
        // main contract form within the same group. Listing-side: 150+;
        // Purchase-side: 155+.
        "PLA" => 150,
        "SSLA" => 151,
        "REOL" => 152,
        "PA" => 155,
        "SSA" => 156,
        "REO" => 157,

        // Mandatory Disclosures — alphabetical
        "AD" => 199,
        "AVID-1" => 200,
        "AVID-2" => 201,
        "CSPQ" => 202,
        "FHDS" => 203,
        "LCA" => 204,
        "LPD" => 205,
        "MHDA" => 206,
        "MHTDS" => 207,
        "RGM" => 208,
        "SBSA" => 209,
        "SPQ" => 210,
        "TDS" => 211,
        "VLQ" => 212,
        "VP" => 213,
        "WCMD" => 214,
        "WFDA" => 215,
        "WHSD" => 216,

        // Additional Disclosures — strictly alphabetical across every
        // code that any per-type list places in this group (the lease
        // sheet's "Disclosures — If Applicable" section shares it).
        "ADM" => 400,
        "BCA" => 401,
        "BDS" => 402,
        "BIRN" => 403,
        "BP-FFE" => 404,
        "BRBC" => 405,
        "CO" => 406,
        "CR" => 407,
        "DRA" => 408,
        "EL" => 409,
        "EQ" => 410,
        "EQ-R" => 411,
        "ETA" => 412,
        "FEHN" => 413,
        "FVAC" => 414,
        "HID" => 415,
        "HOA-IR" => 416,
        "MCA" => 417,
        "MII" => 418,
        "MOI" => 419,
        "MT" => 420,
        "NOE" => 421,
        "NRI" => 422,
        "NTP" => 423,
        "PMA" => 424,
        "PMOI" => 425,
        "POF" => 426,
        "PRBS-B" => 427,
        "PRBS-S" => 428,
        "PRQ" => 429,
        "QUAL" => 430,
        "RCSD" => 431,
        "RR" => 432,
        "RRRR" => 433,
        "SWPI" => 434,
        "SWPI-C" => 435,
        "SWPI-Q" => 436,
        "TA" => 437,

        // Escrow Documents — alphabetical
        "APRL" => 600,
        "CC&R" => 601,
        "CLSD" => 602,
        "COMM" => 603,
        "EA" => 604,
        "EI" => 605,
        "EMD" => 606,
        "HOA" => 607,
        "NET" => 608,
        "NHD" => 609,
        "NHDS" => 610,
        "PREL" => 611,

        // Reports, Certificates & Clearances — alphabetical
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

        // Governing Documents (lease checklists). CC&R and HOA reuse
        // their Escrow ranks (601/607) — the relative order inside this
        // group stays alphabetical, and ranks only compare within a
        // group on one checklist.
        "R&R" => 652,

        // Application, Receipts & Reports (lease checklists)
        "CCR" => 750,
        "LRA" => 751,
        "SDR" => 752,

        // Release Disclosures
        "CC" => 800,
        "CLR" => 801,
        "COL" => 802,
        "WOO" => 803,

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
    CarForm {
        code: "RNTD",
        name: "Rented Status MLS Report",
        description: "Rented MLS listing report",
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
    // Lease checklist items (2026 lease data sheet). AD/CCR/SDR/R&R
    // were previously only broker-added custom forms; the printed
    // Commercial Lease + Rental/Lease sheet makes them library canon.
    CarForm {
        code: "AD",
        name: "Agency Disclosure",
        description: "Agency relationship disclosure",
        allows_multiple: false,
    },
    CarForm {
        code: "CCR",
        name: "Credit Check / Credit Report",
        description: "Applicant credit report",
        allows_multiple: true,
    },
    CarForm {
        code: "SDR",
        name: "Security Deposit Receipt",
        description: "Receipt for the tenant's security deposit",
        allows_multiple: false,
    },
    CarForm {
        code: "R&R",
        name: "Rules & Regulations",
        description: "Property or HOA rules and regulations",
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
    // June 2026 releases: CAR split SWPI into a cost-allocation addendum
    // (SWPI-C) and a standalone questionnaire (SWPI-Q), and split the
    // PRBS disclosure into buyer (PRBS-B) and seller (PRBS-S) consent
    // variants.
    CarForm {
        code: "SWPI-C",
        name: "Septic, Well, Propane Tank, and Property Boundaries Inspection and Allocation of Costs Addendum",
        description: "Inspection cost allocation addendum",
        allows_multiple: false,
    },
    CarForm {
        code: "SWPI-Q",
        name: "Septic, Well, and Propane Tank Questionnaire",
        description: "Seller-completed systems questionnaire",
        allows_multiple: false,
    },
    CarForm {
        code: "PRBS-B",
        name: "Disclosure and Buyer Consent to Possible Representation of More Than One Buyer, and Dual Agency in a Transaction",
        description: "Buyer consent to possible multiple representation",
        allows_multiple: false,
    },
    CarForm {
        code: "PRBS-S",
        name: "Disclosure and Seller Consent to Possible Representation of More Than One Seller, and Dual Agency in a Transaction",
        description: "Seller consent to possible multiple representation",
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
    // ---------------------------------------------------------------------------
    // Full CAR 2026 forms catalog — imported from `All CAR 2026 Forms.pdf`.
    // Descriptions are intentionally blank for these entries; the form name
    // alone is enough for the dropdown picker, and we don't have a per-form
    // narrative to seed the meta-line under each item title.
    // ---------------------------------------------------------------------------
    CarForm {
        code: "A-1",
        name: "Arbitration Complaint",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AAA",
        name: "Additional Agent Acknowledgement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AB",
        name: "Buyer's Affidavit",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ABA",
        name: "Additional Broker Acknowledgement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ABSPA",
        name: "Already-Built Subdivision Purchase Agreement and Joint Escrow Instruction",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AC",
        name: "Confirmation Real Estate Agency Relationships",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ACS",
        name: "Agent Commission Sharing Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AD-1",
        name: "Disclosure Regarding Real Estate Agency Relationship (Seller's Brokerage Firm to Seller)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AD-2",
        name: "Disclosure Regarding Real Estate Agency Relationship (Buyer's Brokerage Firm to Buyer)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AD-3",
        name: "Disclosure Regarding Real Estate Agency Relationship (Generic)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ADM-GEN",
        name: "Addendum - Generic",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "AEA",
        name: "Amendment of Existing Agreement Terms",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "AFA",
        name: "Assumed Financing Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AGAD",
        name: "Agricultural Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AOAA",
        name: "Assignment of Agreement Addendum",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "APD",
        name: "Amendment to Prior Disclosure",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "ARB",
        name: "Arbitration Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ARC",
        name: "Authorization to Receive and Convey Information",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ASA",
        name: "Additional Signature Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AS-S",
        name: "Seller's Affidavit of Nonforeign Status (FIRPTA)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ATCA",
        name: "Animal Terms and Conditions Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "AVID",
        name: "Agent Visual Inspection Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BBD",
        name: "Bed Bug Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BCO",
        name: "Buyer Counter Offer",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "BEO",
        name: "Buyer Early Occupancy Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BFPI",
        name: "Buyer Financial and Personal Information",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BHAA",
        name: "Buyers Homeowner's Association Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BHIA",
        name: "Buyer Homeowner's Insurance Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BIA",
        name: "Buyer's Investigation Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BIE",
        name: "Buyer's Investigation Elections",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BIPP",
        name: "Buyer Identification of Preferences and Priorities",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BIRN",
        name: "Notice Regarding Background Investigation Reports Pursuant to California Law",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BMI-SP",
        name: "Buyer Material Issues for a Specific Property",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BNA",
        name: "Buyer Non-Agency Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BOS",
        name: "Bill of Sale",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BP-APP",
        name: "Business Purchase - Allocation of Purchase Price",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BP-ECET",
        name: "Business Purchase - Employee Certificate of Employment Terms",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BUO",
        name: "Back-Up Offer Addendum",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "BVLIA",
        name: "Buyer's Vacant Land Additional Inspection Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "BXA",
        name: "Buyer's Intent to Exchange Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CA",
        name: "Compensation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CAC",
        name: "Cancellation of Agency Confirmation; Amendment to Purchase Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CCA",
        name: "Court Confirmation Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CCPA",
        name: "California Consumer Privacy Act Advisory, Disclosure and Notice",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CCSPA",
        name: "Condominium Conversion Subdivision Purchase Agreement & Joint Escrow Instructions",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CEEI",
        name: "Condominium Conversion & Existing Supplemental Escrow Instructions",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CFC",
        name: "Consent for Communications",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CFK",
        name: "Cash for Keys Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CL",
        name: "Commercial Lease Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CLR",
        name: "Cancellation of Lease or Rent",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CML-CNDA",
        name: "Commercial Confidentiality and Non-Disclosure Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CML-EIA",
        name: "Commercial - Environmental Issues Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CML-LEC",
        name: "Commercial - Landlord's Environmental Consent",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CML-REL",
        name: "Commercial Release Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CNC-PA",
        name: "Completed New Construction - Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "COBR",
        name: "Cancellation of Buyer Representation",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CONDEF",
        name: "Seller's Disclosure of the Existence of Construction Defect Claim",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "COOP-OA",
        name: "Stock Cooperative Ownership Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "COOP-PA",
        name: "Stock Cooperative Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CR-B1",
        name: "Buyer Contingency Removal",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CR-S",
        name: "Seller Contingency Removal",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "CTT",
        name: "Notice of Change in Terms of Tenancy",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "D-1",
        name: "Disciplinary Complaint",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DBD",
        name: "Megans Law Data Base Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DCE",
        name: "Demand to Close Escrow",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DEDA",
        name: "Designated Electronic Delivery Address Amendment",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DIA",
        name: "Disclosure Information Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DID",
        name: "Delivery of Increased Deposit and Liquidated Damages Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DLT",
        name: "Declaration Regarding Real Estate License and Tax Reporting",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DM",
        name: "Demand for Mediation",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DRA",
        name: "Denial of Rental Application for Credit Reasons",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DSDT",
        name: "Defensible Space Decision Tree",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "DSSC",
        name: "Delivery of or Failure to Deliver Short Sale Lender Written Notice",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "EBC",
        name: "Estimated Buyer Costs",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ECC-B",
        name: "Estimated Compensation Costs for Buyer",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ECC-S",
        name: "Estimated Compensation Costs for Seller",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "EL",
        name: "Extension of Lease",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ESP",
        name: "Estimated Seller Proceeds",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ESV",
        name: "Electronic Signature Verification for Third Parties",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "FEHN",
        name: "48-Hour Notice of Inspection Prior to Termination of Tenancy",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "FHDA",
        name: "Fair Housing & Discrimination Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "FLTN",
        name: "Notice of Right to Receive Foreign Language Translation of Lease / Rental Agreements",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "FRR-PA",
        name: "Federal Reporting Requirement Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "HOA-RN",
        name: "Homeowner Association Request for Non-Statutory Documents, Other Information, and Charges",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "HOA-RS",
        name: "Homeowner Association Request for Required Statutory Documents and Charges",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ICA",
        name: "Independent Contractor Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "IDA",
        name: "Increased Deposit Addendum",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "IOA",
        name: "Interim Occupancy Agreement (Buyer in Possession Prior to Close of Escrow)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "ITA",
        name: "Interpreter / Translator Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "KLA",
        name: "Keysafe / Lockbox Addendum and Tenant Permission to Access Property",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LBSB",
        name: "Loan Broker-Sales Broker Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LCA",
        name: "Lease / Rental Compensation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LDAA",
        name: "Liquidated Damages and Arbitration Additional Signature Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LPD-RLAS",
        name: "Lead-Based Paint and Lead-Based Paint Hazards Disclosure (RLAS)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LRA",
        name: "Application to Lease or Rent /Screening Fee",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "LRM",
        name: "Lease / Rental Mold and Ventilation Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MARSMRN",
        name: "Mortgage Assistance Relief Services Offer of Mortgage Relief Notice",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MARSSN",
        name: "Mortgage Assistance Relief Services Short Sale Negotiation Notice",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MCN",
        name: "Methamphetamine Contamination Notice",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MH-PA",
        name: "Manufactured or Mobile Home Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MII",
        name: "Move In Inspection",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "MLSA",
        name: "Multiple Listing Service Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MOI",
        name: "Move Out Inspection",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "MSS",
        name: "Mortgage Loan Disclosure Statement Substitute",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MT-BR",
        name: "Modification of Terms - Buyer Representation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MT-LA",
        name: "Modification of Terms - Listing Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "MU-PA",
        name: "Mixed Use Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NBIP",
        name: "Notice of Broker Involved Properties",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NBP",
        name: "Notice to Buyer to Perform",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "NCA",
        name: "New Construction Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NCEI",
        name: "Common Interest Subdivision Supplemental Escrow Instructions",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NCNC",
        name: "New Construction Notice of Completion and Notice to Close Escrow",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NCOA",
        name: "Non-Contingent Offer Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NCOU",
        name: "Options and Upgrades Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NCPA",
        name: "New Construction Purchase Agreement and Joint Escrow Instructions",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NDA",
        name: "Confidentiality and Non-Disclosure Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NNR",
        name: "Notice of Nonresponsibility",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NODPA",
        name: "Notice of Default Purchase Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NOE",
        name: "Notice of Entry",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NPB",
        name: "Notice of Prospective Buyers / Transferees",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NPC",
        name: "Notice of Obligation to Pay Rental or Lease Payments in Cash",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NRI",
        name: "Notice of Right to Inspection Prior to Termination of Tenancy",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NSE",
        name: "Notice of Sale and Entry",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NSF",
        name: "Use of Non-Standard Forms Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NSP",
        name: "Notice to Seller to Perform",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "NTF",
        name: "Notice of Private Transfer Fee",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NTQ",
        name: "Notice to Quit",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NTT",
        name: "Notice of Termination of Tenancy",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NTT-FM",
        name: "Family Move-In Disclosure and Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "NTT-RD",
        name: "Substantial Remodel or Demolition Disclosure and Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "OA",
        name: "Option Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "OHNA-SI",
        name: "Open House Visitor Non-Agency and Sign In",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "OMA",
        name: "Office Management Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PAC",
        name: "Personal Assistant Contract",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PA-PA",
        name: "Probate Agreement Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PCQ",
        name: "Notice to Cure; or Perform Covenant or Quit",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PHSA",
        name: "Pool, Hot Tub and Spa Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PIA",
        name: "Property Images Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PMA",
        name: "Property Management Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PMOI",
        name: "Pre-Move Out Inspection Statement",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "POSA",
        name: "Buyer Pre-Occupancy Storage Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PPN",
        name: "Pre-Possession Notice to Tenant to Pay",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PRBS",
        name: "Possible Representation of More than One Buyer or Seller - Disclosure and Consent",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PRQ",
        name: "Notice to Pay Rent or Quit",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PSD",
        name: "Parking and Storage Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PSRA",
        name: "Property Showing and Representation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PVOH",
        name: "Property Visit and Open House Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "PVR",
        name: "Photo and Video Agreement and Release",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "QS",
        name: "Qualified Substitute Declaration of Possession of Transferor's Affidavit of Nonforeign Status",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RA",
        name: "REALTOR'S Acknowledgement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RAD",
        name: "Realtor Acknowledgement and Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RCJC",
        name: "Rent Cap and Just Cause Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RCSD-B",
        name: "Representative Capacity Signature Disclosure (For Buyer Representatives)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RCSD-HP",
        name: "Representative Capacity Signature Disclosure (For Housing Provider Representative)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RCSD-S",
        name: "Representative Capacity Signature Disclosure (For Seller Representatives)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RCSD-T",
        name: "Representative Capacity Signature Disclosure (For Tenant Representatives)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RFA",
        name: "Referral Fee Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RFR",
        name: "Receipt for Reports",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RLAN",
        name: "Residential Listing Agreement - Open",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RLAS",
        name: "Residential Lease After Sale (Seller in Possession After Close of Escrow)",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RLASR",
        name: "Residential Listing Agreement - Seller Reserved",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RLMM",
        name: "Residential Lease / Month-to-Month Rental Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RPOA",
        name: "Rental Property Owner Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RPOD",
        name: "Rental Property Owner Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RPOI",
        name: "Rental Property Owner Intake Form",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "RU-PA",
        name: "Residential Units Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SA",
        name: "Seller's Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SALSQ",
        name: "Seller Agricultural Land Supplementary Questionnaire",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SCO",
        name: "Seller Counter Offer",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "SCV-AD",
        name: "San Fernando Valley Local Area Disclosure and Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SDDA",
        name: "Security Deposit Disclosure and Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SELI",
        name: "Seller Instructions to Exclude Listing From Internet",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SFA",
        name: "Seller Financing Addendum and Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SFLS",
        name: "Square Foot and Lot Size Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SIP",
        name: "Seller License to Remain in Possession Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SIPA",
        name: "Seller in Possession Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SMCO",
        name: "Seller Multiple Counter Offer",
        description: "",
        allows_multiple: true,
    },
    CarForm {
        code: "SNA",
        name: "Seller Non-Agency Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SOFBN",
        name: "Salesperson Owned Fictitious Business Name Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SP",
        name: "Single Party Compensation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SPT",
        name: "Notice of Your 'Supplemental' Property Tax Bill",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "STRA",
        name: "Short-Term Rental Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "STRA-SA",
        name: "Seasonal Addendum to Short-Term Rental Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "STRL",
        name: "Short-Term Rental Listing",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SUM-MII",
        name: "Summary of Move-in Inspection",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SUM-MO",
        name: "Summary of Multiple Offers",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SUM-MOI",
        name: "Move Out Inspection Summary",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SUM-PMOI",
        name: "Pre-Move Out Inspection Summary",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SVLA",
        name: "Seller's Vacant Land Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SVRA",
        name: "Short Term (Vacation) Rental Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "SXA",
        name: "Seller's Intent to Exchange Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TAA-1",
        name: "Trust Bank Account Record for All Trust Funds Deposited / Withdrawn",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TAB-1",
        name: "Trust Bank Account Record for Each Beneficiary",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TAP-1",
        name: "Trust Bank Account Record for Each Property Managed",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TCS",
        name: "Transaction Cover Sheet",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TEAM",
        name: "Team Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TEC",
        name: "Tenant Estoppel Certificate",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TF",
        name: "Trust Funds Received and Released",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TFHD",
        name: "Tenant Flood Hazard Disclosure",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TIC-FD",
        name: "Tenancy-In-Common (\"TIC\") Financial Disclosure Statement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TIC-OA",
        name: "Tenancy-In-Common (\"TIC\") Ownership Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TIC-PA",
        name: "Tenancy-In-Common (\"TIC\") Purchase Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TMEA",
        name: "Team Member Exit Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TOBR",
        name: "Transfer of Buyer Representation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TOL",
        name: "Transfer of Listing Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TOPA",
        name: "Tenant Occupied Property Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TPA",
        name: "Broker / Associate-Licensee / Assistant Three Party Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TRBC",
        name: "Tenant Representation and Broker Compensation Agreement",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "TRPR",
        name: "Offer of Tenant Positive Rental Payment Reporting",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "UOA",
        name: "Unsolicited Offer Attestation",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "WBSA",
        name: "Wooden Balconies and Stairs Addendum",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "WFA",
        name: "Wire Fraud and Electronic Funds Transfer Advisory",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "WHS",
        name: "Water Heater Statement of Compliance",
        description: "",
        allows_multiple: false,
    },
    CarForm {
        code: "WSM",
        name: "Water Submeter Addendum",
        description: "",
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
pub enum SalesSide {
    Listing,
    Purchase,
    Both,
}

impl SalesSide {
    /// Stable slug stored in `form.applies_sides` and matched against
    /// at checklist-resolution time. A `Both` deal stores `"both"`;
    /// forms that appear on a merged listing+purchase checklist record
    /// `"both"` alongside their single-side slug so they match.
    pub fn as_str(self) -> &'static str {
        match self {
            SalesSide::Listing => "listing",
            SalesSide::Purchase => "purchase",
            SalesSide::Both => "both",
        }
    }
}

pub fn sales_side(sales: SalesType) -> SalesSide {
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

// All MLS-side fixed entries — same across every checklist.
const MLS_ACT: DefaultItem = item("ACT", FormGroup::MlsDataSheets, true);
const MLS_PEND: DefaultItem = item("PEND", FormGroup::MlsDataSheets, true);
const MLS_SOLD: DefaultItem = item("SOLD", FormGroup::MlsDataSheets, true);

// Residential — Listing. Contract group is singular (RLA is the only
// listing contract on this checklist).
const RESIDENTIAL_LISTING: &[DefaultItem] = &[
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("RLA", FormGroup::ListingContract, true),
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
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("FVAC", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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

// Residential — Purchase. Contract group is plural (RPA + RIPA — only
// one is mandatory but both belong in the same section). RPA is marked
// required; brokers swap which is required from the UI if the deal uses
// RIPA instead.
const RESIDENTIAL_PURCHASE: &[DefaultItem] = &[
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("RPA", FormGroup::PurchaseContracts, true),
    item("RIPA", FormGroup::PurchaseContracts, false),
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
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("FVAC", FormGroup::AdditionalDisclosures, false),
    item("HID", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("CLA", FormGroup::ListingContract, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("CSPQ", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("CPA", FormGroup::PurchaseContract, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("CSPQ", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("VLL", FormGroup::ListingContract, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VLQ", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("VLPA", FormGroup::PurchaseContract, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VLQ", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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

// Mobile/Manufactured Home — Listing. Both RLA and MHLA are required —
// they're two separate forms, hence the plural `ListingContracts` group.
const MOBILE_HOME_LISTING: &[DefaultItem] = &[
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("RLA", FormGroup::ListingContracts, true),
    item("MHLA", FormGroup::ListingContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("MHDA", FormGroup::MandatoryDisclosures, false),
    item("MHTDS", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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

// Mobile/Manufactured Home — Purchase. RPA + MHPA, both required —
// plural `PurchaseContracts` group.
const MOBILE_HOME_PURCHASE: &[DefaultItem] = &[
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("RPA", FormGroup::PurchaseContracts, true),
    item("MHPA", FormGroup::PurchaseContracts, true),
    item("AVID-1", FormGroup::MandatoryDisclosures, true),
    item("AVID-2", FormGroup::MandatoryDisclosures, true),
    item("FHDS", FormGroup::MandatoryDisclosures, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("MHDA", FormGroup::MandatoryDisclosures, false),
    item("MHTDS", FormGroup::MandatoryDisclosures, true),
    item("RGM", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("SPQ", FormGroup::MandatoryDisclosures, true),
    item("WCMD", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("WHSD", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("EQ", FormGroup::AdditionalDisclosures, false),
    item("EQ-R", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("HID", FormGroup::AdditionalDisclosures, false),
    item("MCA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
    item("SWPI", FormGroup::AdditionalDisclosures, false),
    item("TA", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("BLA", FormGroup::ListingContract, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BDS", FormGroup::AdditionalDisclosures, false),
    item("BP-FFE", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
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
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("BPA", FormGroup::PurchaseContract, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BDS", FormGroup::AdditionalDisclosures, false),
    item("BP-FFE", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("CO", FormGroup::AdditionalDisclosures, false),
    item("CR", FormGroup::AdditionalDisclosures, false),
    item("ETA", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NTP", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
    item("RCSD", FormGroup::AdditionalDisclosures, false),
    item("RR", FormGroup::AdditionalDisclosures, false),
    item("RRRR", FormGroup::AdditionalDisclosures, false),
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

// Referral — per the client's data sheet, the whole checklist is the
// Referral Fee Agreement under its own "Referral Contract" heading.
// Side-independent: refer a buyer or a seller, the paperwork is the
// same.
const REFERRAL_ANY_SIDE: &[DefaultItem] = &[item("RFA", FormGroup::ReferralContract, true)];

// Commercial Lease + Rental/Lease — one shared printed data sheet.
// Landlord (listing) side carries the Lease Listing Agreement; both
// sides carry the lease contract itself (RLMM) and the application /
// deposit paperwork; WFDA is marked "(Tenant Only)" on the sheet so it
// appears only on the tenant side. The "Disclosures — If Applicable"
// section maps to the shared Additional Disclosures group; the sheet's
// "Application, Receipts & Reports" and "Governing Documents" sections
// are lease-specific groups. Note CC&R/HOA deliberately sit under
// Governing Documents here while the SALE checklists file them under
// Escrow Documents — the seeder keys forms by (code, group) so both
// placements coexist.
const LEASE_LISTING: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("RNTD", FormGroup::MlsDataSheets, true),
    item("LL", FormGroup::LeaseListingContract, true),
    item("RLMM", FormGroup::RentalContract, true),
    item("AD", FormGroup::MandatoryDisclosures, true),
    item("LCA", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BIRN", FormGroup::AdditionalDisclosures, false),
    item("DRA", FormGroup::AdditionalDisclosures, false),
    item("EL", FormGroup::AdditionalDisclosures, false),
    item("FEHN", FormGroup::AdditionalDisclosures, false),
    item("HOA-IR", FormGroup::AdditionalDisclosures, false),
    item("MII", FormGroup::AdditionalDisclosures, false),
    item("MOI", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NOE", FormGroup::AdditionalDisclosures, false),
    item("NRI", FormGroup::AdditionalDisclosures, false),
    item("PMA", FormGroup::AdditionalDisclosures, false),
    item("PMOI", FormGroup::AdditionalDisclosures, false),
    item("PRQ", FormGroup::AdditionalDisclosures, false),
    item("CCR", FormGroup::ApplicationReceiptsReports, true),
    item("LRA", FormGroup::ApplicationReceiptsReports, true),
    item("SDR", FormGroup::ApplicationReceiptsReports, true),
    item("CC&R", FormGroup::GoverningDocuments, false),
    item("HOA", FormGroup::GoverningDocuments, false),
    item("R&R", FormGroup::GoverningDocuments, false),
    item("CLR", FormGroup::ReleaseDisclosures, false),
];

// Tenant side: no landlord listing agreement; WFDA joins the mandatory
// section per the sheet's "(Tenant Only)" note. Everything else
// mirrors the listing side.
const LEASE_TENANT: &[DefaultItem] = &[
    item("ACT", FormGroup::MlsDataSheets, true),
    item("PEND", FormGroup::MlsDataSheets, true),
    item("RNTD", FormGroup::MlsDataSheets, true),
    item("RLMM", FormGroup::RentalContract, true),
    item("AD", FormGroup::MandatoryDisclosures, true),
    item("LCA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("ADM", FormGroup::AdditionalDisclosures, false),
    item("BIRN", FormGroup::AdditionalDisclosures, false),
    item("DRA", FormGroup::AdditionalDisclosures, false),
    item("EL", FormGroup::AdditionalDisclosures, false),
    item("FEHN", FormGroup::AdditionalDisclosures, false),
    item("HOA-IR", FormGroup::AdditionalDisclosures, false),
    item("MII", FormGroup::AdditionalDisclosures, false),
    item("MOI", FormGroup::AdditionalDisclosures, false),
    item("MT", FormGroup::AdditionalDisclosures, false),
    item("NOE", FormGroup::AdditionalDisclosures, false),
    item("NRI", FormGroup::AdditionalDisclosures, false),
    item("PMA", FormGroup::AdditionalDisclosures, false),
    item("PMOI", FormGroup::AdditionalDisclosures, false),
    item("PRQ", FormGroup::AdditionalDisclosures, false),
    item("CCR", FormGroup::ApplicationReceiptsReports, true),
    item("LRA", FormGroup::ApplicationReceiptsReports, true),
    item("SDR", FormGroup::ApplicationReceiptsReports, true),
    item("CC&R", FormGroup::GoverningDocuments, false),
    item("HOA", FormGroup::GoverningDocuments, false),
    item("R&R", FormGroup::GoverningDocuments, false),
    item("CLR", FormGroup::ReleaseDisclosures, false),
];

// Multi-Family — hidden from the new-transaction picker but kept here so
// any old persisted rows still seed a sensible checklist. Lands in the
// listing-contract bucket since the dual-side fallback uses listing
// labelling.
const MULTI_FAMILY_FALLBACK: &[DefaultItem] = &[
    MLS_ACT,
    MLS_PEND,
    MLS_SOLD,
    item("RIPA", FormGroup::ListingContracts, true),
    item("CLA", FormGroup::ListingContracts, true),
    item("LPD", FormGroup::MandatoryDisclosures, false),
    item("RGM", FormGroup::MandatoryDisclosures, true),
    item("SBSA", FormGroup::MandatoryDisclosures, true),
    item("WFDA", FormGroup::MandatoryDisclosures, true),
    item("VP", FormGroup::MandatoryDisclosures, true),
    item("BCA", FormGroup::AdditionalDisclosures, false),
    item("BRBC", FormGroup::AdditionalDisclosures, false),
    item("POF", FormGroup::AdditionalDisclosures, false),
    item("QUAL", FormGroup::AdditionalDisclosures, false),
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

/// Pick the contract group variant that this (type, side) checklist
/// uses for its main contract form(s). Special-condition addenda land
/// in the same group so they render directly below the contract.
fn contract_group_for(t: TransactionType, side: SalesSide) -> FormGroup {
    match (t, side) {
        // Lease deals: special-condition addenda join the landlord's
        // listing section when one exists, else the rental contract.
        (TransactionType::CommercialLease | TransactionType::RentalLease, SalesSide::Purchase) => {
            FormGroup::RentalContract
        }
        (TransactionType::CommercialLease | TransactionType::RentalLease, _) => {
            FormGroup::LeaseListingContract
        }
        // Referral: everything, addenda included, files under the one
        // Referral Contract section.
        (TransactionType::Referral, _) => FormGroup::ReferralContract,
        // Residential Purchase bundles RPA + RIPA → plural.
        (TransactionType::Residential, SalesSide::Purchase) => FormGroup::PurchaseContracts,
        // Manufactured Home bundles two contracts on each side → plural.
        (TransactionType::ManufacturedHome, SalesSide::Listing) => FormGroup::ListingContracts,
        (TransactionType::ManufacturedHome, SalesSide::Purchase) => FormGroup::PurchaseContracts,
        // Dual-side (Listing & Purchase, Tenant & Landlord) — special
        // items land in the listing variant. Since the merged checklist
        // already mixes both sides' main contracts, the special-addenda
        // location is somewhat arbitrary; listing-side is the printed
        // convention.
        (_, SalesSide::Both) => FormGroup::ListingContract,
        // Multi-Family falls through to the same plural-listing slot it
        // uses for its main contracts.
        (TransactionType::MultiFamily, _) => FormGroup::ListingContracts,
        // All other single-contract listings/purchases.
        (_, SalesSide::Listing) => FormGroup::ListingContract,
        (_, SalesSide::Purchase) => FormGroup::PurchaseContract,
    }
}

// Special-condition addenda. Listing-side gets PLA / SSLA / REOL;
// purchase-side gets PA / SSA / REO. These now slot into the per-
// checklist contract group rather than a dedicated section, and are
// marked `required` because once a condition is set, the addendum
// becomes part of the binding contract.
fn special_condition_items(
    c: SpecialSalesCondition,
    side: SalesSide,
    contract_group: FormGroup,
) -> Vec<DefaultItem> {
    let listing_codes: &[&str] = match c {
        SpecialSalesCondition::None => &[],
        SpecialSalesCondition::Probate => &["PLA"],
        SpecialSalesCondition::ShortSale => &["SSLA"],
        SpecialSalesCondition::REO => &["REOL"],
    };
    let purchase_codes: &[&str] = match c {
        SpecialSalesCondition::None => &[],
        SpecialSalesCondition::Probate => &["PA"],
        SpecialSalesCondition::ShortSale => &["SSA"],
        SpecialSalesCondition::REO => &["REO"],
    };
    let codes: Vec<&str> = match side {
        SalesSide::Listing => listing_codes.to_vec(),
        SalesSide::Purchase => purchase_codes.to_vec(),
        SalesSide::Both => {
            let mut v = listing_codes.to_vec();
            for c in purchase_codes {
                if !v.contains(c) {
                    v.push(c);
                }
            }
            v
        }
    };
    codes
        .into_iter()
        .map(|code| item(code, contract_group, true))
        .collect()
}

/// Pick the per-(type, side) checklist, returning a fresh `Vec` so the
/// caller can mutate without affecting the static arrays.
fn defaults_for(t: TransactionType, side: SalesSide) -> Vec<DefaultItem> {
    match (t, side) {
        (TransactionType::Residential, SalesSide::Listing) => RESIDENTIAL_LISTING.to_vec(),
        (TransactionType::Residential, SalesSide::Purchase) => RESIDENTIAL_PURCHASE.to_vec(),
        (TransactionType::Residential, SalesSide::Both) => {
            merge_sides(RESIDENTIAL_LISTING, RESIDENTIAL_PURCHASE)
        }

        (TransactionType::Commercial, SalesSide::Listing) => COMMERCIAL_LISTING.to_vec(),
        (TransactionType::Commercial, SalesSide::Purchase) => COMMERCIAL_PURCHASE.to_vec(),
        (TransactionType::Commercial, SalesSide::Both) => {
            merge_sides(COMMERCIAL_LISTING, COMMERCIAL_PURCHASE)
        }

        // Both lease types share the printed Commercial Lease +
        // Rental/Lease data sheet.
        (TransactionType::CommercialLease | TransactionType::RentalLease, SalesSide::Listing) => {
            LEASE_LISTING.to_vec()
        }
        (TransactionType::CommercialLease | TransactionType::RentalLease, SalesSide::Purchase) => {
            LEASE_TENANT.to_vec()
        }
        (TransactionType::CommercialLease | TransactionType::RentalLease, SalesSide::Both) => {
            merge_sides(LEASE_LISTING, LEASE_TENANT)
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

        // Referral: the same minimal fee-paperwork checklist regardless
        // of which side the client was referred from.
        (TransactionType::Referral, _) => REFERRAL_ANY_SIDE.to_vec(),

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
/// special-condition addenda which now slot into the contract group
/// directly under the main contract form.
pub fn build_default_checklist(
    t: TransactionType,
    cond: SpecialSalesCondition,
    sales: SalesType,
) -> Vec<DefaultItem> {
    let side = sales_side(sales);
    let contract_group = contract_group_for(t, side);
    let mut out = defaults_for(t, side);
    out.extend(special_condition_items(cond, side, contract_group));
    out
}
