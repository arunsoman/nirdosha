//! Structural, exact conformance checking between an extracted
//! `workflow` (`extraction_schema::ExtractedWorkflow` —
//! `scratch/extracted_typed_v1.json`'s `workflows[]` shape) and a real
//! `.nir` program's own `workflow { ... }` declaration
//! (`ast::WorkflowDecl`).
//!
//! Unlike `contract_check.rs` (which needs Z3 because a pre/post
//! condition is a claim about *every* input a function could receive),
//! this needs no solver at all: a workflow's states/transitions/data
//! fields are a finite, already-fully-known set the moment the `.nir`
//! source parses — checking "does the real workflow declare exactly
//! these states, exactly these transitions, exactly these data fields"
//! is ordinary set/relation equality over two finite structures. That
//! makes it the more *complete* check of the two — a real counterexample
//! or a real proof, always, no `Unsupported` case — but also the
//! narrower one: it verifies shape, not behavior. `on_entry`/`on_exit`
//! action lists are compared by *count* only, not by matching prose like
//! `"notify the checker role..."` against a real `notify(...)` call's
//! actual arguments — that would need a natural-language-to-call binding
//! this module deliberately doesn't attempt (see `ROADMAP.md`'s entry
//! for why that's named as a real, separate gap rather than silently
//! approximated here).

use std::collections::HashSet;

