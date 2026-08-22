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
- [ ] `[BUILD]` `PurchaseOrder` with `workflow_variant` (ADVANCE_PAYMENT
      / OPEN_ACCOUNT / DOCUMENTARY_COLLECTION / LC_BACKED /
      MILESTONE_ESCROW) — all five variants named in the doc, as real
      selectable/stored state, not prose.
- [ ] `[BUILD]` Payment-method decision engine: a real, deterministic
      rule function (counterparty trust tier, credit rating, order
      size, bank-guarantee requirement → recommended variant), with the
      final choice always a separate human-recorded field (matches the
      doc's "recommendation, not automatic" rule).
- [ ] `[BUILD]` `LetterOfCredit` lifecycle state machine (applied →
      advised → amended → presented → discrepancy-checked → settled),
      Maker-Checker/6-Eyes per the doc's table (issuance = 6-Eyes,
      amendment = Maker-Checker).
- [ ] `[BUILD]` UCP 600 discrepancy checker: a real rule function
      cross-checking presented document fields (quantities, dates,
      party names) against the PO/LC, producing a real
      `DiscrepancyCheckResult` row per field — the actual comparison
      logic, not a stub.
- [ ] `[MOCK]` `mock_generate_swift_message(msg_type, fields_json) ->
      str` — stands in for real Prowide/SWIFT Alliance Access
      transmission for MT700/MT707/MT710/MT760; logs a
      `SwiftMessageLog` row with the real semantic fields (not
      wire-format bytes, not actually transmitted anywhere).
- [ ] `[SUBSTITUTED]` OCR/ML line-item extraction from scanned trade
      documents → structured fields entered directly (same reasoning as
      Module 2 — no vision pipeline, no file upload yet).
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
- [ ] `[BUILD]` `GoodsReceiptNote` ingestion (web form; per-line
      received quantity + condition).
- [ ] `[BUILD]` Three-way match engine: one query/comparison per line
      item across PO quantity, B/L quantity (from the LC/PO record),
      and GRN received quantity, computing a real variance % against a
      configurable tolerance — real reconciliation math, not a stub.
- [ ] `[BUILD]` Auto-confirmation within tolerance (no human action,
      `status = "auto_confirmed"`) vs. dispute routing beyond tolerance
      — matches the doc's governance table exactly (auto = none inside
      tolerance; Maker-Checker to manually confirm outside it).
- [ ] `[BUILD]` `DeliveryDispute` workflow (short/damage/over), carrier
      claim initiation, Maker-Checker gated.
- [ ] `[BUILD]` Real cross-module gate: an open dispute on a line item
      is checked synchronously by Module 5's discounting-eligibility
      function and Module 4's milestone-release function (a real
      function call/query, not just documented intent).
- [ ] `[BUILD]` `mq_publish("inventory.grn.confirmed", ...)` on
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
- [ ] `[BUILD]` Unified `TradePayment` initiation across all five
      workflow variants from Module 3, with governance routing
      (Module 1 classification call) per the doc's table (standard =
      Maker-Checker, above threshold = 6-Eyes).
- [ ] `[BUILD]` Milestone/escrow tranche release, cross-checked against
      Module 6's `DeliveryDispute` (real query, real gate — 6-Eyes per
      the doc's table).
- [ ] `[BUILD]` Bulk/batch payment run: one request wrapping N line
      items, aggregate value drives governance routing even if
      individual lines are below threshold (real aggregate-sum check).
- [ ] `[BUILD]` **Commission waterfall engine** — the doc's central
      Mi-Pay-style mechanic, done for real: platform origination fee
      first, then override commissions cascading Super-Distributor →
      Regional Distributor → Originating Agent per a versioned
      `CommissionRule` rate schedule (historical transactions keep the
      rate that applied at execution time — real versioning, not just
      "current rate").
