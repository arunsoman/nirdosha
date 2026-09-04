//! Typed mirror of the PRD-extraction JSON shape produced by
//! `scratch/prompt_v2.txt` (see `scratch/extracted_typed_v1.json` for a
//! real, worked example) — `user_stories`/`workflows`/`nfrs`, each with
//! its own `pre_logic`/`post_logic`/`acceptance_criteria` where
//! applicable. Deliberately just data: this module has no logic of its
//! own, only the `serde::Deserialize` shape a real extraction file
//! parses into, so `workflow_conformance.rs`/`contract_check.rs` (and
//! whatever consumes `nfrs` later) all read the same one schema instead
//! of each hand-rolling a partial view of it.
//!
//! Unknown/extra JSON keys are ignored, not rejected (`provenance`,
//! `note`, `compiled_and_verified`, ... aren't modeled here at all) —
//! this schema only needs to carry what the verification constructs
//! actually consume, not round-trip every field an extraction happens
//! to include.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractionFile {
    #[serde(default)]
    pub user_stories: Vec<ExtractedUserStory>,
    #[serde(default)]
    pub workflows: Vec<ExtractedWorkflow>,
    #[serde(default)]
    pub nfrs: Vec<ExtractedNfr>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedUserStory {
    pub id: String,
    #[serde(default)]
    pub pre_logic: Vec<String>,
    #[serde(default)]
    pub post_logic: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    /// Which real `.nir` function(s) this story's `pre_logic`/
    /// `post_logic` are actually about — **not part of today's real
    /// extraction schema** (confirmed absent from
    /// `scratch/extracted_typed_v1.json`'s `user_stories[]`; only a
    /// workflow's `routing_fn.name` binds to a real function today).
    /// `#[serde(default)]` so this struct still deserializes the real
    /// file as-is; always empty until the extraction prompt is extended
    /// to emit it. See `contract_check`'s module doc / `docs/ROADMAP.md`'s
    /// contract-checking entry for why this is the actual blocker on
    /// checking a user story's own `pre_logic`/`post_logic` today.
    #[serde(default)]
    pub implements: Vec<String>,
    /// A literal role token (`"treasury_user"`, never a prose label like
    /// `required_permission`'s own value) meant to paste directly into a
    /// real `requires(role: "...")` — 2026-08-26, `docs/WORKFLOW.md`'s
    /// state-ownership proposal. `#[serde(default)]`, same reasoning as
    /// `implements`: not part of `scratch/extracted_typed_v1.json` yet.
    #[serde(default)]
    pub required_role: Option<String>,
    /// One entry per concrete value the persona actually enters across
    /// `actions` — what makes a story renderable as a real form, the
    /// same `{field, type}` shape `ExtractedWorkflow::data` already uses.
    /// `#[serde(default)]`, same reasoning as `implements`.
    #[serde(default)]
    pub input_fields: Vec<ExtractedDataField>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AcceptanceCriterion {
    pub given: String,
    pub when: String,
    pub then: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedWorkflow {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub data: Vec<ExtractedDataField>,
    #[serde(default)]
    pub states: Vec<ExtractedState>,
    #[serde(default)]
    pub transitions: Vec<ExtractedTransition>,
    pub routing_fn: Option<ExtractedRoutingFn>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedDataField {
    pub field: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedState {
    pub name: String,
    pub terminal: bool,
    #[serde(default)]
    pub on_entry: Vec<String>,
    #[serde(default)]
    pub on_exit: Vec<String>,
    /// Human-readable display name (a UI status badge/screen title) —
    /// `docs/WORKFLOW.md`'s state-ownership proposal, `#[serde(default)]`
    /// since `scratch/extracted_typed_v1.json` predates this field.
    #[serde(default)]
    pub label: Option<String>,
    /// Who may fire one of this state's outgoing events — **not** who
    /// `on_entry` notifies (a different question, see `docs/WORKFLOW.md`).
    /// `None` means the PRD doesn't restrict who may act (not "unsure").
    #[serde(default)]
    pub owner_role: Option<String>,
    /// A claim-based alternative to `owner_role`, for the rare state
    /// gated by claim rather than role.
    #[serde(default)]
    pub owner_claim: Option<ExtractedClaim>,
    /// How many *distinct* `owner_role`/`owner_claim` holders must
    /// independently decide before this state's transition fires — `1`
    /// is Maker-Checker-shaped (one other decider), `2`+ is a real
    /// quorum (six-eyes-shaped). Nirdosha's compiler doesn't enforce
    /// this yet (`docs/WORKFLOW.md`'s proposal names the gap directly) — this
    /// field only records the PRD's intent for when it does.
    #[serde(default)]
    pub required_decisions: Option<i64>,
}

/// `requires(claim: "<name>", "<value>")`'s extracted shape — see
/// `ExtractedState::owner_claim`.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedClaim {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedTransition {
    pub from: String,
    pub event: String,
    pub to: String,
    pub link: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedRoutingFn {
    pub name: String,
    #[serde(default)]
    pub pre_logic: Vec<String>,
    #[serde(default)]
    pub post_logic: Vec<String>,
}

/// Deliberately untyped beyond what identifies it — an NFR (`"platform
/// uptime: 99.95%"`, `"sub-60-second reconciliation latency"`) is an
/// *operational* claim about a running deployment, not a property of
/// source code a compiler pass could prove or disprove. `contract_check`/
/// `workflow_conformance` don't (and structurally couldn't) validate
/// these — see `docs/ROADMAP.md`'s contract-checking entry for why this is a
/// stated non-goal, not an oversight.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedNfr {
    pub id: String,
    pub category: String,
    pub statement: String,
    pub metric: String,
    pub target: String,
}
