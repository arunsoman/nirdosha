# CTMS Screen Inventory

Step 1 of the CTMS-as-forcing-function initiative. This is **only** a screen
inventory — no gap analysis, no Nirdosha construct design, no `.nir` code.
Grounded in `/tmp/ctms_doc.txt`'s own module names, component names, actor
names and event names. Organized by the doc's own ~10-module breakdown, plus
a cross-cutting section for screens that don't belong to one module.

Context used to calibrate this inventory:
- The previous attempt (`git show c6d6e3e:examples/ctms/ctms.nir`) declares
  exactly 8 structs (`Transaction`, `Alert`, `Case`, `Wallet`, `CardTxn`,
  `CompliancePolicy`, `AuditEntry`, `Text`) with plain `list_/create_/
  update_/delete_<struct>` functions and **zero** `screen`/`dashboard`/
  `module` blocks — i.e. exactly one generic CRUD list+detail pair per
  struct, relying entirely on naming-convention inference. It has no
  Investigation Workspace, no case workflow stages, no graph views, no geo
  heatmaps, no report scheduling, no role-specific home dashboards, no
  policy/rule-engine config screens, and no SLA/live-status widgets — none
  of which the CTMS document treats as optional.
- `examples/vendor_ops.nir`, `examples/purchase_approval.nir`,
  `examples/kyc_onboarding.nir`, `examples/store.nir` show today's
  `screen`/`dashboard`/`module` vocabulary in use (field relabeling, custom
  actions with confirm dialogs, RBAC-gated fields, nav grouping via
  `module`, `stat`/`chart` dashboard tiles).
- `docs/LANGUAGE.md` §11/§12: today a "screen" is one `struct`'s list+detail pair
  plus optional custom row actions; a "dashboard" is a flat set of `stat`/
  `chart` tiles (exactly one chart type: an inline-SVG bar chart — no line/
  scatter/heatmap/treemap/geo/graph); `module` only groups nav items, it
  does not compose multiple structs onto one screen.

---

## Module 1: Financial Data Ingestion and Standardization

Actor: **Ingestion Admin**

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Ingestion Admin Home | Ingestion Admin | Landing dashboard for ingestion health | Ingestion volume by source (chart), validation error rate trend, schema-drift alert count, active-source count | Drill into a source, jump to error queue | Dashboard-with-drilldown |
| Source Connector Registry | Ingestion Admin | List all configured sources (Banks, Insurance, Microfinance, Payment Gateways, Crypto Exchanges, Regulatory APIs) | Source name, type, format (JSON/CSV/XML/Avro/fixed-width), connection status, last-ingestion timestamp | Enable/disable, edit config, trigger manual re-ingest | List/detail CRUD |
| Source Onboarding Wizard | Ingestion Admin | Self-service onboarding of a new data source ("Future Extensibility": configuration UIs for self-service source onboarding) | Connection-type picker (SFTP/Kafka/REST), format picker, schema mapping fields, credentials | Test connection, save & activate, run initial profiling | Config-as-data (multi-step wizard) |
| Data Profiling / Quality Scorecard | Ingestion Admin | Per-source data quality scorecard from the Data Profiling Engine | Field-type distribution, null rates, range violations, quality score, drift comparison over time | Re-run profiling, flag a field for review | Dashboard-with-drilldown |
| Validation & Rules Engine Config | Ingestion Admin | Author structural/semantic/temporal validation rules (the ingestion DSL) | Rule list, rule DSL/condition editor, rejection-reason mapping, active/inactive toggle | Add/edit/delete rule, test rule against a sample record, activate/deactivate | Config-as-data |
| Ingestion Error Queue | Ingestion Admin | Triage `ValidationError` events | Error type, source, offending record snippet, rejection reason, timestamp | Reprocess, mark reviewed, export | List/queue |
| Schema Registry & Drift screen | Ingestion Admin | Track schema versions per source and `SchemaDriftDetected` alerts | Schema version history, field-diff view, backward-compatibility check result | Approve new schema version, rollback, acknowledge drift | Config-as-data |
| Ingestion Lineage & Audit Store Viewer | Ingestion Admin, Compliance Officer | Trace lineage (source, timestamp, transformation applied) and load metrics for any ingested record | Lineage trail per record, load-metrics chart (volume/latency/errors over time) | Search by transaction ID, export lineage record | Search/export composite |

