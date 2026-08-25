//! Tests for `nirdosha init <project-name>`: the generator half
//! (`nirdosha::init::generate_source`/`render_launcher_*`, pure text —
//! same lex/parse/typecheck/ownership-check harness pattern as
//! `compiler/tests/emit_ui.rs`) and the CLI half (`cmd_init` in
//! `main.rs`, exercised by spawning the real `nirdosha` binary against a
//! scratch directory, the same `std::process::Command` pattern
//! `compiler/tests/codegen.rs` already uses for compiled-binary output).

use nirdosha::init::InitOptions;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;

fn typechecks(src: &str) {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn default_options_emit_email_and_role_mapping_and_typecheck() {
    let src = nirdosha::init::generate_source("shop", &InitOptions::default()).expect("valid name");
    typechecks(&src);

    assert!(src.contains("struct EmailProviderConfig"));
    assert!(src.contains("fn list_email_provider_config"));
    assert!(src.contains("fn create_email_provider_config"));
    assert!(src.contains("fn update_email_provider_config"));
    assert!(src.contains("fn delete_email_provider_config"));

    assert!(src.contains("struct RoleMapping"));
    assert!(src.contains("fn list_role_mapping"));
    assert!(src.contains("fn create_role_mapping"));
    assert!(src.contains("fn update_role_mapping"));
    assert!(src.contains("fn delete_role_mapping"));

    assert!(!src.contains("SmsProviderConfig"), "sms is opt-in, off by default");
    assert!(!src.contains("PushProviderConfig"), "push is opt-in, off by default");

    // Exactly one distinct db_connect(...) literal, used everywhere (the
    // `db_connect("` prefix -- as opposed to `db_connect(` -- excludes the
    // header comment's own prose mention of `db_connect(...)`).
    assert_eq!(count_occurrences(&src, "db_connect(\"shop.db\")"), count_occurrences(&src, "db_connect(\""));
}

#[test]
fn sms_and_push_add_the_identical_shape_under_their_own_names() {
    let opts = InitOptions { email: false, roles: false, sms: true, push: true };
    let src = nirdosha::init::generate_source("shop", &opts).expect("valid name");
    typechecks(&src);

    assert!(src.contains("struct SmsProviderConfig"));
    assert!(src.contains("fn list_sms_provider_config"));
    assert!(src.contains("struct PushProviderConfig"));
    assert!(src.contains("fn list_push_provider_config"));
    assert!(!src.contains("EmailProviderConfig"));
    assert!(!src.contains("RoleMapping"));
}

#[test]
fn all_fixtures_disabled_still_typechecks_as_an_item_less_program() {
    let opts = InitOptions { email: false, roles: false, sms: false, push: false };
    let src = nirdosha::init::generate_source("shop", &opts).expect("valid name");
    typechecks(&src);
    assert!(!src.contains("struct Text"), "no fixtures enabled means Text isn't needed either");
    assert!(!src.contains("struct EmailProviderConfig"));
    assert!(!src.contains("struct RoleMapping"));
    assert!(src.contains("no structs yet"));
}

#[test]
fn project_name_with_a_quote_is_rejected() {
    let err = nirdosha::init::generate_source("sho\"p", &InitOptions::default())
        .expect_err("a `\"` in the name would break out of a db_connect(...) literal");
    assert!(err.contains("sho"));
}

#[test]
fn project_name_with_a_newline_is_rejected() {
    assert!(nirdosha::init::generate_source("sho\np", &InitOptions::default()).is_err());
}

#[test]
fn launcher_scripts_invoke_serve_with_the_project_db() {
    let sh = nirdosha::init::render_launcher_unix("shop");
    assert!(sh.starts_with("#!/bin/sh"));
    assert!(sh.contains("serve shop.nir"));
    assert!(sh.contains("--db shop.db"));

    let bat = nirdosha::init::render_launcher_windows("shop");
    assert!(bat.starts_with("@echo off"));
    assert!(bat.contains("serve shop.nir"));
    assert!(bat.contains("--db shop.db"));
}

#[test]
fn placeholder_jwks_is_a_valid_empty_key_set() {
    let jwks = nirdosha::init::placeholder_jwks();
    let parsed: serde_json::Value = serde_json::from_str(jwks).expect("placeholder jwks must parse as JSON");
    assert_eq!(parsed["keys"].as_array().expect("keys array").len(), 0);
}

// --- CLI half: spawns the real `nirdosha` binary against a scratch dir ---

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("nirdosha_init_test_{}_{}_{}", std::process::id(), unique_suffix(), name));
    p
}

#[test]
fn cli_scaffolds_a_self_contained_project_folder() {
    let dest = scratch_dir("scaffold");
    std::fs::create_dir_all(&dest).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .args(["init", "demo", "--dest"])
        .arg(&dest)
        .output()
        .expect("nirdosha init should run");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));

    let project_dir = dest.join("demo");
    assert!(project_dir.join("demo.nir").is_file());
    assert!(project_dir.join("jwks.json").is_file());
    let exe_name = format!("nirdosha{}", std::env::consts::EXE_SUFFIX);
    assert!(project_dir.join(&exe_name).is_file(), "bundled executable should be present");

    let launcher = if cfg!(windows) { "run.bat" } else { "run.sh" };
    assert!(project_dir.join(launcher).is_file());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(project_dir.join(launcher)).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "launcher script should be executable");
    }

    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn cli_refuses_to_overwrite_without_force_and_succeeds_with_it() {
    let dest = scratch_dir("overwrite");
    std::fs::create_dir_all(&dest).unwrap();

    let first = std::process::Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .args(["init", "demo", "--dest"])
        .arg(&dest)
        .output()
        .expect("first init should run");
    assert!(first.status.success());

    let second = std::process::Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .args(["init", "demo", "--dest"])
        .arg(&dest)
        .output()
        .expect("second init should run");
    assert!(!second.status.success(), "without --force, a second init on the same target should fail");

    let forced = std::process::Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .args(["init", "demo", "--dest"])
        .arg(&dest)
        .arg("--force")
        .output()
        .expect("forced init should run");
    assert!(forced.status.success(), "stderr: {}", String::from_utf8_lossy(&forced.stderr));

    let _ = std::fs::remove_dir_all(&dest);
}

#[test]
fn cli_creates_the_dest_directory_if_missing() {
    let dest = scratch_dir("newdest");
    // Deliberately not created -- `--dest` should create it.
    assert!(!dest.exists());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nirdosha"))
        .args(["init", "demo", "--dest"])
        .arg(&dest)
        .output()
        .expect("nirdosha init should run");
    assert!(output.status.success(), "stderr: {}", String::from_utf8_lossy(&output.stderr));
    assert!(dest.join("demo").join("demo.nir").is_file());

    let _ = std::fs::remove_dir_all(&dest);
}
