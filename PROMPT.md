# CLAUDE PROMPT — Build Proof-of-Concept MVP  
**TransactVault** – Modern Real Estate Transaction Management Platform

You are an elite full-stack engineer using the skills: html-purist, rust-saas, surrealdb, frontend, humanizer.

Build this as a beautiful, extremely fast, modern, clean, and simple product that feels significantly better than SkySlope or Dotloop.

Focus exclusively on product requirements, user experience, design, and workflows. Make the UI premium, calm, and delightful — modern, spacious, and professional with excellent typography and micro-interactions.

**Product Positioning**
- Transparent flat monthly pricing with no sales calls required
- 3 years of compliant storage included while subscribed
- Built specifically for California brokerages (strong Antelope Valley focus)
- Secure, auditable, versioned document storage
- No built-in e-signatures (handled externally by MLS/association)

### Landing Page (Public Home Page)
Create a stunning, high-converting landing page with these sections:
- Hero section: Bold headline “Transaction management that actually feels good to use” + subheadline “Flat pricing. 3 years storage included. Built for California brokers.”
- Clear comparison: “Better than SkySlope — no hidden fees, no sales calls”
- Key benefits (cards): Transparent Pricing, 3-Year Storage Included, Beautiful & Fast, Full Compliance Visibility, California Focused
- Testimonials placeholder (can be fake for PoC)
- Pricing teaser with “See plans” button
- Footer with links

### Pricing Page (Fully Public & Transparent)
Create a beautiful, clean pricing page that requires **no sales call**. Show these exact tiers:

| Tier          | Max Transactions per Month | Monthly Price | Best For                          | Key Features |
|---------------|----------------------------|---------------|-----------------------------------|--------------|
| Starter      | 25                         | $149          | Solo agents & small teams        | Full features, 3-year storage |
| Growth       | 100                        | $299          | Typical brokerages               | Everything + priority support |
| Scale        | 250                        | $499          | Growing & mid-size offices       | Everything + advanced search |
| Enterprise   | Unlimited                  | $799          | Large & multi-office brokerages  | Dedicated support + custom |

- Show annual option with 15% discount
- Clear “No hidden fees • Cancel anytime • 60-day data export after cancellation”
- Strong CTA: “Start free 14-day trial – No credit card required”

### Core Users & Roles
1. **Broker / Owner** – Full visibility across all transactions, manages team, compliance oversight
2. **Agent** – Can create and manage only their own transactions
3. **Transaction Coordinator / Compliance Staff** – Can view all transactions, focus on checklists and audit readiness

### Key Entities
- Brokerage (the company/account)
- User (with role inside a brokerage)
- Transaction (one real estate deal)
- Checklist Item (per transaction)
- Document (stored files)

### Detailed MVP Workflows

**1. Onboarding & Setup**
- Sign up with Google or email/password
- First user creates the brokerage account and becomes the Broker
- Broker can invite Agents and Transaction Coordinators by email
- Simple profile with name and photo

**2. Dashboard**
- Clean overview of all active transactions (for Brokers) or my transactions (for Agents)
- Quick filters by status and search
- Recent activity feed
- Clear visual count of transactions that need attention

**3. Transaction Management**
- Create new transaction with property address, city, price, parties involved, and expected closing date
- Status options: Open, Under Contract, Closed, Cancelled
- Beautiful transaction detail page as the central workspace

**4. Checklist & Compliance Workflow (Very Important)**
- Each transaction has a clear, visual checklist
- Pre-defined standard items (Contract, Disclosures, Inspection Report, Appraisal, Title Report, Closing Documents, etc.)
- Broker can add custom items
- Items can be marked complete by Agents or TCs
- Strong visual design: items turn **green** when completed
- Clear “Compliance Score” or progress bar
- When **all items are green**, the transaction shows a prominent “Compliance Complete” state so the Broker knows it is audit-ready
- Simple audit trail showing who completed each item and when

**5. Document Management**
- Drag-and-drop upload area inside each transaction
- Documents are organized clearly (can be grouped by category)
- Support for PDFs, photos, inspection reports, etc.
- Automatic versioning of documents
- View version history
- One-click download of entire transaction as a ZIP file
- Signed PDFs must be preserved exactly (never altered)

**6. Search & Visibility**
- Fast search across all documents and transactions in the brokerage
- Brokers can search everything; Agents see only their own

**7. Cancellation & Data Export**
- Users can export all their transaction documents at any time
- Clear policy: while subscribed, 3 years of storage is included; after cancellation, 60-day export window

### Design & Experience Requirements
- Extremely clean, modern, and premium look
- Very fast and responsive (feels instant)
- Generous whitespace and beautiful typography
- Mobile-friendly (many users will check on phones)
- Calm color palette with excellent use of green for completed states
- Delightful micro-interactions on checklist completion, uploads, and status changes
- The overall feeling should be “this is so much easier and nicer than what I’m currently using”

**MVP Success Criteria**
The proof of concept must allow a Broker to:
- Sign up and create a brokerage
- Invite an Agent
- Create a transaction
- Add documents (some signed, some not)
- Complete checklist items until the transaction shows full green compliance
- Search and export the transaction

Build this with focus purely on product excellence, beautiful UX, and strong compliance workflow. Start by outlining the full user flows and entity relationships, then proceed to build the landing page, pricing page, and core app.

Begin now.