## Module 2: Real-Time Fraud Detection and AI Modelling

Actor: **Fraud Analyst** (consumed downstream by Investigator, Compliance Officer)

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Fraud Analyst Home | Fraud Analyst | Landing dashboard for the detection engine | Alert volume by severity, model performance summary, `HighRiskScoreGenerated` feed, blacklist-match count | Drill into an alert, jump to rule tuning | Dashboard-with-drilldown |
| Suspicious-Transaction Alert Queue | Fraud Analyst | Triage `SuspiciousTransactionAlert`/`HighRiskScoreGenerated` events | Transaction ID, risk score, triggering rule/model, severity, timestamp, entity | Assign to investigator, dismiss, escalate to case | List/queue |
| Alert Detail / Risk-Score Breakdown | Fraud Analyst, Investigator | Explain why one alert fired | Risk score broken down by contributing signal (rule violations, anomaly indexes, ML output), transaction payload, related-entity summary, blacklist-match detail | Escalate to case, mark false positive, request investigator feedback | Composite multi-pane workspace |
| Rule Engine Configuration | Fraud Analyst | Configure the Rule-Based Engine (blacklist, volume/frequency limits, country restrictions, time-of-day flags) | Rule list, condition editor, active/version state, jurisdiction scope | Create/edit rule, activate/deactivate, version rollback, test against sample transactions | Config-as-data |
| Scoring Weights Configuration | Fraud Analyst, Compliance Officer | Configure Risk Scoring Engine weights aggregating rule/anomaly/ML signals | Per-signal weight table/sliders, composite-score formula preview, historical score distribution | Adjust weights, save new scoring profile, simulate against historical data | Config-as-data |
| Behavioural Profile screen | Fraud Analyst, Investigator | Per customer/account/device behavioural baseline and deviations | Normal-behaviour baseline chart, deviation timeline, per-channel breakdown | Flag entity, add to watchlist | Dashboard-with-drilldown |
| ML Model Management | Fraud Analyst | Manage supervised/unsupervised models, retraining, drift | Model list, accuracy/precision-recall metrics, drift report, training-data lineage, explainability/feature-importance panel | Trigger retraining, promote model version, rollback | Composite (ML-ops dashboard) |
| Analyst Feedback / Labeling Queue | Fraud Analyst | Capture investigator feedback (false positive/confirmed fraud) for the retraining loop | Resolved-alert queue, current label, model that flagged it | Confirm/correct label, submit to training set | List/queue |
| Blacklist/Whitelist Management | Fraud Analyst, Compliance Officer | Maintain static reference lists used by the rule engine | Entry list (entity, reason, source, added date) | Add/remove entry, import from OFAC/UN/EU feed, bulk upload | List/detail CRUD |

## Module 3: Investigative Case Management System

Actors: **Investigator, Supervisor, Regulatory Officer**, plus Compliance Officer / Admin

