# CHANGE REQUEST — Update TransactVault MVP with California CAR Forms & Smart Checklists

You are an elite full-stack engineer using the skills: html-purist, rust-saas, surrealdb, frontend, humanizer.

Incorporate the following new product requirements into the existing TransactVault application. Focus exclusively on product requirements, user experience, design, and workflows.

### New Core Product Requirements

**1. Master California CAR Forms Library (ALL CAR REAL ESTATE FORMS 2026)**
The system must contain a complete master library of California Association of REALTORS (CAR) forms. Every form includes:
- Abbreviation/Code (e.g. RPA, AVID-1, TDS)
- Full Form Name
- Short description/purpose (when available)

Key forms include (but are not limited to):
RPA – California Residential Purchase Agreement, AVID-1/AVID-2 – Agent Visual Inspection Disclosure, TDS – Real Estate Transfer Disclosure Statement, SBSA – Statewide Buyer and Seller Advisory, WFDA – Wildfire Disaster Advisory, SPQ – Seller Property Questionnaire, CPA – Commercial Property Purchase Agreement, CLA – Commercial and Residential Income Listing Agreement, and all other forms listed in the 2026 CAR catalog (ADM, CO, CR, ETA, MT, NTP, RR, RRRR, etc.).

**2. Transaction Creation – Smart Default Checklist**
When creating a new transaction, the system automatically generates the correct default checklist based on these exact fields and dropdown values:

**Transaction Type** (required):
- Residential
- Commercial
- Multi-Family (5+ Units)
- Vacant Lots & Land
- Manufactured / Mobile Home
- Business Opportunity
- Commercial Lease
- Rental / Lease

**Special Sales Condition**:
- None
- Probate Sale
- Short Sale
- REO (Bank Owned)

**Sales Type**:
- Listing
- Purchase
- Listing & Purchase
- Lease (Tenant)
- Lease (Landlord)
- Lease (Tenant & Landlord)
- Referral

Additional fields shown on the New Transaction page:
- Property Address
- City
- Assessors Parcel Number (APN)
- Postal Code
- Sales Price
- Client Name
- Primary MLS Number
- Office File Number

**3. Checklist Grouping & Visual Progress**
All checklist items must be grouped exactly as they appear in the official California checklists:
- MLS DATA SHEETS
- LISTING/PURCHASING CONTRACTS
- MANDATORY DISCLOSURES
- SPECIAL CONDITIONS DISCLOSURES – ACTIVE WHEN CHECKED
- ADDITIONAL DISCLOSURES
- DISCLOSURES – IF APPLICABLE
- ESCROW DOCUMENTS
- REPORTS, CERTIFICATES & CLEARANCES
- RELEASE DISCLOSURES

Each group must show:
- Group title
- A percentage-complete progress bar (e.g. “Mandatory Disclosures – 7 of 11 complete (64%)”)
- The individual checklist items inside the group

**4. Required vs Optional Items**
- The system automatically marks the correct forms as **required** based on the selected Transaction Type + Special Sales Condition + Sales Type.
- Items turn **green** when marked complete.
- When every required item across all groups is green, the transaction shows a prominent “Compliance Complete / Audit-Ready” state.
- Agents and TCs can freely **add any optional forms** from the master CAR library or create custom free-text items.

**5. Default Checklists by Transaction Type (exact groups and forms)**
Use these groupings and forms for each type (automatically selected on transaction creation):

**Residential:**
- MLS DATA SHEETS: ACT, PEND, SOLD
- LISTING/PURCHASING CONTRACTS: RPA, RIPA, RLA
- MANDATORY DISCLOSURES: AVID-1, AVID-2, FHDS, LPD, RGM, SBSA, SPQ, TDS, WCMD, WFDA, WHSD, VP
- etc. (full groups as shown in the Residential checklist PDF)

**Commercial:**
- MLS DATA SHEETS: ACT, PEND, SOLD
- LISTING/PURCHASING CONTRACTS: CPA, CLA
- MANDATORY DISCLOSURES: AVID-1, AVID-2, CSPQ, SBSA, WFDA, VP
- etc. (full groups as shown in the Commercial checklist PDF)

**Multi-Family, Vacant Lots & Land, Manufactured/Mobile Home, Business Opportunity** follow the exact groupings and forms provided in their respective checklist PDFs.

**6. Document Upload & Storage Relationship**
- Every checklist item can have one or more documents attached.
- File storage structure follows this pattern: Brokerage → Property Folder (address or APN) → Form Code Folder (e.g. RPA, AVID-1) → actual PDF files.

**7. Existing Pages to Update**
- Update the **New Transaction** page to include all the exact fields and dropdown values listed above.
- Update the **Transaction Detail** page to display the new grouped checklist with progress bars, green completion states, and the ability to add optional forms.

**MVP Success Criteria (Updated)**
A Broker must be able to:
- Create a new transaction selecting different Transaction Types, Special Sales Conditions, and Sales Types
- See the correct default grouped checklist automatically populated with the proper CAR forms
- Mark items complete and watch group progress bars update in real time
- Add optional extra forms from the master library
- Upload documents to specific checklist items
- Reach “Compliance Complete” state when all required items are green

Build this with focus purely on product excellence, beautiful UX, and strong compliance workflow. Start by outlining the full updated user flows and entity relationships that incorporate the CAR forms library and grouped checklists, then proceed to implement the changes while maintaining the existing beautiful, modern, premium UX.

Begin now.
