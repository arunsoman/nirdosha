//! Integration tests for `WORKFLOW.md`'s "state ownership + a generated
//! queue UI" section end to end, over a real `nirdosha serve` HTTP
//! server — not just typecheck-level (`compiler/tests/workflow.rs`
//! already covers the interpreter directly). Same `start_server`/
//! `http_request`/`mint_token` harness `tests/role_mapping.rs` already
//! establishes. Covers, against a real 3-level sequential approval chain
//! (the same shape `examples/purchase_approval.nir` demonstrates):
//! per-instance `owner` enforcement (`NotStateOwner` for the wrong role,
//! success for the right one, at every level in sequence),
//! `list_<workflow>_pending_for_me`, `list_<workflow>_submitted_by_me`
//! ("who submitted this"), `get_<workflow>_history` ("audit trail" --
//! actor/via-link/comment), and the general `Option(VerifiedIdentity)`
//! optional-injection capability `start_<workflow>` relies on.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use nirdosha::ast::Program;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::serve::AuthConfig;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;

const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://mock-idp.local";
const AUDIENCE: &str = "purchase-approval-app";

const FIXTURE_SRC: &str = r#"
    struct Text { value: str }

    fn log_stage_entered(instance_id: i64) -> bool requires(public) { return true }

    workflow PurchaseApproval {
        data { requester_subject: str, description: str, amount_cents: i64 }

        state PendingManagerApproval {
            owner: role("manager")
            label: "Pending Manager Approval"
            on_entry { log_stage_entered(instance_id) }
            on Approved -> PendingDirectorApproval
            on Rejected -> Rejected
        }
        state PendingDirectorApproval {
            owner: role("director")
            label: "Pending Director Approval"
            on_entry { log_stage_entered(instance_id) }
            on Approved -> PendingVpApproval
            on Rejected -> Rejected
        }
        state PendingVpApproval {
            owner: role("vp")
            label: "Pending VP Approval"
            on_entry { log_stage_entered(instance_id) }
            on Approved -> Approved
            on Rejected -> Rejected
        }
        state Approved terminal { label: "Approved" }
        state Rejected terminal { label: "Rejected" }
    }

    fn submit_purchase_order(identity: VerifiedIdentity, description: Text, amount_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_purchase_approval(Some(identity), PurchaseApprovalData(identity.subject, description.value, amount_cents))
    }

    fn submit_purchase_order_anonymously(description: Text, amount_cents: i64) -> Result(i64, WorkflowActionError) requires(public) {
        return start_purchase_approval(None(), PurchaseApprovalData("(anonymous)", description.value, amount_cents))
    }

    fn main() {}