This is the doc's own richest module (it gets a dedicated "Case Management
System" section with role-specific interfaces and a 4-stage workflow model).

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Investigator Home | Investigator | Landing dashboard for an investigator | My open cases, **SLA countdown per case**, cases nearing SLA breach, alerts newly assigned to me | Open case, claim a case from the queue | Dashboard-with-drilldown |
| Supervisor Home | Supervisor | Landing dashboard for a supervisor ("SLA tracking, reassignment, escalation approvals") | Team workload (cases per investigator), SLA-breach count, pending escalation approvals, case-aging distribution | Reassign case, approve/reject escalation | Dashboard-with-drilldown |
| Regulatory Officer Home | Regulatory Officer | Landing dashboard for final-outcome/compliance work ("final outcome approval, compliance tagging, audit preparation") | Cases awaiting outcome approval, compliance-tagging queue, audit-prep checklist | Approve outcome, tag for filing | Dashboard-with-drilldown |
| Case Queue | Investigator, Supervisor | All cases, filterable and triageable | Case ID, origin signal, severity tag, status (Open/UnderInvestigation/Escalated/LegalHold/Resolved/Closed), **live SLA deadline**, assignee | Assign, bulk reassign, filter/search, export | List/queue (live SLA status, not plain CRUD) |
| Investigation Workspace | Investigator | The doc's own named composite: work one case end to end | Transaction timeline, geo/IP trail, KYC metadata panel, related alerts & cases panel, notes, tagged entities, linked artifacts, risk score | Add note, tag entity, link artifact, merge case, escalate, dismiss | Composite multi-pane workspace |
| Case Workflow / Stage Tracker | Investigator, Supervisor, Regulatory Officer | Render the 4-stage model: Investigation & Enrichment → Compliance Escalation/Legal Hold → Resolution → Regulatory Filing | Stage progress indicator, stage-specific required fields, gate checklist | Advance stage, apply legal hold, submit for compliance escalation | Composite / workflow-state screen |
| Case Collaboration & Comment Thread | Investigator, Supervisor, Compliance Officer | Real-time commenting, tagging, team mentions | Comment thread, @mentions, action-log feed | Post comment, tag teammate, view action log | Composite (embeds live feed) |
| Evidence Management | Investigator | Store/search case evidence with versioning and signatures | Evidence list (type, uploader, hash/signature, version), full-text search bar, tag list | Upload, version, sign, search, tag, download | Composite (list + viewer + search) |
| Decision Panel | Investigator, Supervisor | Record case resolution and trigger closure | Resolution options (Confirmed Fraud / False Positive / Suspicious–Re-monitor), justification field, timestamp | Submit resolution, trigger closure workflow, send feedback to detection engine | Composite (decision + workflow trigger) |
| Case Linking / Entity Graph | Investigator, Fraud Analyst | "Link related entities across cases (graph views)"; case graphing by common entity/pattern | Neo4j-style graph of entities/cases/transactions, node/edge risk coloring, cluster highlighting | Expand node, merge cases from the graph, export subgraph | Graph/network view |
| Escalation & Regulatory Referral | Supervisor, Regulatory Officer | Escalation to compliance/legal, external referral to law enforcement or other FIs, secured push to INSA/FIS | Escalation reason, target (compliance/legal/regulator/external FI), referral status, regulator response log | Escalate, refer externally, log regulator response | Composite / workflow |
| Case Export / Audit-Ready Dossier | Investigator, Regulatory Officer | Produce the downloadable, tamper-proof case dossier | Export format selector (PDF/CSV/JSON), artifact checklist, hash/signature display | Generate dossier, download, verify signature | Composite / report generation |
| Case Analytics Dashboard | Supervisor, Compliance Officer | "Dashboard: Open cases, SLA breaches, investigation duration trends" | Open-case count, SLA-breach trend, average investigation duration, case-aging histogram, resolution-outcome distribution | Drill into any segment | Dashboard-with-drilldown |
| SLA / Workload Routing Config | Supervisor | Configure severity-based/skillset-based/jurisdiction-specific routing and SLA tiers (e.g. P1 within 30 min) | Routing-rule list, SLA-tier definitions, team skillset mapping | Edit routing rule, edit SLA tier, bulk reassign | Config-as-data |

## Module 4: Regulatory and Secure Data Exchange