use crate::ast::{Program, WorkflowDecl};
use crate::extraction_schema::ExtractedWorkflow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    WorkflowNotFound { name: String },
    MissingDataField { field: String, ty: String },
    ExtraDataField { field: String, ty: String },
    DataFieldTypeMismatch { field: String, extracted_ty: String, actual_ty: String },
    MissingState { name: String },
    ExtraState { name: String },
    TerminalFlagMismatch { name: String, extracted_terminal: bool, actual_terminal: bool },
    MissingTransition { from: String, event: String, to: String, link: bool },
    ExtraTransition { from: String, event: String, to: String, link: bool },
    OnEntryCountMismatch { state: String, extracted: usize, actual: usize },
    OnExitCountMismatch { state: String, extracted: usize, actual: usize },
    RoutingFnNotFound { name: String },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::WorkflowNotFound { name } => write!(f, "no `workflow {name}` declared in the program"),
            Mismatch::MissingDataField { field, ty } => write!(f, "extracted data field `{field}: {ty}` has no match in the real workflow's `data`"),
            Mismatch::ExtraDataField { field, ty } => write!(f, "real workflow has data field `{field}: {ty}` the extraction doesn't mention"),
            Mismatch::DataFieldTypeMismatch { field, extracted_ty, actual_ty } => {
                write!(f, "data field `{field}`: extracted type `{extracted_ty}` != real type `{actual_ty}`")
            }
            Mismatch::MissingState { name } => write!(f, "extracted state `{name}` has no match in the real workflow"),
            Mismatch::ExtraState { name } => write!(f, "real workflow has state `{name}` the extraction doesn't mention"),
            Mismatch::TerminalFlagMismatch { name, extracted_terminal, actual_terminal } => write!(
                f,
                "state `{name}`: extraction says terminal={extracted_terminal}, real workflow says terminal={actual_terminal}"
            ),
            Mismatch::MissingTransition { from, event, to, link } => {
                write!(f, "extracted transition `{from} --{event}(link={link})--> {to}` has no match in the real workflow")
            }
            Mismatch::ExtraTransition { from, event, to, link } => {
                write!(f, "real workflow has transition `{from} --{event}(link={link})--> {to}` the extraction doesn't mention")
            }
            Mismatch::OnEntryCountMismatch { state, extracted, actual } => {
                write!(f, "state `{state}`: extraction lists {extracted} `on_entry` action(s), real workflow has {actual}")
            }
            Mismatch::OnExitCountMismatch { state, extracted, actual } => {
                write!(f, "state `{state}`: extraction lists {extracted} `on_exit` action(s), real workflow has {actual}")
            }
            Mismatch::RoutingFnNotFound { name } => write!(f, "extracted `routing_fn.name` `{name}` — no such function is declared in the program"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConformanceReport {
    pub mismatches: Vec<Mismatch>,
}

impl ConformanceReport {
    pub fn is_exact_match(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Checks `extracted` against whichever real `workflow` in `program`
/// shares its `name` — an empty `ConformanceReport` means every state,
/// every transition, every data field, and (existence-only) the
/// `routing_fn` name all matched exactly; anything else lists precisely
/// what didn't, so a mismatch is a concrete, actionable diff, not a bare
/// "doesn't match."
pub fn check_workflow_conformance(program: &Program, extracted: &ExtractedWorkflow) -> ConformanceReport {
    let mut mismatches = Vec::new();
    let Some(real) = program.workflows.iter().find(|w| w.name == extracted.name) else {
        mismatches.push(Mismatch::WorkflowNotFound { name: extracted.name.clone() });
        return ConformanceReport { mismatches };
    };

    check_data_fields(extracted, real, &mut mismatches);
    check_states(extracted, real, &mut mismatches);
    check_transitions(extracted, real, &mut mismatches);

    if let Some(routing_fn) = &extracted.routing_fn {
        if !program.fns.iter().any(|f| f.name == routing_fn.name) {
            mismatches.push(Mismatch::RoutingFnNotFound { name: routing_fn.name.clone() });
        }
    }

    ConformanceReport { mismatches }
}

fn check_data_fields(extracted: &ExtractedWorkflow, real: &WorkflowDecl, out: &mut Vec<Mismatch>) {
    let real_fields: std::collections::HashMap<&str, String> = real.data.iter().map(|f| (f.name.as_str(), f.ty.name())).collect();
    let extracted_names: HashSet<&str> = extracted.data.iter().map(|f| f.field.as_str()).collect();

    for f in &extracted.data {
        match real_fields.get(f.field.as_str()) {
            None => out.push(Mismatch::MissingDataField { field: f.field.clone(), ty: f.ty.clone() }),
            Some(actual_ty) if *actual_ty != f.ty => {
                out.push(Mismatch::DataFieldTypeMismatch { field: f.field.clone(), extracted_ty: f.ty.clone(), actual_ty: actual_ty.clone() })
            }
            Some(_) => {}
        }
    }
    for (name, ty) in &real_fields {
        if !extracted_names.contains(name) {
            out.push(Mismatch::ExtraDataField { field: name.to_string(), ty: ty.clone() });
        }
    }
}

fn check_states(extracted: &ExtractedWorkflow, real: &WorkflowDecl, out: &mut Vec<Mismatch>) {
    let real_states: std::collections::HashMap<&str, &crate::ast::StateDecl> = real.states.iter().map(|s| (s.name.as_str(), s)).collect();
    let extracted_names: HashSet<&str> = extracted.states.iter().map(|s| s.name.as_str()).collect();

    for s in &extracted.states {
        match real_states.get(s.name.as_str()) {
            None => out.push(Mismatch::MissingState { name: s.name.clone() }),
            Some(real_state) => {
                if real_state.terminal != s.terminal {
                    out.push(Mismatch::TerminalFlagMismatch { name: s.name.clone(), extracted_terminal: s.terminal, actual_terminal: real_state.terminal });
                }
                if s.on_entry.len() != real_state.on_entry.len() {
                    out.push(Mismatch::OnEntryCountMismatch { state: s.name.clone(), extracted: s.on_entry.len(), actual: real_state.on_entry.len() });
                }
                if s.on_exit.len() != real_state.on_exit.len() {
                    out.push(Mismatch::OnExitCountMismatch { state: s.name.clone(), extracted: s.on_exit.len(), actual: real_state.on_exit.len() });
                }
            }
        }
    }
    for name in real_states.keys() {
        if !extracted_names.contains(name) {
            out.push(Mismatch::ExtraState { name: name.to_string() });
        }
    }
}

fn check_transitions(extracted: &ExtractedWorkflow, real: &WorkflowDecl, out: &mut Vec<Mismatch>) {
    let mut real_set: HashSet<(String, String, String, bool)> = HashSet::new();
    for s in &real.states {
        for t in &s.transitions {
            real_set.insert((s.name.clone(), t.event.clone(), t.target.clone(), t.via_link));
        }
    }
    let mut extracted_set: HashSet<(String, String, String, bool)> = HashSet::new();
    for t in &extracted.transitions {
        extracted_set.insert((t.from.clone(), t.event.clone(), t.to.clone(), t.link));
    }

    for (from, event, to, link) in &extracted_set {
        if !real_set.contains(&(from.clone(), event.clone(), to.clone(), *link)) {
            out.push(Mismatch::MissingTransition { from: from.clone(), event: event.clone(), to: to.clone(), link: *link });
        }
    }
    for (from, event, to, link) in &real_set {
        if !extracted_set.contains(&(from.clone(), event.clone(), to.clone(), *link)) {
            out.push(Mismatch::ExtraTransition { from: from.clone(), event: event.clone(), to: to.clone(), link: *link });
        }
    }
}
