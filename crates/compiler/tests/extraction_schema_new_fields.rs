//! Confirms the new extraction-schema fields (`docs/WORKFLOW.md`'s
//! state-ownership proposal / `docs/ROADMAP.md` A12a) round-trip correctly —
//! both when present and, for backward compatibility with extraction
//! files predating them (`scratch/extracted_typed_v1.json`), when
//! entirely absent.

use nirdosha::extraction_schema::ExtractionFile;

#[test]
fn new_user_story_fields_deserialize_when_present() {
    let json = r#"{
        "user_stories": [{
            "id": "US-TEST-001",
            "implements": ["submit_widget"],
            "required_role": "widget_admin",
            "input_fields": [{"field": "widget_name", "type": "str"}]
        }],
        "workflows": [],
        "nfrs": []
    }"#;
    let file: ExtractionFile = serde_json::from_str(json).expect("should deserialize");
    let story = &file.user_stories[0];
    assert_eq!(story.implements, vec!["submit_widget".to_string()]);
    assert_eq!(story.required_role.as_deref(), Some("widget_admin"));
    assert_eq!(story.input_fields[0].field, "widget_name");
    assert_eq!(story.input_fields[0].ty, "str");
}

#[test]
fn new_state_ownership_fields_deserialize_when_present() {
    let json = r#"{
        "user_stories": [],
        "workflows": [{
            "id": "WF-TEST-001",
            "name": "TestFlow",
            "data": [],
            "states": [{
                "name": "PendingReview",
                "terminal": false,
                "label": "Pending Review",
                "owner_role": "six_eyes_reviewer",
                "owner_claim": null,
                "required_decisions": 2
            }],
            "transitions": [],
            "routing_fn": null
        }],
        "nfrs": []
    }"#;
    let file: ExtractionFile = serde_json::from_str(json).expect("should deserialize");
    let state = &file.workflows[0].states[0];
    assert_eq!(state.label.as_deref(), Some("Pending Review"));
    assert_eq!(state.owner_role.as_deref(), Some("six_eyes_reviewer"));
    assert!(state.owner_claim.is_none());
    assert_eq!(state.required_decisions, Some(2));
}

#[test]
fn owner_claim_deserializes_when_present() {
    let json = r#"{
        "user_stories": [],
        "workflows": [{
            "id": "WF-TEST-002",
            "name": "TestFlow2",
            "data": [],
            "states": [{
                "name": "PendingReview",
                "terminal": false,
                "owner_claim": {"name": "department", "value": "cardiology"}
            }],
            "transitions": [],
            "routing_fn": null
        }],
        "nfrs": []
    }"#;
    let file: ExtractionFile = serde_json::from_str(json).expect("should deserialize");
    let claim = file.workflows[0].states[0].owner_claim.as_ref().expect("owner_claim present");
    assert_eq!(claim.name, "department");
    assert_eq!(claim.value, "cardiology");
}

/// Backward compatibility: an extraction file that predates all of these
/// fields (every field entirely absent from the JSON, not just `null`)
/// must still deserialize, with sensible defaults — the same shape
/// `scratch/extracted_typed_v1.json` itself is.
#[test]
fn old_extraction_shape_with_none_of_the_new_fields_still_deserializes() {
    let json = r#"{
        "user_stories": [{"id": "US-OLD-001"}],
        "workflows": [{
            "id": "WF-OLD-001",
            "name": "OldFlow",
            "data": [],
            "states": [{"name": "Start", "terminal": true}],
            "transitions": [],
            "routing_fn": null
        }],
        "nfrs": []
    }"#;
    let file: ExtractionFile = serde_json::from_str(json).expect("old-shape extraction files must still deserialize");
    let story = &file.user_stories[0];
    assert!(story.required_role.is_none());
    assert!(story.input_fields.is_empty());
    let state = &file.workflows[0].states[0];
    assert!(state.label.is_none());
    assert!(state.owner_role.is_none());
    assert!(state.owner_claim.is_none());
    assert!(state.required_decisions.is_none());
}