Actors: **Compliance Officer, Regulator**

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Data Exchange Dashboard | Compliance Officer | "Real-time views into shared volumes, access logs, policy violations" | Shared-volume trend, access-log feed, policy-violation count | Drill into a violation, revoke access | Dashboard-with-drilldown |
| Secure Data Sharing Log | Compliance Officer, Regulator | Audit trail of `DataAccessed`/`DataSharedWithRegulator` events | Who/what/when/destination, signature/checksum status | View detail, export | List/detail CRUD |
| RBAC/ABAC Policy Editor | Compliance Officer, Admin | Author attribute-based access policies (time/device/jurisdiction/risk conditions) — explicitly policy-as-data, not a plain record | Policy-rule list (attribute conditions), role-to-permission matrix | Add/edit/delete policy, simulate against a sample identity, activate | Config-as-data |
| Consent & Data Handling Policy | Compliance Officer | Manage consent capture, retention periods, erasure/minimization policies | Per-subject consent log, retention-policy list, erasure-request queue | Record/revoke consent, configure retention rule, process erasure request | Config-as-data + queue |
| Regulatory Reporting Queue | Compliance Officer | Prepare/transmit SAR/STR/CTR reports per jurisdiction template | Report queue (type, jurisdiction, status, due date), template selector, transmission status | Generate now, schedule, retry transmission, view acknowledgment | Report generation/scheduling |
| Access Violation / Breach Monitor | Compliance Officer | Track `AccessViolationDetected`/`ConsentRevoked` events | Violation feed, severity, affected subject, response status | Acknowledge, escalate, close | List/queue |
| Regulator/Partner Portal | Regulator | External-facing scoped view: "receives reports, audit exports, data snapshots via API/SFTP" | Available reports/exports, download log, submission status | Download report, acknowledge receipt | List/detail (external-facing, scoped) |
| Exchange Audit Export | Compliance Officer | Export access/sharing logs for inspection; compliance trend reporting | Export filters, format selector (JSON/PDF/CSV), compliance trend chart | Generate export | Dashboard + report generation |

## Module 5: Advanced Analytics, Reporting, and BI

Actors: **Investigator, Compliance Officer, Executive/BI stakeholder**

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Executive/BI Home | Executive stakeholder | Role-sensitive dashboard for decision-makers | Alert volumes by severity, entity risk rankings, case-aging & SLA trends, geo heatmap of transaction spikes | Drill into any widget | Dashboard-with-drilldown |
| Investigator Analytics View | Investigator | Role variant of the same dashboard layer, investigator-focused | My alert-to-case conversion rate, my SLA performance | Drill into own metrics | Dashboard-with-drilldown |
| Windowed Metrics Explorer | Compliance Officer, Fraud Analyst | Real-time stream aggregations (txn count/account/min, volume/channel, high-frequency source, alert rate/segment) | Time-windowed metric charts, sliding/tumbling window selector | Change window, drill into segment | Dashboard-with-drilldown |
| Self-Service Query Interface | Investigator, Compliance Officer | Ad-hoc query/visualization builder with reusable templates | Query builder UI, saved query templates, result table/chart | Build query, save template, run, export (CSV/JSON/PDF) | Tool (ad-hoc query builder) |
| Analytics Report Generation & Scheduling | Compliance Officer | Template-based SAR/CTR/regulatory audit-response report generation, scheduled dispatch | Template picker, schedule config (daily/monthly/incident-driven), dispatch channel (email/SFTP/API), report history | Generate on-demand, schedule, edit template mapping, view dispatch log | Report generation/scheduling |
| Forecasting & Predictive Analytics | Compliance Officer, Fraud Analyst | Time-series anomaly forecasting, behavioural drift detection | Forecast chart (ARIMA/Prophet output), drift alerts per entity, explainability panel | Drill into entity, acknowledge drift | Dashboard-with-drilldown |
| Geo Heatmap | Investigator, Compliance Officer, Executive | Visualize "geospatial heatmaps ... and graph networks" of high-risk regions | Interactive heatmap, region drill-down, FATF grey/black-list overlay | Drill into region, escalate cluster | Geo/heatmap view |
| Graph Network Explorer | Investigator, Fraud Analyst | Analytics-level entity/transaction network exploration | Graph canvas, node types (account/entity/wallet), edge weight = transaction value, clustering highlight | Expand, filter by risk score, export subgraph to a case | Graph/network view |
| Analytics Access & Audit Log | Compliance Officer | Log dashboard access and report downloads; lineage of analytics assets | Access-log table, download log, asset lineage | Export, filter | List/detail CRUD |

