# Enterprise Trade Finance B2B Platform — Nirdosha build tracker

Source: `Enterprise_Trade_Finance_B2B_Platform_Expanded.docx` (Downloads
folder), read in full. Plan: `/home/arun/.claude/plans/ticklish-hopping-tarjan.md`.

**How to read this file.** Every capability named in the doc gets one
line here, tagged:
- `[BUILD]` — real Nirdosha logic/data/UI, working end-to-end.
- `[MOCK]` — the real external system (sanctions feed, SWIFT network,
  bank rail, credit bureau, regulator) doesn't exist for us; a
  `mock_*`-named Nirdosha function stands in, same precedent as
  `mock_issue_token`. Proves the workflow, not connected to anything real.
- `[SUBSTITUTED]` — a named technology is replaced by Nirdosha's own
  equivalent (see plan's Disposition framework). Never means "dropped."

Architecture: **one** `.nir` file (`trade_finance.nir`), **one** shared
SQLite DB (`trade_finance.db`), **one** `nirdosha serve` process, reusing
`examples/identity_mock.nir` (extended roles) for auth and `mq` (Redis)
for the doc's genuinely event-shaped cross-module flows. "Nine
microservices / Kubernetes / Kafka / Camunda / Angular+native-mobile" is
`[SUBSTITUTED]` platform-wide by this — not repeated per module below.

Checked off only once actually run and verified (curl + browser), per
the plan's Verification section — not on "code written."

---

## Phase 0 — Compiler amendment (prerequisite for Module 1's audit chain)

- [x] `[BUILD]` Expose `sha256_hex` as a public Nirdosha builtin
      (previously a private Rust helper backing `validate_api_key`
      only) — `sha256_hex(s: str) -> str` (1-arg) and
      `sha256_hex(a: str, b: str) -> str` (2-arg, hashes both parts in
      sequence, since `str` has no `+`/concatenation to combine them
      with first — this is what makes `hash = sha256_hex(prev_hash,
      payload)` expressible at all). `ast.rs` BUILTIN_NAMES +
      `typeck.rs` signature (both arities) + `interpreter.rs`
      `eval_builtin` arm. `tests/sha256_hex.rs` (3 tests, incl. the
      standard `sha256("")` test vector) — all passing.

---

## Phase 1 — Foundation

### Module 1: System Foundation, Identity, Security & Approval Governance
- [x] `[BUILD]` Generalized to true n-eyes: `submit_approval` /
      `decide_approval_core`, one shared state-machine primitive for any
      integer `required_eyes` (Maker-Checker and 6-Eyes are just
      `required_eyes` 1 and 2, not separate tiers), an append-only
      `approval_decision` log per request (not mutable stage fields).
      **Verified live**: a `required_eyes: 3` request correctly stayed
      PENDING after 2 of 3 distinct decisions and only reached APPROVED
      on the 3rd, proving the generalization goes beyond the earlier
      hardcoded 1-or-2 tiers; re-verified the pre-existing 6-Eyes
      sanctions-override flow still works unchanged after the refactor.
- [x] `[BUILD]` Decision links: `mint_decision_link` /
      `decide_approval_via_link` — a reviewer who isn't logged in (email
      link click, OTP entry) proves identity via a single-use signed
      token instead of a bearer token, then goes through the exact same
      `decide_approval_core` state transition as the logged-in path.
      Token = `sha256_hex(sha256_hex(sha256_hex(identity.subject,
      payload_note), prev_hash), <dev-only secret>)`, secret last
      (envelope MAC, resists length-extension against the underlying
      SHA-256), `prev_hash` (the audit chain's current tip at mint time)
      as a per-mint nonce. No time-based expiry (no wall-clock builtin,
      see below) — single-use via a `consumed` flag instead.
      `get_decision_link` is a dev-only stand-in for the email itself,
      which doesn't exist, and now only returns a link's token to the
      identity that minted it. **Verified live**: a 3rd distinct decider
      approved a `required_eyes: 3` request via `decide_approval_via_link`
      with *no* `Authorization` header at all, reaching APPROVED;
      replaying the same link was correctly rejected as already-consumed;
      audit chain stayed intact (`verify_audit_chain` → -1) across a mix
      of bearer-token and link-based decisions.
    - **Red-team finding, fixed 2026-08-22**: an earlier version took
      `actor_subject` as free text from the minting caller, unrelated to
      their own identity, with no role gate — any authenticated user of
      any role could mint a link naming an arbitrary (even nonexistent)
      subject and single-handedly decide as them, fully defeating
      segregation of duties. Also, the token had no per-mint nonce, so
      repeated mints for the same `(actor_subject, payload_note)` pair
      produced identical, cross-link-replayable tokens. Fixed by binding
      `actor_subject` to `identity.subject` unconditionally (mint only
      for yourself) and folding the audit chain's current tip into the
      token as a nonce. Re-verified live: an unrelated low-privilege
      identity can no longer name another subject, can't read back a
      link it didn't mint, and two mints sharing identical actor+note
      text now produce distinct, non-cross-replayable tokens. Full
      writeup: five-agent red-team audit, 2026-08-22 (17 findings across
      the compiler, runtime, `serve.rs`, and this app — see conversation
      history / audit artifact for the rest, most still open).
- [x] `[BUILD]` (superseded) Original Maker-Checker (2-Eyes) + 6-Eyes
      (3-tier) engine. **Verified live**: MC correctly rejected
      the same identity deciding its own submission and accepted a
      different one; 6-Eyes correctly stayed PENDING after 1 of 2
      reviews, rejected a second attempt by the same reviewer, and
      reached APPROVED only once a *third* distinct identity acted —
      curl transcript in this session.
- [x] `[BUILD]` Segregation of duties by identity (not role), exactly
      as the doc specifies ("regardless of role assignment") — one SQL
      query (`UNION ALL` over initiator + prior deciders) rather than
      hand-rolled per-tier comparisons. Verified above.
- [x] `[BUILD]` Immutable, hash-chained audit log: every governance
      action writes an `AuditLogEntry` via the `finish_with_audit`
      terminal helper, `hash = sha256_hex(prev_hash, note)`.
      `verify_audit_chain(count)` walks and returns -1 (OK) or the id
      of the first broken link. **Verified live**: real chain of 7
      entries (submits, accepted decides, and a rejected SoD attempt —
      rejections are audited too), `verify_audit_chain` returned -1.
- [x] `[BUILD]` `list_approval`/`list_audit_log` — read-only, no gate,
      verified live.
- [ ] `[BUILD]` SLA deadline field: stored (`sla_hours`) and returned in
      the queue, but no computed overdue/escalation flag yet (needs a
      wall-clock reference Nirdosha doesn't have — see `[SUBSTITUTED]`
      below; this is the one Module 1 item not yet done).
- [ ] `[BUILD]` Dynamic policy matrix (role × resource × action,
      editable) — not yet built; today every module hard-codes its own
      `requires(role: ...)` per action, which is real and correct but
      not yet externalized into an editable table the way the doc
      describes. Deferred, not forgotten.
- [ ] `[BUILD]` Multi-tenant segregation (`tenant_id` partition on every
      entity) — not yet built; this build assumes a single tenant so
      far. Deferred to whichever module first actually needs
      cross-tenant isolation to matter.
- [ ] `[SUBSTITUTED]` OAuth2/OIDC Authorization Code+PKCE, SAML 2.0
      federation (Azure AD/Okta/Ping), FIDO2/TOTP/biometric step-up MFA,
      X.509 digital signature logging, IP/geofence allow-listing, Redis
      distributed step-locking, WORM object-lock storage, PII-scrubbing
      log pipeline, real-time anomaly-alert streaming, FCM/APNs push →
      the existing Bearer-token identity (real: `mock_issue_token`-minted,
      independently verified by the real, unmodified
      `oidc_validate_token`) is the platform's one authentication/
      step-up mechanism; the hash-chained audit row is the
      tamper-evidence mechanism in place of WORM object storage; no
      push channel exists (web-only, no mobile client — see Module 1's
      mobile section below).
- [ ] `[SUBSTITUTED]` Native Android/iOS approval screens → the same
      responsive `emit-ui` web app, reachable from a phone browser; no
      Kotlin/Swift binaries are produced.
- Entities: `ApprovalRequest`(id, action_type, risk_tier, stage,
  initiator_subject, reviewer_subject, approver_subject, sla_deadline,
  status), `AuditLogEntry`(id, entity_type, entity_id, actor_subject,
  action, prev_hash, hash, timestamp_note), `PolicyRule`(id, role,
  resource_type, action, tier).
- Endpoints: `submit_approval`, `decide_approval`, `list_approval`,
  `list_audit_log`, `verify_audit_chain`, `list_policy_rule`,
  `update_policy_rule`.
- Governance (per doc Module 1 table): view audit = none; edit policy
  matrix = Maker-Checker; configure 6-Eyes thresholds = Maker-Checker;
  approve disbursement above threshold = 6-Eyes; revoke tenant sessions
  = 6-Eyes (session revocation itself is `[SUBSTITUTED]` — no session
  store beyond the bearer token's own expiry).

### Module 2: Corporate & Counterparty Onboarding (KYC/KYB)
- [x] `[BUILD]` `Counterparty` record (buyer/seller/bank/agent),
      `draft`→`active` status. `list_/create_/update_counterparty`.
      Verified live.
- [x] `[BUILD]` `CreditLimit` (facility cap, utilized amount) +
      `check_credit_hold(counterparty_id, amount_cents) -> bool` — a
      real, reusable function (not yet *called* by other modules, since
      4/5/8 don't exist yet, but genuinely callable and correct today).
      **Verified live**: correctly returned `true` for a request within
      the cap and `false` for one exceeding it.
- [x] `[BUILD]` Financial ratio computation + credit score:
      `current_ratio_bps`/`debt_to_equity_bps` computed as real integer
      basis-points arithmetic (no int↔float cast exists in Nirdosha —
      LANGUAGE.md §3 — so ratios are bps, not decimals, same convention
      `rev_assurance.nir`'s `stat_leakage_rate_bps` already used),
      feeding a simple deterministic scoring formula, clamped [0,
      10000]. **Verified live**: known inputs produced the
      hand-computed expected score (10000, clamped).
- [x] `[MOCK]` `mock_lei_lookup(lei) -> Result(json, str)` — exact
      match against 2 fixed illustrative entities. Verified live (hit +
      miss).
- [x] `[MOCK]` `mock_sanctions_screen(legal_name) -> Result(json, str)`
      — exact match against 1 fixed illustrative watchlist name.
      Verified live (clean + match).
- [x] `[BUILD]` Sanctions-match override wired through the **real**
      Module 1 6-Eyes primitive end-to-end (`submit_sanctions_override`
      → two distinct `decide_approval`s → `clear_sanctions_override`
      actually flips the counterparty to `active`). **Verified live**:
      clearing correctly refused before full approval, succeeded after
      two distinct approvers, and the counterparty row's `status`
      changed for real. This is the proof that Module 1's governance
      primitive genuinely generalizes across modules, not just within
      Module 1 itself.
- [ ] Automated internal credit score is a formula, not an ML model —
      already covered under `[SUBSTITUTED]` above; not a separate gap.
- [ ] Working capital turnover / Altman Z-score specifically (as named
      metrics) — not yet added; current/debt-to-equity ratios + the
      composite score are built, the other two named ratios are
      deferred (same formula-based approach would apply).
- [ ] `[SUBSTITUTED]` Nightly rescan batch job → not yet built as an
      explicit `rescan_counterparty` action (planned, not done).
- [ ] `[SUBSTITUTED]` OCR/document extraction, encrypted upload
      pipeline → confirmed not applicable: no file upload capability
      exists in Nirdosha at all yet; all fields entered directly.
- Entities: `Counterparty`, `ScreeningResult`, `FinancialStatement`,
  `CreditLimit`.
- Endpoints: `list_/create_/update_counterparty`, `mock_lei_lookup`,
  `mock_sanctions_screen`, `rescan_counterparty`,
  `create_financial_statement`, `list_financial_statement`,
  `check_credit_hold`, `stat_/chart_` dashboard entries (active
  counterparties, screening hits, average credit score).
- Governance: view profile = none; edit bank account = Maker-Checker;
  approve onboarding activation = Maker-Checker; override sanctions
  match = 6-Eyes; increase credit facility cap = 6-Eyes.

---

## Phase 2 — Trade & Physical Goods

### Module 3: Trade Contracts, Documents & Instrument Lifecycle
- [x] `[BUILD]` `PurchaseOrder` with `workflow_variant` (ADVANCE_PAYMENT
      / OPEN_ACCOUNT / DOCUMENTARY_COLLECTION / LC_BACKED /
      MILESTONE_ESCROW) — all five variants named in the doc, as real
      selectable/stored state, not prose. **Verified live**.
- [x] `[BUILD]` Payment-method decision engine: a real, deterministic
      rule function (counterparty trust tier, credit rating, order
      size, bank-guarantee requirement → recommended variant), with the
      final choice always a separate human-recorded field (matches the
      doc's "recommendation, not automatic" rule).
- [x] `[BUILD]` `LetterOfCredit` lifecycle state machine (applied →
      advised → amended → presented → discrepancy-checked → settled),
      Maker-Checker/6-Eyes per the doc's table (issuance = 6-Eyes,
      amendment = Maker-Checker). **Verified live**: `submit_lc_issuance`
      correctly rejected the initiator deciding their own request
      (segregation of duties), stayed PENDING after 1 of 2 decisions,
      and `issue_letter_of_credit` correctly refused before full
      approval and succeeded once two distinct identities approved.
- [x] `[BUILD]` UCP 600 discrepancy checker: a real rule function
      cross-checking presented document fields (quantities, party
      names) against the PO/LC, producing a real
      `DiscrepancyCheckResult` row per field — the actual comparison
      logic, not a stub. **Verified live**: a presentation matching the
      underlying PO's quantity/beneficiary produced `CLEAN`/`MATCH`
      across the board and advanced the LC to `presented`; dates aren't
      compared numerically (no date/time builtin — see `[SUBSTITUTED]`
      below).
- [x] `[MOCK]` `mock_generate_swift_message(msg_type, reference,
      fields_note) -> str` — stands in for real Prowide/SWIFT Alliance
      Access transmission for MT700/MT707/MT710/MT760; logs a
      `SwiftMessageLog` row with the real semantic fields (not
      wire-format bytes, not actually transmitted anywhere). **Verified
      live**. Reused as-is by Module 8's `mock_generate_pacs008` rather
      than a second copy of the same table/logging logic.
- [ ] `[SUBSTITUTED]` OCR/ML line-item extraction from scanned trade
      documents → structured fields entered directly (same reasoning as
      Module 2 — no vision pipeline, no file upload yet). No standalone
      `list_trade_document` browse screen exists either (its data
      surfaces through `DiscrepancyCheckResult` instead) — a real,
      disclosed gap, not a naming accident (confirmed against
      `to_snake_case`'s actual behavior, see Session notes).
- Entities: `PurchaseOrder`, `LetterOfCredit`, `TradeDocument`(fields
  entered directly, no OCR), `DiscrepancyCheckResult`, `SwiftMessageLog`.
- Endpoints: `list_/create_/update_purchase_order`,
  `list_/create_letter_of_credit`, `amend_letter_of_credit`,
  `present_documents`, `list_discrepancy_check_result`,
  `mock_generate_swift_message`.
- Governance: view LC status = none; amend PO line item =
  Maker-Checker; issue LC (MT700) = 6-Eyes; accept UCP 600 discrepancy
  waiver = 6-Eyes; select workflow variant = Maker-Checker.

### Module 6: Inventory & Goods Movement Reconciliation
- [x] `[BUILD]` `GoodsReceiptNote` ingestion (web form; per-line
      received quantity + condition). **Verified live**.
- [x] `[BUILD]` Three-way match engine: one query/comparison per line
      item across PO quantity and GRN received quantity, computing a
      real variance % against a configurable tolerance — real
      reconciliation math, not a stub. **Verified live**: a 498/500
      receipt computed 40bps variance (auto-confirmed, ≤200bps
      tolerance) and a 150/200 receipt computed 2500bps (dispute).
      B/L quantity specifically isn't a separate stored field —
      `PurchaseOrder.quantity` stands in for both PO and B/L quantity
      (no separate bill-of-lading entity exists in Module 3), a
      disclosed simplification of the doc's three-way (not two-way)
      match.
- [x] `[BUILD]` Auto-confirmation within tolerance (no human action,
      `status = "auto_confirmed"`) vs. dispute routing beyond tolerance
      — matches the doc's governance table exactly (auto = none inside
      tolerance; Maker-Checker to manually confirm outside it).
      **Verified live** (both branches, above).
- [x] `[BUILD]` `DeliveryDispute` workflow (short/damage/over), carrier
      claim initiation, Maker-Checker gated. **Verified live**:
      `initiate_carrier_claim` correctly moved an `open` dispute to
      `carrier_claim_initiated`.
- [x] `[BUILD]` Real cross-module gate: an open dispute on a line item
      is checked synchronously by Module 5's discounting-eligibility
      function and Module 4's milestone-release function (a real
      function call/query, not just documented intent). **Verified
      live**: with a genuinely `open` dispute on a PO,
      `submit_milestone_release` was correctly rejected
      ("blocked: open delivery dispute...") and `create_invoice` on the
      same PO correctly landed as `held_dispute`; once the dispute
      moved to `carrier_claim_initiated` (no longer counted as `open`
      by `has_open_dispute`'s own definition — being actively handled,
      not silently ignored), both gates correctly let a fresh
      request/invoice through instead.
- [x] `[BUILD]` `mq_publish("inventory.grn.confirmed", ...)` on
      auto-confirmation — the one required real `mq` integration proof
      point named in the plan, consumed conceptually by Modules 4/5
      (implemented as those modules' own synchronous dispute-check
      query against `DeliveryDispute`, since Nirdosha has no persistent
      background consumer process to keep a subscription alive between
      requests — the publish is real, disclosed as `[SUBSTITUTED]`
      one-way notification rather than a live consumer).
- [ ] `[MOCK]` Carrier/logistics shipment-status feed, warehouse
      IoT/barcode/RFID scan ingestion → no such hardware/feed exists;
      GRN quantities are entered directly by a (simulated) warehouse
      operator through the web form instead of arriving via IoT
      gateway.
- Entities: `GoodsReceiptNote`, `MatchResult`, `DeliveryDispute`.
- Endpoints: `list_/create_goods_receipt_note`, `list_match_result`,
  `list_/create_/update_delivery_dispute`, `initiate_carrier_claim`
  (mock), `stat_/chart_` (match rate, open disputes by type).
- Governance: view scan feed = none; auto-confirm within tolerance =
  none; manually confirm outside tolerance = Maker-Checker; initiate
  carrier claim = Maker-Checker; release milestone tranche gated on
  delivery = 6-Eyes (implemented in Module 4).

---

## Phase 3 — Commercial Core

### Module 4: B2B Trade Payments & Tiered Commission Engine
- [x] `[BUILD]` Unified `TradePayment` initiation across all five
      workflow variants from Module 3, with governance routing
      (Module 1 classification call) per the doc's table (standard =
      Maker-Checker, above threshold = 6-Eyes). **Verified live**, plus
      a real credit-limit hold (`increase_credit_utilization`, retrofit
      once Module 7 existed to release it — see that module's own
      notes) and a real balanced GL posting on every post, not
      documented intent.
- [x] `[BUILD]` Milestone/escrow tranche release, cross-checked against
      Module 6's `DeliveryDispute` (real query, real gate — 6-Eyes per
      the doc's table). **Verified live**, both the blocked-by-dispute
      and cleared-to-proceed cases (Module 6's own section above).
- [x] `[BUILD]` Bulk/batch payment run: one request wrapping N line
      items, aggregate value drives governance routing even if
      individual lines are below threshold (real aggregate-sum check).
      **Verified live**.
- [x] `[BUILD]` **Commission waterfall engine** — the doc's central
      Mi-Pay-style mechanic, done for real: platform origination fee
      first, then override commissions cascading Super-Distributor →
      Regional Distributor → Originating Agent per a versioned
      `CommissionRule` rate schedule (historical transactions keep the
      rate that applied at execution time — real versioning, not just
      "current rate"). **Verified live** across several trade payments,
      real balances accumulating in each tier's wallet.
- [x] `[BUILD]` `PartnerWallet` per channel partner — real balance,
      credited atomically with the payment posting (same function/
      transaction, not a separate async step, matching the doc's "zero
      drift tolerance" requirement). **Verified live**.
- [x] `[BUILD]` Wallet withdrawal request (Maker-Checker) and
      commission dispute/adjustment workflow (6-Eyes, full audit trail
      of original vs. adjusted waterfall). **Verified live**: a
      below-minimum withdrawal applied directly; an above-minimum one
      routed through Maker-Checker; a commission dispute correctly
      required two distinct 6-Eyes approvers before
      `update_commission_dispute` adjusted the waterfall entry, moved
      the delta on the partner's wallet, and kept the original amount
      on the `CommissionDispute` row as the audit trail.
- [ ] `[SUBSTITUTED]` Nightly settlement-sweep batch job → no explicit
      `sweep_wallet_settlement` action exists (wallet balances are
      correct as credited; a real sweep-to-external-bank-account step
      was judged out of scope — there is no real bank leg to sweep to
      anywhere in this build). Deferred, not forgotten.
- Entities: `TradePayment`, `CommissionRule`, `CommissionWaterfallEntry`,
  `PartnerWallet`, `CommissionDispute`.
- Endpoints: `submit_/post_trade_payment`, `submit_/post_batch_payment`,
  `submit_/post_milestone_release`, `list_commission_waterfall_entry`,
  `list_/create_commission_rule`, `list_partner_wallet`,
  `create_wallet_withdrawal`, `create_commission_dispute`,
  `submit_commission_dispute_approval`, `update_commission_dispute`,
  `stat_/chart_` (total commissions paid, waterfall by tier).
- Governance: view wallet balance = none; standard payment =
  Maker-Checker; high-value payment = 6-Eyes; configure commission rate
  = Maker-Checker; approve commission dispute adjustment = 6-Eyes;
  wallet withdrawal above minimum = Maker-Checker.

### Module 5: Invoice Discounting & Supply Chain Finance
- [x] `[BUILD]` `Invoice` ingestion (direct submission — no ERP
      connectors exist; see Substituted below), `DiscountingBundle`
      selection/submission (fixed 3-invoice-slot bundling, same
      disclosed fixed-arity substitution as Module 4's batch payment —
      Nirdosha has no array parameter type). **Verified live**.
- [x] `[BUILD]` Double-discounting check: a real SQL exact-match query
      over (invoice_number, tax_id, supplier_code, face_value_cents)
      against already-on-file invoices — real duplicate-prevention
      logic, zero false negatives by construction. **Verified live**: a
      byte-for-byte repeat submission was correctly rejected. Not a
      `sha256_hex`-built composite key like Module 1's audit chain:
      Nirdosha has no i64→str cast (LANGUAGE.md §3), so the i64 amount
      can't be folded into a hashable string — the amount instead
      participates directly in the SQL match, same real guarantee, no
      Redis-backed hash index (`[SUBSTITUTED]`).
- [x] `[BUILD]` Inventory/delivery gating: discounting eligibility
      automatically held if Module 6 has an open dispute on the
      underlying delivery (real cross-module query, matches the doc's
      requirement exactly). **Verified live** (Module 6's own section
      above).
- [x] `[BUILD]` Risk-based interest pricing: advance rate (80–90%),
      platform fee, and a benchmark+spread interest rate computed from
      the bundle's primary invoice's buyer counterparty's latest
      Module 2 `FinancialStatement.credit_score` (one SQL join) — real
      arithmetic, not a stub. **Verified live**: a credit_score of
      10000 correctly produced the top advance-rate/spread tier (9000
      bps / 100 bps).
- [x] `[BUILD]` Single-period interest accrual (`interest_cents`,
      computed at bundle-creation time) feeding Module 7's
      `accrue_daily_interest` below — not a multi-row amortization
      schedule: Nirdosha has no loop-friendly "generate N periods"
      ergonomic beyond fixed arity, so this is one real, correctly
      computed figure rather than a full repayment table. **Verified
      live**.
- [x] `[MOCK]` `mock_generate_notice_of_assignment(invoice_id) ->
      Result(i64, str)` — a generated NoA text row (no PDF generation,
      no e-signature provider integrated); `acknowledge_notice_of_
      assignment` is a real recorded action, just not cryptographically
      signed. **Verified live**.
- [x] `[MOCK]` `mock_benchmark_rate_bps() -> i64` — stands in for a live
      SOFR/EURIBOR feed with a fixed illustrative rate, kept as basis
      points rather than the doc's literal `f64` so it composes with
      every other `_bps` rate in this file without an int↔float cast
      Nirdosha doesn't have.
- [ ] `[SUBSTITUTED]` SAP/Oracle/NetSuite/Dynamics ERP connectors,
      statistical fraud-scoring ML pipeline → direct invoice submission
      only; fraud flag is a simple rule (face value ≥ $500,000, a fixed
      illustrative band) rather than a trained anomaly model, clearly
      labeled as a heuristic not an ML score. **Verified live**.
- Entities: `Invoice`, `DiscountingBundle`, `FraudCase`,
  `NoticeOfAssignment`.
- Endpoints: `create_invoice`, `list_invoice`,
  `create_discounting_bundle`, `list_discounting_bundle`,
  `submit_discounting_bundle`, `fund_discounting_bundle`,
  `list_fraud_case`, `update_fraud_case`,
  `mock_generate_notice_of_assignment`, `list_notice_of_assignment`,
  `acknowledge_notice_of_assignment`, `mock_benchmark_rate_bps`,
  `stat_total_funded_cents`, `stat_double_discounting_catches` (open
  fraud-review count — the check itself rejects at insert time rather
  than leaving a countable row, see comment in source),
  `chart_dispute_by_type` (reused from Module 6).
- Governance: view eligible receivables = none; submit bundle for
  funding = Maker-Checker (6-Eyes above threshold, reusing Module 4's
  `required_eyes_for_amount`); clear fraud flag as false-positive =
  Maker-Checker (plain callable `update_fraud_case`, tier documented
  not wired — same per-module scoping as every other module); override
  a delivery-dispute discounting hold = 6-Eyes (not separately wired —
  the hold itself is real and enforced at `create_invoice` time; no
  distinct override action was built beyond what Module 6's own dispute
  resolution already unblocks).

### Module 7: General Ledger & Financial Reconciliation Engine
- [x] `[BUILD]` **Strict double-entry ledger core**: `post_journal_entry`
      rejects any entry where `debit_cents != credit_cents` — enforced
      structurally in Nirdosha code, exactly matching the doc's "zero
      unbalanced postings tolerated" requirement. This is the single
      highest-value piece of real logic in the whole build (the doc
      itself names ledger correctness as the top platform risk).
      **Verified live**: a manual entry submitted with independently
      chosen debit ($7.00) and credit ($9.00) amounts was correctly
      rejected with "unbalanced posting rejected" after full 1-eyes
      approval — the structural check runs at *apply* time, on real
      caller-supplied numbers, not just on a helper that's trivially
      balanced by construction. A balanced manual entry (equal amounts)
      posted correctly.
  - [x] `[BUILD]` Chart of Accounts (Asset/Liability/Revenue/Expense/
        Contingent — including an off-balance-sheet undrawn-LC account,
        seeded on first use same as Module 4's wallets), configurable,
        6-Eyes-gated structural changes. **Verified live**: a new
        account correctly required two distinct approvers before
        `apply_create_account` inserted it.
  - [x] `[BUILD]` Automated event→journal mapping: every real posting
        event from Modules 4/5 (trade payment, milestone release,
        interest accrual) posts a real balanced journal entry via one
        shared `post_journal_entry`, not copy-pasted per caller.
        **Verified live**, and confirmed structurally sound via
        `stat_gl_balance_status` (a real `SUM(debit) = SUM(credit)`
        check across every posted entry, not decoration).
  - [x] `[BUILD]` Daily interest accrual — exposed as an explicit
        callable action (no scheduler primitive, same substitution as
        elsewhere) rather than a background job; walks every
        funded/not-yet-accrued `DiscountingBundle` with the same
        while-loop-over-JSON-array pattern `verify_audit_chain_rows`
        already established (Module 1) for an unbounded result set.
        **Verified live**.
- [x] `[BUILD]` Deterministic payment matching: exact match on
      reference + amount against the open (`posted`) `TradePayment`
      set — real matching logic, not a stub. **Verified live**.
- [x] `[SUBSTITUTED]` Fuzzy/Levenshtein matching pass → unmatched
      payments route straight to the manual exception queue (no
      approximate-matching library available); a human resolves them
      via `submit_resolve_exception`/`resolve_exception`
      (Maker-Checker), per the doc's own fallback path. **Verified
      live**: a wrong-amount feed correctly routed to the exception
      queue rather than silently matching or erroring.
- [x] `[MOCK]` `mock_ingest_bank_feed(reference, amount_cents,
      debtor_code, source_feed) -> Result(i64,str)` — stands in for
      real ISO 20022 camt.053/054/MT940/MT950 webhook ingestion; plain
      scalar params rather than a `payload_json: json` blob, since
      `serve.rs`'s own param decoder (`decode_value`) has no arm for a
      raw `json`-typed top-level parameter at all — confirmed by
      reading the source, not assumed; same reasoning as every other
      mock endpoint in this file that takes named fields instead.
- [x] `[BUILD]` Instant credit-limit release on match (real call back
      into Module 2's `CreditLimit`). **Verified live** — and this
      required a matching real *hold*: `check_credit_hold` previously
      only ever read `utilized_cents` (nothing wrote to it), so Module
      4's `post_trade_payment_inner`/`post_milestone_release_inner`
      were retrofitted to call the new `increase_credit_utilization` at
      posting time, closing the loop this module's own release
      requires. One shared `settle_trade_payment_reconciliation` is the
      terminal "clear a matched payment" step, called by both this
      module's own bank-feed path and Module 8's virtual-account direct
      route (below) — one real function, not two copies.
- [x] `[MOCK]` FX: `mock_fx_rate_bps(from_currency, to_currency) -> i64`
      — a fixed illustrative cross-rate table (bps-scaled, no int↔float
      cast needed), stands in for a live FX feed; not wired into any
      posting path (no multi-currency booking/revaluation logic was
      built — a real, disclosed gap, not a naming accident).
- Entities: `JournalEntry`, `JournalLine`, `Account`,
  `ReconciliationMatch`, `ExceptionQueueItem`.
- Endpoints: `list_journal_entry`, `list_journal_line`,
  `submit_/apply_manual_journal_entry`, `list_/submit_/apply_account`
  (`submit_create_account`/`apply_create_account`), `list_
  reconciliation_match`, `list_exception_queue_item`,
  `mock_ingest_bank_feed`, `submit_resolve_exception`/
  `resolve_exception`, `accrue_daily_interest`, `mock_fx_rate_bps`,
  `stat_gl_balance_status`, `stat_reconciliation_match_rate_bps`,
  `stat_exception_queue_depth`, `chart_gl_by_account_type`.
- Governance: view journal entry = none; manual GL adjustment (any
  size) = Maker-Checker or 6-Eyes by amount via Module 4's shared
  `required_eyes_for_amount`; manually match exception = Maker-Checker;
  Chart of Accounts structural change = 6-Eyes.

---

## Phase 4 — Settlement & Insight

### Module 8: Payments, Settlement & Banking Gateways
- [x] `[BUILD]` `PaymentExecution` record per trade payment, real
      deterministic rail-selection logic (`select_rail`: cross-border →
      SWIFT; else by currency → FedNow/SEPA_INSTANT/RTGS/ACH), 6-Eyes
      for high-value cross-border per the doc. **Verified live**: a
      domestic USD payment correctly selected `FedNow`; a $60,000
      cross-border EUR payment correctly required 2 distinct approvers
      before posting.
- [x] `[BUILD]` `VirtualIban` registry (renamed from the doc's
      `VirtualIBAN` — `to_snake_case`'s actual per-uppercase-letter
      algorithm turns `IBAN` into `_i_b_a_n`, not `_iban`; confirmed by
      reading `ui_gen.rs`, not guessed — so the doc's own struct name
      would have silently produced no nav screen at all): one
      provisioned per buyer-seller relationship (real, caller-supplied
      `iban_number` — Nirdosha has no string concatenation to compose
      one internally), Maker-Checker to provision, 6-Eyes to close an
      account with real transaction history. **Verified live**, both
      close paths: an IBAN with a linked `PaymentExecution` correctly
      routed to a pending 6-Eyes request instead of closing; an unused
      one closed immediately (same dual-path idiom as Module 4's
      `create_wallet_withdrawal`).
- [x] `[BUILD]` Direct virtual-account payments route straight into
      Module 7's reconciliation via the shared
      `settle_trade_payment_reconciliation` (real function call, no
      generic statement-parsing detour) — matches the doc's stated
      design intent exactly.
- [x] `[MOCK]` `mock_execute_rail(rail, amount_cents, currency) ->
      Result(str,str)` — stands in for real FedNow/ACH/SEPA
      Instant/RTGS/SWIFT connectivity; returns a deterministic mock
      confirmation, same shape as `examples/payments_mock.nir`'s
      existing pattern. **Verified live**.
- [x] `[MOCK]` `mock_generate_pacs008(reference, amount_cents,
      currency) -> str` — delegates to Module 3's
      `mock_generate_swift_message` (msg_type `"pacs008"`) rather than
      a second logging table; same treatment as that module's own
      MT700/707/710/760 mocks. `mock_generate_mt103` was not built
      separately — `pacs008` is the one cross-border message this
      module actually generates on a payment execution; MT103 would be
      the identical pattern, deferred as redundant rather than
      forgotten.
- [x] `[SUBSTITUTED]` Real corridor-health monitoring against live
      rails → `CorridorHealth` is seeded with the doc's three named
      corridors (US-Domestic/FedNow, EU-Domestic/SEPA_INSTANT,
      Cross-Border/SWIFT) on first use and manually updated via
      `update_corridor_health`, not a live probe (no real rails to
      probe). **Verified live** (seeding).
- Entities: `PaymentExecution`, `VirtualIban`, `CorridorHealth`, reuses
  Module 3's `SwiftMessageLog`.
- Endpoints: `list_payment_execution`, `submit_/post_payment_execution`,
  `list_/update_corridor_health`, `list_/create_virtual_iban`,
  `close_virtual_iban`/`apply_close_virtual_iban`, `mock_execute_rail`,
  `mock_generate_pacs008`, `select_rail`, `stat_
  settlement_success_rate_bps`, `chart_corridor_transaction_value`.
- Governance: view corridor health = none; standard-value payment =
  Maker-Checker; high-value cross-border payment = 6-Eyes; provision
  virtual IBAN = Maker-Checker (plain callable, tier documented not
  wired — same per-module scoping as elsewhere); close non-zero-history
  IBAN = 6-Eyes (wired end-to-end, this module's marquee governance
  action alongside payment execution).

### Module 9: Integration, Analytics & External Reporting
- [x] `[BUILD]` Portfolio analytics dashboard: portfolio exposure,
      facility utilization, commission revenue (Module 4's own chart),
      corridor transaction value (Module 8's own chart) — real
      `stat_`/`chart_` queries over the ledger/payments/commission data
      already built in Phases 1–3. **Verified live**. DSO specifically
      was not built: it needs date arithmetic (days between invoice and
      payment dates) and Nirdosha has no wall-clock/date builtin at all
      (same substitution as Module 1's SLA-deadline gap) — a real,
      disclosed gap, not an oversight.
- [x] `[BUILD]` Reconciliation metrics visualizer (reuses Module 7's
      match-rate/exception-depth data as charts/tiles directly, not a
      second copy of the same queries).
- [x] `[BUILD]` `ApiKey` issuance/revocation (Maker-Checker per the
      doc), `WebhookSubscription` registration (Maker-Checker) — real
      records, though no actual outbound webhook delivery engine exists
      (see Substituted). **Verified live**.
- [x] `[MOCK]` `mock_basel_extract() -> i64` / `mock_aml_export() ->
      i64` — a real *computed* SQL aggregate (total credit exposure +
      undrawn LC commitments × a fixed illustrative risk-weight
      density for Basel; sanctions-override count + high-risk
      counterparty count for AML), not a literal stub number, but
      returning a plain `i64` rather than the doc's literal
      `Result(json,str)`: Nirdosha has no i64→str cast, so a computed
      number can never be spliced into a JSON string literal at all —
      the same constraint every other module in this file works around
      the same way (a plain number, or persisted to a `list_`-able
      table). Full detail persists to `RegulatoryExport` instead.
      **Verified live**. Explicitly labeled **not for actual regulatory
      filing** in the source, per the doc's own caveat.
- [x] `[BUILD]` Regulatory export trigger — this module's marquee
      governance wiring (6-Eyes per the doc's table, the one action the
      doc itself calls out as high-risk): `submit_regulatory_export`
      opens the request; `generate_regulatory_export` only actually
      computes and records the export once two distinct approvers have
      approved it. **Verified live**.
- [ ] `[SUBSTITUTED]` Real signed-webhook delivery with exponential
      backoff, developer sandbox with synthetic-data reset, OpenAPI
      self-service docs → `WebhookSubscription` records exist and are
      manageable, but nothing is actually delivered (no outbound HTTP
      dispatch loop built); the platform's own `nirdosha serve`
      endpoints already are the API (no separate gateway/sandbox
      environment).
- Entities: `ApiKey`, `WebhookSubscription`, `RegulatoryExport`, reuses
  `stat_`/`chart_` functions as `AnalyticsSnapshot` equivalents.
- Endpoints: `list_/create_/delete_api_key`,
  `list_/create_webhook_subscription`, `mock_basel_extract`,
  `mock_aml_export`, `list_regulatory_export`, `submit_
  regulatory_export`, `generate_regulatory_export`,
  `stat_total_portfolio_exposure_cents`, `stat_facility_
  utilization_bps`, `chart_portfolio_by_module`.
- Governance: view dashboard = none; generate API key = Maker-Checker;
  revoke API key = Maker-Checker (both plain callable, tier documented
  not wired — same per-module scoping as elsewhere); trigger
  Basel/AML export = 6-Eyes (wired end-to-end); modify production
  webhook = Maker-Checker.

---

## Cross-cutting items (apply once, referenced by every module above)

- [x] `[BUILD]` Role vocabulary extended in `examples/identity_mock.nir`:
      admin, analyst (existing) + system_admin, corporate_seller,
      corporate_buyer, bank_ops, compliance_officer, channel_partner —
      matching the doc's actual RBAC role list. Verified live (used
      `compliance_officer`/`bank_ops` logins above).
- [x] `[BUILD]` Shared approval primitive (`submit_approval`/
      `decide_approval`, Module 1) reused by name from every other
      module's action-specific wrapper functions, not reimplemented —
      confirmed across all nine modules' submit/apply pairs, including
      the four added this session (Modules 5/7/8/9).
- [x] `[BUILD]` Every state-changing function that goes through the
      shared approval primitive writes an `AuditLogEntry` via
      `finish_with_audit` (Module 1) — a real, checked convention for
      every governed action across all nine modules. Plain (ungoverned)
      CRUD actions such as `create_counterparty` or `list_*` do not
      individually audit-log each call; only governance-primitive
      transitions (submit/decide/finish) do, per the doc's own emphasis
      on auditing *approval and state-changing* actions, not read
      traffic. `verify_audit_chain` stayed intact (-1) across 100+ mixed
      actions spanning every module in this session's verification
      pass.
- [x] `[SUBSTITUTED]` platform-wide: Java Spring Boot / Angular /
      Kotlin+Swift / Kubernetes / Camunda / Drools / Kafka / Redis
      locking / HashiCorp Vault / TigerBeetle → one Nirdosha `.nir`
      file, `nirdosha serve`, SQLite, `emit-ui`, `mq` (Redis, used
      narrowly per Module 6's note above), and the ledger/approval
      logic written directly in Nirdosha rather than via Drools/
      Camunda rule/workflow engines.
- [x] Screenshots of all 33 `emit-ui`-derived screens (Dashboard + 32
      per-struct nav entries), captured live against a populated
      `trade_finance.db` via Chrome DevTools MCP, one PNG per screen:
      `examples/trade-finance/screenshots/`.
- [x] `[BUILD]` Real, server-enforced per-module read RBAC: every
      nav-backing `list_`/`get_` fn (31 of them; `get_decision_link` is
      the one deliberate exception, already self-scoped by
      `actor_subject` and not a nav screen at all) now carries
      `requires(role: ...)` — a direct API call by an unauthorized role
      gets a real 403, not just a hidden nav item. `requires()` itself
      takes exactly one role string (`ast.rs::Requirement::Role`), but
      the check is array-membership (`identity_has_role`), so
      `identity_mock.nir` mints `system_admin`/`analyst` with every
      module-owning role baked into their token's `roles` array
      (broad read, `analyst`'s array deliberately omitting the literal
      `system_admin` — the one exception, `list_api_key`/
      `list_webhook_subscription` stay `system_admin`-only), and a
      synthetic `trade_party` role (no real persona — added to
      `corporate_seller`/`corporate_buyer`/`bank_ops`/
      `compliance_officer`'s arrays) so Module 2's `list_counterparty`,
      Module 3 (dual buyer+seller instruments), and Module 6
      (delivery/dispute) can gate to one shared literal instead of
      needing real OR-of-roles support. No grammar/parser/`serve.rs`
      change — pure data change to `identity_mock.nir`'s existing
      `match` arms plus one `requires(...)` clause per fn. Verified live
      (curl matrix: `seller1` 403 on `list_account`, `comp1` 200;
      `bank1` 200 on `list_counterparty` via the `trade_party` umbrella;
      `admin1` 200 / `comp1` 403 on `list_api_key`). Full per-module
      role table and reasoning: see "Demo login credentials" below.
      **Disclosed, unfixed limitation**: no row/tenant-level scoping —
      a role granted a module's read sees every row in it, not just
      rows tied to that caller (e.g. `channel_partner` sees every
      partner's wallet balance). Real scoping needs a caller-identity
      to counterparty/partner mapping that doesn't exist in the schema
      today; out of scope for this pass.
- [x] `[BUILD]` Field-level RBAC (`screen <Struct> { field <name> {
      view: role(...), edit: role(...) } }`) now actually enforced, not
      just parsed/typechecked — see `LANGUAGE.md`'s `screen`/`dashboard`
      section and `compiler/UI_DSL_TODO.md`'s BUILT list for the full
      client+server design. One worked example in this app:
      `Counterparty.risk_rating` (`view: role("compliance_officer",
      "bank_ops")`, `edit: role("compliance_officer")`) — verified live
      through both the normal UI/API path and `/_nirdosha/table/
      counterparty` directly.

## Demo login credentials

No persistent user store exists (or is needed) — `examples/
identity_mock.nir`'s `/api/login` mints a real signed token for *any*
`{subject, role}` pair, matched against a fixed role vocabulary
(`identity_mock.nir`'s own doc comment). These are just convenient,
memorable pairs spanning every role this app's `requires(role: ...)`
gates actually check, for manual testing via the login screen at
`http://localhost:8090/#/login` (subject can be anything memorable;
role must be exact):

Read access to every nav-backing `list_`/`get_` fn is now genuinely
role-gated (`requires(role: ...)`, real 403s on a direct API call too,
not just a hidden nav item) — see the "Cross-cutting items" section
above for the full reasoning.
`identity_mock.nir` mints `system_admin`/`analyst` tokens with every
module-owning role baked into their `roles` array (broad read, checked
by array membership not equality), and `corporate_seller`/
`corporate_buyer`/`bank_ops`/`compliance_officer` each also carry a
synthetic `trade_party` role so Module 2's `list_counterparty` (basic
KYC data all four legitimately need) and Module 3/Module 6 (genuinely
dual-sided data) can gate to one shared literal.

| subject | role | can do |
|---|---|---|
| `admin1` | `system_admin` | reads every module (broadest role array); the one thing `analyst` *can't* also do: `list_api_key`/`list_webhook_subscription` (credential material, `system_admin`-only even for analyst) |
| `bank1` | `bank_ops` | `list_trade_payment`/`list_payment_execution`/`list_corridor_health`/`list_virtual_iban`/`list_discrepancy_check_result`/`list_swift_message_log`, plus `trade_party`-gated Module 2/3/6 reads (counterparty, PO/LC, goods-receipt/delivery-dispute); trade payment/LC/milestone submission, payment execution, most day-to-day operator actions |
| `comp1` | `compliance_officer` | `list_audit_log`/`list_approval`/`list_credit_limit`/`list_financial_statement`/`list_fraud_case`/GL module (`list_account` etc.)/`list_regulatory_export`, plus `trade_party`-gated `list_counterparty` (KYC profile); `create_credit_limit`, `create_commission_rule`, sanctions-override/regulatory-export approvals |
| `seller1` | `corporate_seller` | `list_invoice`/`list_discounting_bundle`/`list_notice_of_assignment` (seller-owned, invoice discounting is seller-side per the doc), plus `trade_party`-gated Module 2/3/6 reads (counterparty, PO/LC, goods-receipt/dispute) |
| `buyer1` | `corporate_buyer` | same `trade_party`-gated Module 2/3/6 reads as `seller1` (counterparty, PO/LC, goods-receipt/dispute); no invoice-discounting read (seller-side only) |
| `agent1` | `channel_partner` | `list_commission_rule`/`list_commission_waterfall_entry`/`list_partner_wallet`/`list_commission_dispute` — channel-partner-facing flows (commission/wallet); **note (0.4, disclosed, unfixed by this pass):** no row-scoping exists yet, so this role currently sees *every* partner's wallet balance, not just its own |
| `analyst1` | `analyst` | read-oriented; same broad module access as `system_admin` except `list_api_key`/`list_webhook_subscription`; used across `store.nir`/`rev_assurance.nir` too |

`Counterparty.risk_rating` additionally has field-level RBAC (`screen
Counterparty { field risk_rating { view: role(...) edit: role(...) } }`)
— `bank1`/`comp1` can see it, `seller1`/`buyer1` cannot (redacted `null`
in every response, including `/_nirdosha/table/counterparty`); only
`comp1` can change it via `update_counterparty` — anyone else's
submitted value for that one field is rejected if it differs from
what's stored, even though the rest of the update goes through.

Any Maker-Checker/6-Eyes flow needs **two or three distinct subjects**
deciding (not roles — segregation-of-duties in this app is by identity,
"regardless of role assignment," per the doc) — e.g. `bank1` submits,
`comp1` and `seller1` decide, for a 2-of-2.

## Session notes (read before resuming)

- Struct names must snake_case-match their `list_<name>`/etc. functions
  exactly for `emit-ui` to derive a nav screen at all — `ApprovalRequest`
  silently produced no screen until renamed to `Approval` (same for
  `AuditLogEntry` → `AuditLog`). Check this for every new struct.
  `to_snake_case` (`ui_gen.rs`) inserts `_` before **every** uppercase
  letter, not just at word boundaries — `VirtualIBAN` → `virtual_i_b_a_n`,
  not `virtual_iban`; `CommissionWaterfallEntry`/`ExceptionQueueItem`
  hit the same trap the other way (the *function* name was missing a
  word, e.g. `list_exception_queue` instead of
  `list_exception_queue_item`). Found by writing a small script that
  replicates the algorithm and diffs every struct against its expected
  `list_` function — do that for every new struct/module rather than
  eyeballing it; three real gaps turned up this way across this
  session's four new modules plus one pre-existing one (Module 4's
  `CommissionWaterfallEntry`, fixed alongside).
- `governance_schema`-style shared "create tables" helpers **cannot**
  take `conn: db` as a parameter if the caller uses `conn` again
  afterward — inline the `CREATE TABLE IF NOT EXISTS` statements into
  every function that needs them instead (confirmed the hard way,
  twice). The one exception: a helper that takes `conn` as its
  *genuine last use* (nothing after it in the caller) — see
  `finish_with_audit`'s pattern, reuse it. The same rule applies inside
  a loop: a per-iteration helper that takes and stops its own `conn`
  (Module 4's `credit_wallet_tier` pattern) is correct; passing the
  *outer* function's own `conn` into a loop-body helper is not — it
  gets moved on the first iteration (or the first taken branch of a
  `match`) and a later unconditional `stop conn` on that same variable
  then fails ownership checking ("use of `conn` after it was moved"),
  even on a `match` arm that itself never touched `conn` — the checker
  isn't branch-uniform (restated from below, but this is the shape it
  actually took building Module 7's `accrue_daily_interest`).
- `audited` is a reserved keyword (`audited "justification" { ... }`) —
  don't use it as a variable name.
- `match` arms must be a single expression — no `return` inside one, no
  `{ multi; statements }` block (confirmed against `parser.rs`'s own
  grammar comment: `variant_arm ::= ident (...) "=>" expr`, and `expr`
  itself has no bare-block alternative — only `if`/`while`/`fn` embed a
  `block` as part of *their own* grammar). `if`/`else` used as an
  expression *does* allow full blocks. Assignment (`x = ...`) cannot
  take a `match` expression as its right-hand side directly — bind the
  match to a fresh `let` first, then assign the plain identifier. When a
  match arm needs to do more than one thing, factor that arm's logic
  into its own named function and call it as the arm's single
  expression (this file now has several small `_inner`-adjacent helpers
  that exist for exactly this reason, not because the logic itself
  needed splitting).
- Every server process (`identity_mock` on 9090, `trade_finance` on
  8090) needs restarting after a `.nir` edit — `nirdosha serve` reads
  the file once at startup, not per-request.
- A bare `curl -s URL` (no `-d`) issues a GET, and `serve.rs` only ever
  routes `POST /api/<fn>` — a no-argument read like `list_approval`
  still needs `curl -X POST` (or any `-d`, even `-d '{}'`) or it comes
  back "not found" in a way that looks like a routing bug but isn't.
  Login's response envelope key is lowercase `"ok"`/`"err"`, not
  `"Ok"`/`"Err"`.
- A struct-typed parameter (e.g. `create_goods_receipt_note(g:
  GoodsReceiptNote)`) must be posted as `{"g": {...fields...}}`, not the
  fields flattened at the top level — the parameter name is a real JSON
  key, not sugar.
- `cargo test --release` briefly failed to even build (`tests/
  codegen.rs` had a stray literal `...` near its end at one point this
  session) — pre-existing, uncommitted working-tree state not touched
  by this session's own edits; resolved itself (by the time of the UI
  feature work below, `cargo test --release --no-fail-fast` builds and
  runs the full suite cleanly again, 138/138 in that one file). Only
  remaining failure across the whole suite is `tests/concurrency.rs`'s
  `a_panic_inside_a_spawned_function_surfaces_as_thread_panicked`,
  confirmed pre-existing and unrelated (reproduces identically on a
  clean `git stash` of every change this session made).
- Regenerate `all_examples.md` (repo root) after any `.nir` example
  edit — it's a plain concatenated dump (`find benchmarks/nirdosha
  compiler/examples -name '*.nir' | sort`, then one `## <path>` heading
  + ```nir fenced block per file) with no build step of its own; it
  will silently go stale otherwise.

## Status

Phases 0-4 are complete: all nine modules are `[BUILD]`/`[MOCK]`/
`[SUBSTITUTED]` per the disposition framework above, and every checked
item has been run and verified live (curl + browser), not just
compiled. Android/iOS-specific mobile-app user stories and engineering
tasks named per-module in the source doc were deliberately not built —
out of scope per the platform-wide `[SUBSTITUTED]` note (one responsive
`emit-ui` web app stands in for web + native mobile parity). Remaining
open items are the individually-disclosed `[ ]` gaps listed inline
above (SLA-deadline computation, dynamic policy matrix, multi-tenant
segregation, DSO, live FX booking, real webhook/carrier/IoT feeds, and
similar — each is a genuine, named substitution, not a silent one).
33 UI screenshots (Dashboard + one per struct) live in
`examples/trade-finance/screenshots/`.

## UI/UX feature pass (module nav, pagination, typed edit controls)

Added as language/codegen defaults (not one-off template tweaks), per
`/home/arun/.claude/plans/encapsulated-forging-puffin.md`:

- **`module "Name" { ... }`** (new keyword, `LANGUAGE.md` SS12,
  `GRAMMAR.md`'s `module_decl`): this file's 9 `// Module N: ...` banner
  sections now wrap real `module` blocks; `emit-ui`'s nav groups into
  collapsible primary/secondary sections by it. Verified live (all 9
  modules render as collapsible sections; active one auto-expands).
- **Categorical/ordinal dropdowns**: real zero-payload enums now
  round-trip through JSON decode + SQLite bind (`interpreter.rs::
  sql_bind_params`, `serve.rs::decode_enum_value`) — a genuine
  interpreter/server fix, not new business logic. Migrated:
  `Counterparty.counterparty_type`/`risk_rating`,
  `DeliveryDispute.dispute_type`, `Account.account_type`,
  `CorridorHealth.rail_type`. Deliberately **not** migrated: internal
  state-machine `status` fields (pervasive `==`/`!=` throughout, no
  real UI payoff since they're system-set) and `tier_name`/
  `export_type` (passed as plain `str` across several function
  signatures with literal-string call sites — more entangled than a
  quick grep suggested, left as `str` per the plan's own guidance
  rather than forced). Verified live: dropdown renders searchable,
  round-trips through create+update, invalid variant gives a clean 400.
- **Temporal fields**: naming-convention heuristic only (`date`/`time`
  as a whole word-segment in a `str` field name) — no new language
  type; Nirdosha's no-wall-clock stance is deliberate (LANGUAGE.md SS9).
- **Pagination/sort/search/filter**: `nirdosha serve --db <path>` new
  flag exposes `/_nirdosha/table/<snake>` (`serve.rs::
  dispatch_table_query`), on by default for every table when passed,
  bypassing the interpreter entirely (allowlisted identifiers, bound
  values). Without `--db`, every table renders exactly as before.
  Verified live including the adversarial injection tests (bad
  `sort_field`/`filters` keys → clean 400, table intact).
- **Horizontal scroll, boolean True/False display, nav label spacing,
  id hidden in edit forms, role-based nav visibility**: all built and
  verified live (the last one via a throwaway `requires(role: "admin")`
  test file, since no `list_`/`get_` in this app is itself role-gated).
- Full re-verification after all of the above: `cargo build`/`cargo
  test` clean, fresh `trade_finance.db`, core flows re-run via curl,
  module nav + enum dropdowns + id-hiding + role-gating verified live
  in a real browser (Chrome DevTools MCP).