- [ ] `[BUILD]` `PartnerWallet` per channel partner — real balance,
      credited atomically with the payment posting (same function/
      transaction, not a separate async step, matching the doc's "zero
      drift tolerance" requirement).
- [ ] `[BUILD]` Wallet withdrawal request (Maker-Checker) and
      commission dispute/adjustment workflow (6-Eyes, full audit trail
      of original vs. adjusted waterfall).
- [ ] `[SUBSTITUTED]` Nightly settlement-sweep batch job → an explicit
      `sweep_wallet_settlement` callable action (no scheduler
      primitive).
- Entities: `TradePayment`, `CommissionRule`, `CommissionWaterfallEntry`,
  `PartnerWallet`.
- Endpoints: `create_trade_payment`, `create_batch_payment`,
  `list_commission_waterfall`, `list_/create_commission_rule`,
  `list_partner_wallet`, `create_wallet_withdrawal`,
  `create_commission_dispute`, `update_commission_dispute`,
  `stat_/chart_` (total commissions paid, waterfall by tier).
- Governance: view wallet balance = none; standard payment =
  Maker-Checker; high-value payment = 6-Eyes; configure commission rate
  = Maker-Checker; approve commission dispute adjustment = 6-Eyes;
  wallet withdrawal above minimum = Maker-Checker.

### Module 5: Invoice Discounting & Supply Chain Finance
- [ ] `[BUILD]` `Invoice` ingestion (direct submission — no ERP
      connectors exist; see Substituted below), `DiscountingBundle`
      selection/submission.
- [ ] `[BUILD]` Double-discounting check: a real hash/lookup over
      (invoice_id, tax_id, supplier_code, total_amount) against
      already-funded invoices — real duplicate-prevention logic, zero
      false negatives by construction (exact match, not probabilistic).
- [ ] `[BUILD]` Inventory/delivery gating: discounting eligibility
      automatically held if Module 6 has an open dispute on the
      underlying delivery (real cross-module query, matches the doc's
      requirement exactly).
- [ ] `[BUILD]` Risk-based interest pricing: advance rate (80–90%),
      platform fee, and risk-retention reserve computed from a
      benchmark rate + buyer risk spread — real arithmetic.
- [ ] `[BUILD]` Amortization schedule generation feeding Module 7's
      interest-accrual entries.
- [ ] `[MOCK]` `mock_generate_notice_of_assignment(invoice_id) -> str`
      — a generated NoA text (no PDF generation, no e-signature
      provider integrated); buyer acknowledgement is a real recorded
      action (`update` on the NoA record), just not cryptographically
      signed.
- [ ] `[MOCK]` `mock_benchmark_rate() -> f64` — stands in for a live
      SOFR/EURIBOR feed with a fixed illustrative rate.
- [ ] `[SUBSTITUTED]` SAP/Oracle/NetSuite/Dynamics ERP connectors,
      statistical fraud-scoring ML pipeline → direct invoice submission
      only; fraud flag is a simple rule (e.g. amount far outside a
      seller's historical range) rather than a trained anomaly model,
      clearly labeled as a heuristic not an ML score.
- Entities: `Invoice`, `DiscountingBundle`, `FraudCase`,
  `NoticeOfAssignment`.
- Endpoints: `create_invoice`, `list_invoice`,
  `create_discounting_bundle`, `submit_discounting_bundle`,
  `list_fraud_case`, `update_fraud_case`,
  `mock_generate_notice_of_assignment`,
  `acknowledge_notice_of_assignment`, `stat_/chart_` (total funded,
  double-discounting catches).
- Governance: view eligible receivables = none; submit bundle =
  Maker-Checker; approve disbursement above threshold = 6-Eyes; clear
  fraud flag as false-positive = Maker-Checker; override a
  delivery-dispute discounting hold = 6-Eyes.

### Module 7: General Ledger & Financial Reconciliation Engine
- [ ] `[BUILD]` **Strict double-entry ledger core**: `post_journal_entry`
      rejects any entry where `sum(debits) != sum(credits)` — enforced
      structurally in Nirdosha code, exactly matching the doc's "zero
      unbalanced postings tolerated" requirement. This is the single
      highest-value piece of real logic in the whole build (the doc
      itself names ledger correctness as the top platform risk).
  - [ ] `[BUILD]` Chart of Accounts (Asset/Liability/Revenue/Expense/
        Contingent — including off-balance-sheet undrawn-LC and escrow
        accounts), configurable, 6-Eyes-gated structural changes.
  - [ ] `[BUILD]` Automated event→journal mapping: every real posting
        event from Modules 3/4/5/6/8 above (LC fee, commission entry,
        trade payment, milestone release) posts a real balanced journal
        entry via one shared function, not copy-pasted per caller.
  - [ ] `[BUILD]` Daily interest accrual — exposed as an explicit
        callable action (no scheduler primitive, same substitution as
        elsewhere) rather than a background job.
- [ ] `[BUILD]` Deterministic payment matching: exact match on
      reference/amount/debtor code against the open exception queue —
      real matching logic.
- [ ] `[SUBSTITUTED]` Fuzzy/Levenshtein matching pass → unmatched
      payments route straight to the manual exception queue (no
      approximate-matching library available); a human resolves them,
      per the doc's own fallback path.
- [ ] `[MOCK]` `mock_ingest_bank_feed(payload_json) -> Result(i64,str)`
      — stands in for real ISO 20022 camt.053/054/MT940/MT950 webhook
      ingestion; accepts a simplified JSON shape (reference, amount,
      debtor code) instead of parsing real ISO 20022/SWIFT MT wire
      formats.
- [ ] `[BUILD]` Instant credit-limit release on match (real call back
      into Module 2's `CreditLimit`).
- Entities: `JournalEntry`(+lines), `Account`, `ReconciliationMatch`,
  `ExceptionQueueItem`.
- Endpoints: `list_journal_entry`, `create_manual_journal_entry`,
  `list_/create_account`, `list_exception_queue`,
  `mock_ingest_bank_feed`, `resolve_exception`, `stat_/chart_`
  (balance-check status, match rate, exception-queue depth, GL by
  account type).
- Governance: view journal entry = none; manual GL adjustment below
  threshold = Maker-Checker; above threshold = 6-Eyes; manually match
  exception = Maker-Checker; Chart of Accounts structural change =
  6-Eyes.

---

## Phase 4 — Settlement & Insight

### Module 8: Payments, Settlement & Banking Gateways
- [ ] `[BUILD]` `PaymentExecution` record per trade payment, real rail
      selection logic (currency/amount/destination → domestic vs.
      cross-border), 6-Eyes for high-value cross-border per the doc.
- [ ] `[BUILD]` `VirtualIBAN` registry: one provisioned per
      buyer-seller relationship at onboarding completion (real
      deterministic mapping/lookup), Maker-Checker to provision,
      6-Eyes to close a non-zero-history account.
- [ ] `[BUILD]` Direct virtual-account payments route straight into
      Module 7's reconciliation (real function call, no generic
      statement-parsing detour) — matches the doc's stated design
      intent exactly.
- [ ] `[MOCK]` `mock_execute_rail(rail, amount, currency) -> Result(str,str)`
      — stands in for real FedNow/ACH/SEPA Instant/RTGS/SWIFT
      connectivity; returns a deterministic mock confirmation, same
      shape as `examples/payments_mock.nir`'s existing pattern.
- [ ] `[MOCK]` `mock_generate_pacs008(fields_json) -> str` /
      `mock_generate_mt103` — same treatment as Module 3's SWIFT mock.
- [ ] `[SUBSTITUTED]` Real corridor-health monitoring against live
      rails → `CorridorHealth` is a manually/periodically updated
      status record, not a live probe (no real rails to probe).
- Entities: `PaymentExecution`, `VirtualIBAN`, `CorridorHealth`,
  reuses Module 3's `SwiftMessageLog`.
- Endpoints: `create_payment_execution`, `list_corridor_health`,
  `create_virtual_iban`, `list_virtual_iban`, `close_virtual_iban`,
  `mock_execute_rail`, `mock_generate_pacs008`.
- Governance: view corridor health = none; standard-value payment =
  Maker-Checker; high-value cross-border payment = 6-Eyes; provision
  virtual IBAN = Maker-Checker; close non-zero-history IBAN = 6-Eyes.

### Module 9: Integration, Analytics & External Reporting
- [ ] `[BUILD]` Portfolio analytics dashboard: DSO, facility
      utilization, portfolio exposure, commission revenue trend,
      corridor transaction value — real `stat_`/`chart_` queries over
      the ledger/payments/commission data already built in Phases 1–3
      (this module is mostly "wire up dashboards over what already
      exists," which is exactly what `stat_`/`chart_` derivation is
      for).
- [ ] `[BUILD]` Reconciliation metrics visualizer (reuses Module 7's
      match-rate/exception-depth data as charts).
- [ ] `[BUILD]` `ApiKey` issuance/revocation (Maker-Checker per the
      doc), `WebhookSubscription` registration (Maker-Checker) — real
      records, though no actual outbound webhook delivery engine exists
      (see Substituted).
- [ ] `[MOCK]` `mock_basel_extract() -> Result(json,str)` /
      `mock_aml_export() -> Result(json,str)` — a realistically-shaped
      RWA/CRM/AML extract computed from real ledger/exposure data,
      explicitly labeled as illustrative and **not for actual
      regulatory filing**. 6-Eyes-gated per the doc.
- [ ] `[SUBSTITUTED]` Real signed-webhook delivery with exponential
      backoff, developer sandbox with synthetic-data reset, OpenAPI
      self-service docs → `WebhookSubscription` records exist and are
      manageable, but nothing is actually delivered (no outbound HTTP
      dispatch loop built); the platform's own `nirdosha serve`
      endpoints already are the API (no separate gateway/sandbox
      environment).
- Entities: `ApiKey`, `WebhookSubscription`, `RegulatoryExport`,
  reuses `stat_`/`chart_` functions as `AnalyticsSnapshot` equivalents.
- Endpoints: `list_/create_/delete_api_key`,
  `list_/create_webhook_subscription`, `mock_basel_extract`,
  `mock_aml_export`, plus the module's own `stat_`/`chart_` set.
- Governance: view dashboard = none; generate API key =
  Maker-Checker; revoke API key = Maker-Checker; trigger
  Basel/AML export = 6-Eyes; modify production webhook = Maker-Checker.

---

## Cross-cutting items (apply once, referenced by every module above)

- [x] `[BUILD]` Role vocabulary extended in `examples/identity_mock.nir`:
      admin, analyst (existing) + system_admin, corporate_seller,
      corporate_buyer, bank_ops, compliance_officer, channel_partner —
      matching the doc's actual RBAC role list. Verified live (used
      `compliance_officer`/`bank_ops` logins above).
- [ ] `[BUILD]` Shared approval primitive (`submit_approval`/
      `decide_approval`, Module 1) reused by name from every other
      module's action-specific wrapper functions, not reimplemented.
- [ ] `[BUILD]` Every state-changing function writes an
      `AuditLogEntry` (Module 1) — a real, checked convention, not
      optional per-module.
- [ ] `[SUBSTITUTED]` platform-wide: Java Spring Boot / Angular /
      Kotlin+Swift / Kubernetes / Camunda / Drools / Kafka / Redis
      locking / HashiCorp Vault / TigerBeetle → one Nirdosha `.nir`
      file, `nirdosha serve`, SQLite, `emit-ui`, `mq` (Redis, used
      narrowly per Module 6's note above), and the ledger/approval
      logic written directly in Nirdosha rather than via Drools/
      Camunda rule/workflow engines.

## Session notes (read before resuming)

- Struct names must snake_case-match their `list_<name>`/etc. functions
  exactly for `emit-ui` to derive a nav screen at all — `ApprovalRequest`
  silently produced no screen until renamed to `Approval` (same for
  `AuditLogEntry` → `AuditLog`). Check this for every new struct.
- `governance_schema`-style shared "create tables" helpers **cannot**
  take `conn: db` as a parameter if the caller uses `conn` again
  afterward — inline the `CREATE TABLE IF NOT EXISTS` statements into
  every function that needs them instead (confirmed the hard way,
  twice). The one exception: a helper that takes `conn` as its
  *genuine last use* (nothing after it in the caller) — see
  `finish_with_audit`'s pattern, reuse it.
- `audited` is a reserved keyword (`audited "justification" { ... }`) —
  don't use it as a variable name.
- `match` arms must be a single expression — no `return` inside one, no
  `{ multi; statements }` block. `if`/`else` used as an expression *does*
  allow full blocks. Assignment (`x = ...`) cannot take a `match`
  expression as its right-hand side directly — bind the match to a
  fresh `let` first, then assign the plain identifier.
- Every server process (`identity_mock` on 9090, `trade_finance` on
  8090) needs restarting after a `.nir` edit — `nirdosha serve` reads
  the file once at startup, not per-request.

## Not started

Everything above is unchecked. Next action: Phase 0 (`sha256_hex`
builtin), then Phase 1 (Module 1 + Module 2), building
`trade_finance.nir` incrementally and verifying each module end-to-end
before moving to the next, per the plan's Verification section.