## Module 6: Compliance and Regulatory Management

Actor: **Compliance Officer**

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Compliance Officer Home / Compliance Dashboard | Compliance Officer | "Compliance scores, pending obligations, filing health" landing view | Compliance score, pending-obligation count, SLA violations, overdue reports, alert history per rule/jurisdiction | Drill into obligation | Dashboard-with-drilldown |
| Policy Management Engine | Compliance Officer, Admin | Configure regulatory thresholds, risk-scoring matrices, watchlist integration — static **and** dynamic policies, versioned with activation timelines | Policy list, threshold editor, version history, activation-timeline scheduler | Create/edit policy version, schedule activation, rollback | Config-as-data |
| Compliance Flag Queue | Compliance Officer | Triage `ComplianceFlagRaised`/`ReportDueSoon`/`BreachThresholdCrossed` events | Flag list (rule matched, entity, threshold), severity | Review, dismiss, escalate to case | List/queue |
| Report Generator & Scheduler | Compliance Officer | SAR/CTR/RBA templates, auto-scheduled by rule match (daily/monthly/incident-driven) | Report-template list, schedule rules, internal-case/alert-to-report-field mapping | Create/edit template mapping, schedule, generate, preview | Config-as-data + report scheduling |
| Legal Hold Management | Compliance Officer | Freeze data for investigation, track expiry, per-field/per-subject erasure | Legal-hold list (case/entity, status, applied-by, expiry), erasure-request queue | Apply legal hold, release hold, process erasure request | Workflow/approval list |
| Regulatory Filing Calendar | Compliance Officer | Track upcoming/overdue filings and `ReportMissed` events per jurisdiction | Calendar/list of filing due dates by jurisdiction | Mark filed, request extension | List/calendar view |

## Module 7: Crypto Risk Monitoring (Phase 2)

Actor group: Fraud Analyst, Compliance Officer, plus **Exchange/Partner FI**

Note: the doc describes two overlapping sub-systems here — on-chain wallet
monitoring, and card-based crypto-purchase detection (MCC filtering) — both
folded into this one module.

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Crypto Risk Home | Fraud Analyst | Landing dashboard for crypto risk | High-risk wallet count, sanction-breach alerts, Travel Rule violations, crypto-flagged card-txn trend | Drill into any metric | Dashboard-with-drilldown |
| Wallet Watchlist | Fraud Analyst | Track monitored wallet addresses | Wallet address, cluster ID, risk score, sanctioned flag, last activity | Add to watchlist, flag sanctioned, drill into cluster | List/detail CRUD |
| Wallet Cluster Graph | Fraud Analyst | "Neo4j-powered visualization" of wallet clusters, mixer/tumbler detection, risk propagation | Wallet-cluster graph, mixer/tumbler-flagged nodes, risk-propagation coloring | Expand cluster, escalate to case | Graph/network view |
| Fiat–Crypto Correlation | Fraud Analyst | Card-based crypto-purchase detection via MCC filtering (6051/4829), merchant matching, keyword scanning | Crypto-flagged card transactions, merchant map, MCC match list, keyword-scan hits | Review, escalate, mark false positive | Composite / dashboard-with-drilldown |
| Card-Crypto Monitoring Dashboard | Fraud Analyst | "Interactive Visualization... dashboards show crypto-flagged card transactions with time trends, merchant maps, high-risk cards" | Time-trend chart, merchant heatmap, high-risk card list, transaction-level drill-down | Drill into card, drill into merchant | Dashboard-with-drilldown |
| Merchant/Keyword Dictionary | Fraud Analyst, Admin | Maintain known exchange/wallet-service merchant list and keyword dictionary used for detection | Merchant list, keyword list | Add/edit/remove entries, import feed | Config-as-data |
| Travel Rule Compliance | Compliance Officer | Real-time validation of counterparty info for blockchain transfers; `TravelRuleViolationDetected` | Transaction counterparty info, validation status, violation list | Review violation, request counterparty info | List/detail CRUD |
| Crypto Compliance Report | Compliance Officer | Scheduled generation of Travel Rule reports, SCARs, wallet movement summaries | Report template, schedule, dispatch history | Generate, schedule | Report generation/scheduling |
| Wallet Sanctions Screening Queue | Compliance Officer | Continuous screening of wallets against OFAC/EU/UN/national blacklists | Screening-hit list, list source, match confidence | Review hit, clear/confirm, escalate | List/queue |
| Crypto Legal Hold | Compliance Officer | `CryptoLegalHoldApplied` tracking | Held wallets/transactions list | Apply/release hold | List/detail CRUD |
| Crypto Transaction Simulation Sandbox | Fraud Analyst | Sandbox to test/tune detection thresholds against simulated card-purchase patterns | Simulation config, generated pattern preview, detection result | Run simulation, tune thresholds | Tool (simulation) |