"#;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn build_program(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}"));
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn start_server() -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(FIXTURE_SRC));
    let auth = AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() };
    let transact_log = std::env::temp_dir().join(format!("nirdosha-workflow-ownership-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-workflow-ownership-workflow-{port}.db"));
    std::thread::spawn(move || {
        nirdosha::serve::run(program, "127.0.0.1", port, Some(auth), None, transact_log, workflow_log, None, None, None, None, None, None)
            .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on port {port} never came up");
}

fn http_request(port: u16, path: &str, body: &str, auth_header: Option<&str>) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(h) = auth_header {
        req.push_str(&format!("Authorization: {h}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body_str = parts.next().unwrap_or("");
    let status = head.lines().next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let json = serde_json::from_str(body_str).unwrap_or_else(|e| panic!("response body wasn't JSON: {e} -- body was {body_str:?}"));
    (status, json)
}

/// Same technique `tests/role_mapping.rs::mint_token` uses.
fn mint_token(subject: &str, roles_json: &str) -> String {
    let issued_at =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock is before the Unix epoch").as_secs() as i64;
    let src = format!(
        r#"
        struct Text {{ value: str }}
        fn main() -> Text {{
            return match mock_issue_token("{subject}", "{ISSUER}", "{AUDIENCE}", {issued_at}, 3600, "{{\"roles\":{roles_json}}}", "{jwks}") {{
                Ok(token) => Text(token),
                Err(e) => Text(e),
            }}
        }}
    "#,
        jwks = JWKS.replace('"', "\\\"")
    );
    match nirdosha::run(&src) {
        Ok(nirdosha::interpreter::Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            nirdosha::interpreter::Value::Str(s) => s.to_string(),
            other => panic!("expected Text(Str(_)), got Text({other:?})"),
        },
        other => panic!("expected a minted token, got {other:?}"),
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

#[test]
fn wrong_role_is_rejected_and_right_role_advances_through_every_level() {
    let port = start_server();
    let manager = mint_token("alice", r#"[\"manager\"]"#);
    let director = mint_token("bob", r#"[\"director\"]"#);
    let vp = mint_token("carol", r#"[\"vp\"]"#);

    let (status, submit) = http_request(
        port,
        "/api/submit_purchase_order",
        r#"{"description":{"value":"laptops"},"amount_cents":500000}"#,
        Some(&bearer(&manager)),
    );
    assert_eq!(status, 200, "{submit:?}");
    let instance_id = submit["ok"].as_i64().expect("submit should return an instance id");

    // The VP tries to jump the queue while it's still sitting on the
    // manager -- must be a clean NotStateOwner, not silently accepted.
    let (status, denied) = http_request(
        port,
        "/api/advance_purchase_approval",
        &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":null}}"#),
        Some(&bearer(&vp)),
    );
    assert_eq!(status, 200, "{denied:?}"); // a clean Err, not an HTTP-level rejection
    assert_eq!(denied["err"]["variant"], "NotStateOwner", "{denied:?}");

    // The manager approves -- moves to PendingDirectorApproval.
    let (status, approved) = http_request(
        port,
        "/api/advance_purchase_approval",
        &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":{{"comment":"looks fine"}}}}"#),
        Some(&bearer(&manager)),
    );
    assert_eq!(status, 200, "{approved:?}");
    assert_eq!(approved["ok"], true, "{approved:?}");

    // The manager, now demoted to a bystander for this instance, can no
    // longer act on it either.
    let (_, denied_again) = http_request(
        port,
        "/api/advance_purchase_approval",
        &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":null}}"#),
        Some(&bearer(&manager)),
    );
    assert_eq!(denied_again["err"]["variant"], "NotStateOwner", "{denied_again:?}");

    // The director approves -- moves to PendingVpApproval.
    let (_, approved2) =
        http_request(port, "/api/advance_purchase_approval", &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":null}}"#), Some(&bearer(&director)));
    assert_eq!(approved2["ok"], true, "{approved2:?}");

    // The VP gives the final approval -- terminal Approved state.
    let (_, approved3) =
        http_request(port, "/api/advance_purchase_approval", &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":null}}"#), Some(&bearer(&vp)));
    assert_eq!(approved3["ok"], true, "{approved3:?}");
}

#[test]
fn pending_for_me_only_shows_instances_this_role_currently_owns() {
    let port = start_server();
    let manager = mint_token("alice", r#"[\"manager\"]"#);
    let director = mint_token("bob", r#"[\"director\"]"#);

    let (_, submit) = http_request(
        port,
        "/api/submit_purchase_order",
        r#"{"description":{"value":"chairs"},"amount_cents":10000}"#,
        Some(&bearer(&manager)),
    );
    let instance_id = submit["ok"].as_i64().unwrap();

    let (_, manager_queue) = http_request(port, "/api/list_purchase_approval_pending_for_me", "{}", Some(&bearer(&manager)));
    let rows = manager_queue["ok"].as_array().expect("expected an array");
    assert!(rows.iter().any(|r| r["instance_id"] == instance_id), "{rows:?}");
    assert_eq!(rows.iter().find(|r| r["instance_id"] == instance_id).unwrap()["state_label"], "Pending Manager Approval");

    let (_, director_queue) = http_request(port, "/api/list_purchase_approval_pending_for_me", "{}", Some(&bearer(&director)));
    let director_rows = director_queue["ok"].as_array().expect("expected an array");
    assert!(!director_rows.iter().any(|r| r["instance_id"] == instance_id), "the director shouldn't see a manager-stage item yet: {director_rows:?}");
}

#[test]
fn submitted_by_me_tracks_the_requester_across_every_stage_start_takes_an_optional_identity() {
    let port = start_server();
    let requester = mint_token("erin", r#"[\"employee\"]"#);
    let other = mint_token("frank", r#"[\"employee\"]"#);

    let (_, submit) = http_request(
        port,
        "/api/submit_purchase_order",
        r#"{"description":{"value":"AWS renewal"},"amount_cents":1250000}"#,
        Some(&bearer(&requester)),
    );
    let instance_id = submit["ok"].as_i64().unwrap();

    let (_, mine) = http_request(port, "/api/list_purchase_approval_submitted_by_me", "{}", Some(&bearer(&requester)));
    let rows = mine["ok"].as_array().expect("expected an array");
    assert!(rows.iter().any(|r| r["instance_id"] == instance_id), "{rows:?}");
    let row = rows.iter().find(|r| r["instance_id"] == instance_id).unwrap();
    assert_eq!(row["data"]["requester_subject"], "erin");

    // Someone else's "my requests" doesn't include it.
    let (_, others) = http_request(port, "/api/list_purchase_approval_submitted_by_me", "{}", Some(&bearer(&other)));
    let other_rows = others["ok"].as_array().unwrap();
    assert!(!other_rows.iter().any(|r| r["instance_id"] == instance_id), "{other_rows:?}");

    // `Option(VerifiedIdentity)`: an anonymous submission is accepted (no
    // 401), and never shows up in anyone's "submitted by me" -- there's
    // no subject to attribute it to.
    let (status, anon) =
        http_request(port, "/api/submit_purchase_order_anonymously", r#"{"description":{"value":"pens"},"amount_cents":500}"#, None);
    assert_eq!(status, 200, "{anon:?}");
    assert!(anon["ok"].is_i64(), "{anon:?}");
    let anon_id = anon["ok"].as_i64().unwrap();
    let (_, requester_mine_again) = http_request(port, "/api/list_purchase_approval_submitted_by_me", "{}", Some(&bearer(&requester)));
    let rows2 = requester_mine_again["ok"].as_array().unwrap();
    assert!(!rows2.iter().any(|r| r["instance_id"] == anon_id), "an anonymous start must not be attributed to anyone: {rows2:?}");
}

#[test]
fn history_records_actor_via_link_and_comment_for_every_transition() {
    let port = start_server();
    let manager = mint_token("alice", r#"[\"manager\"]"#);

    let (_, submit) = http_request(
        port,
        "/api/submit_purchase_order",
        r#"{"description":{"value":"monitors"},"amount_cents":80000}"#,
        Some(&bearer(&manager)),
    );
    let instance_id = submit["ok"].as_i64().unwrap();

    http_request(
        port,
        "/api/advance_purchase_approval",
        &format!(r#"{{"instance_id":{instance_id},"event":"Approved","payload":{{"comment":"budget confirmed"}}}}"#),
        Some(&bearer(&manager)),
    );

    let (status, history) =
        http_request(port, "/api/get_purchase_approval_history", &format!(r#"{{"instance_id":{instance_id}}}"#), Some(&bearer(&manager)));
    assert_eq!(status, 200, "{history:?}");
    let rows = history["ok"].as_array().expect("expected an array");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["from_state"], "PendingManagerApproval");
    assert_eq!(rows[0]["to_state"], "PendingDirectorApproval");
    assert_eq!(rows[0]["event"], "Approved");
    assert_eq!(rows[0]["actor_subject"], "alice");
    assert_eq!(rows[0]["via_link"], false);
    assert_eq!(rows[0]["comment"], "budget confirmed");
}

#[test]
fn a_state_with_no_owner_warns_but_an_owned_one_does_not() {
    let unowned = build_program(
        r#"
        workflow NoOwner {
            data { }
            state Start { on Go -> Done }
            state Done terminal { }
        }
        fn main() {}
    "#,
    );
    let warnings = nirdosha::typeck::workflow_owner_warnings(&unowned);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(matches!(
        &warnings[0].kind,
        nirdosha::typeck::TypeWarningKind::WorkflowStateHasNoOwner { workflow, state }
            if workflow == "NoOwner" && state == "Start"
    ));

    let owned = build_program(FIXTURE_SRC);
    assert!(nirdosha::typeck::workflow_owner_warnings(&owned).is_empty(), "every non-terminal state in the fixture declares an owner");
}