## Module 8: RTFDS (Real-Time Fraud Detection Management System)

Note: the doc treats RTFDS as its own independently-operating module
(session/account/device fraud), distinct from Module 2's transaction fraud
detection, though they share personnel (Fraud Analyst).

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| RTFDS Home | Fraud Analyst | Landing dashboard for session/device fraud | Fraud-rate trend, blocked-attempts count, response SLA, risk-tiered alert breakdown (High/Medium/Low) | Drill into any metric | Dashboard-with-drilldown |
| Session/Fraud Alert Queue | Fraud Analyst | Triage `FraudDetected`/`DeviceFlagged`/`SessionAnomalyReported` events | Event type, user, device, IP, risk tier, action taken (blocked/challenged/logged) | Investigate, override action, escalate | List/queue |
| Session/Device Linkage View | Fraud Analyst | "Linkage views for recurring device/account/IP patterns" | Device/account/IP link graph | Drill into linked entity | Graph/network view |
| Real-Time Action Console | Fraud Analyst | Live block/challenge/log decisioning at the transaction/session level | Pending-action queue, risk tier, recommended action | Block, force 2FA, override, notify customer | Composite (live-action queue) |
| RTFDS Retention & Reporting | Fraud Analyst | 90-day fraud-log retention view; fraud-rate/blocked-attempt/SLA dashboards | Fraud-rate trend, blocked-attempt trend, SLA-compliance chart | Filter by date range | Dashboard-with-drilldown |

## Module 9: Identity and Access Management (IAM)

Actor: **Admin** (plus every user, self-service)

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| IAM Admin Home | Admin | Landing dashboard for identity/access posture | Active-session count, failed-login trend, MFA-adoption rate, pending role-elevation approvals | Drill into any metric | Dashboard-with-drilldown |
| User & Role Management | Admin | Manage RBAC roles (Investigator, Supervisor, Compliance Officer, Regulator, Analyst, Admin) and delegated department-level administration | User list, role assignment, department scoping | Create/edit user, assign role, deactivate, approve elevation request | List/detail CRUD (elevation approval pushes it toward workflow) |
| ABAC Policy Editor | Admin, Security Specialist | Author attribute-based policy conditions (time, region, risk level, device fingerprint) evaluated via OPA | Policy-rule list, condition builder, ruleset version | Create/edit rule, publish ruleset, simulate | Config-as-data |
| Federated Identity / SSO Config | Admin | Configure National ID (Fayda), OAuth2/OIDC federated login providers | Identity-provider list, attribute-mapping config | Add/edit provider, test connection | Config-as-data |
| MFA & Session Security Policy | Admin, Security Specialist | Configure MFA methods, device/browser fingerprinting, IP allowlisting, auto-timeout | MFA-method config, session-timeout policy, IP allowlist | Edit policy | Config-as-data |
| Access & Consent Log | Admin, Compliance Officer | Search who/what/when/why access logs and PII/regulatory consent logs | Access-log table, consent-log table | Search/filter, export for audit | List/detail CRUD (search-heavy) |
| Behavioural Access Analytics | Admin, Security Specialist | User behaviour profiling for dynamic risk scoring; deviation alerts (off-hours access, country mismatch, bulk downloads) | Deviation-alert feed, per-user risk score, adaptive-access trigger log | Drill into user, force re-auth | Dashboard-with-drilldown |
| Onboarding / Role-Elevation Approval | Admin, Supervisor | Internal approval workflow for onboarding, role elevation, revocation | Pending-request queue, requester, requested role/department, approver chain | Approve/reject | Workflow/approval queue |

## Module 10: Audit and Logging Infrastructure

Actors: **Investigator, Compliance Officer/Auditor, Admin**, Regulator (external view)

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Audit Search & Export | Investigator, Compliance Officer, Admin | "Filter logs by module, date, actor, action type"; export signed bundles | Filter panel (module/date/actor/action), result table, hash-chain integrity indicator | Search, export (signed PDF/CSV/JSON), verify integrity | Search/export composite |
| Audit Trail Detail | Investigator, Compliance Officer | "Who did what, when, why" for one decision, linked to legal hold | Chronological trail, justification text, linked legal-hold reference | View, export single trail | List/detail CRUD |
| SIEM-Style Security Alert Dashboard | Admin, Security Specialist | Wazuh/Graylog-style dashboard for suspicious/policy-violating behaviour | Security-alert feed, severity, source system | Acknowledge, escalate | Dashboard-with-drilldown (SIEM-style) |
| Integrity / Tamper-Check Screen | Admin | Periodic hash-chain/Merkle-tree integrity scans and tamper alerts | Integrity-scan history, tamper-alert list, verification status | Run manual scan, view tamper detail | Dashboard-with-drilldown |
| WORM Archive Browser | Compliance Officer, Regulator | Browse the immutable archive of STR/SAR reports, audit trails, user actions | Archive object list, retention expiry, object-lock status | View, verify lock status, request extended retention | List/detail CRUD |

## Cross-Cutting Screens (no single owning module)

| Screen | Actor(s) | Purpose | Key data/widgets | Key actions | Shape |
|---|---|---|---|---|---|
| Global Notification / Alert Center | All roles | Unified feed of SLA breaches, escalations, compliance due-dates, system alerts across every module | Unified notification feed, filter by type/module | Mark read, jump to source item | Composite/feed |
| Global Search | All roles | Cross-module search across transactions, cases, alerts, wallets, audit logs, entities | Unified search bar, faceted results by module/entity type | Jump to detail | Search composite |
| User & Session Security (self-service) | All roles | "My account" view of MFA status, active sessions, login history | My active sessions, my login history, MFA setup | Revoke session, enroll MFA | Dashboard-with-drilldown |
| System Health / Observability | Admin, DevOps | Prometheus/Grafana-style infra visibility referenced repeatedly across modules | Service health matrix, Kafka topic lag, error rate | Drill into service | Dashboard-with-drilldown |
| Entity 360 / Master Entity Profile | Investigator, Fraud Analyst, Compliance Officer | Full cross-module profile of one account/customer/wallet (implied throughout by "entity linking," behavioural profiling reused across Detection, Case Mgmt, Analytics, Crypto) | KYC summary, transaction history, alert/case history, risk-score trend, linked entities | Flag entity, open new case, add to watchlist | Composite multi-pane workspace |
| Exchange/Partner FI Portal | Exchange/Partner FI | External actor's own interaction surface: "submits wallet KYC, receives inter-bank alerts, participates in federated compliance workflows" | KYC submission form, inter-bank alert feed, federated-workflow status | Submit wallet KYC, acknowledge alert, participate in workflow | Composite (external-facing portal) |

---

## Summary

**Total screens: 89**

By module:

| Module | Screens |
|---|---|
| 1. Financial Data Ingestion & Standardization | 8 |
| 2. Real-Time Fraud Detection & AI Modelling | 9 |
| 3. Investigative Case Management System | 14 |
| 4. Regulatory and Secure Data Exchange | 8 |
| 5. Advanced Analytics, Reporting, and BI | 9 |
| 6. Compliance and Regulatory Management | 6 |
| 7. Crypto Risk Monitoring (Phase 2) | 11 |
| 8. RTFDS | 5 |
| 9. IAM | 8 |
| 10. Audit and Logging Infrastructure | 5 |
| Cross-cutting | 6 |

Rough split by screen shape (some screens straddle two categories; counted
by their dominant shape):

| Shape | Approx. count |
|---|---|
| Dashboard-with-drilldown (role homes + module dashboards) | ~23 |
| Config-as-data (policy/rule/scoring/routing editors) | ~14 |
| Composite multi-pane workspace | ~14 |
| List/detail CRUD (straightforward) | ~11 |
| List/queue (live-status flavored, not plain CRUD) | ~9 |
| Report generation/scheduling | ~4 |
| Graph/network view | ~4 |
| Search/export composite | ~4 |
| Workflow/approval queue | ~2 |
| Tool (ad-hoc query builder / simulation sandbox) | ~2 |
| Geo/heatmap view | ~1 |
| Notification/feed composite | ~1 |

Only about 1 in 8 screens is a plain "one struct, list+detail, done" shape —
the great majority need something today's `screen`/`dashboard` DSL either
can't express at all, or can only fake by pretending several linked
concerns are one struct.

**Screens likely to stress today's `screen`/`dashboard` DSL hardest** (not
a gap analysis — just the pointer for the next step):

1. **Investigation Workspace** (Module 3) — the doc's own named composite:
   transaction timeline + geo/IP trail + KYC metadata + related alerts/cases
   + notes, all live on one screen. A `screen` block today is one struct's
   list/detail pair; this needs several structs' data composed and
   cross-linked on a single view.
2. **Wallet Cluster Graph / Case Linking Entity Graph / Graph Network
   Explorer / Session-Device Linkage** (Modules 3, 5, 7, 8) — the doc
   explicitly calls for "Neo4j-powered visualization." Today's DSL has
   exactly one chart type (an inline-SVG bar chart) and no graph/network
   rendering primitive at all.
3. **Geo Heatmap** (Module 5) — explicitly requested ("geospatial
   heatmaps"), and explicitly out of scope in `docs/LANGUAGE.md`'s own
   "Deliberate non-goals" for `dashboard`'s chart types (no geo/heatmap
   chart).
4. **Policy Management Engine / Rule Engine Config / Scoring Weights /
   ABAC Policy Editor** (Modules 2, 4, 6, 9) — these are policy-as-data
   screens with versioning, activation timelines, and simulate-before-apply
   — not a plain record CRUD form, and not covered by today's fixed
   seven-kind form-control set.
5. **Case Workflow / Stage Tracker** (Module 3) — a 4-stage state-machine
   UI. `workflow` (docs/LANGUAGE.md §14) exists as a backend durable-state-machine
   construct, but nothing today renders its stage progression as a UI
   surface a Supervisor/Investigator would look at.
6. **Report Generation & Scheduling screens** (Modules 4, 5, 6, 7 — SAR/
   STR/CTR/CBTR/Crypto Compliance) — need template binding + schedule
   config (daily/monthly/incident-driven) + multi-format export + dispatch
   log, none of which map onto a struct's CRUD form.
7. **Case Queue / Alert Queue / Compliance Flag Queue** (Modules 2, 3, 6) —
   look list-shaped at a glance, but need a *live* SLA countdown per row,
   not a static field — today's list view has no live/derived-value column
   concept.
8. **Self-Service Query Interface** (Module 5) — an ad-hoc query builder
   with saved templates; not list/detail-shaped at all.
