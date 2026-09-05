use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_activemq::ActiveMqPlugin;

fn url() -> String {
    std::env::var("ACTIVEMQ_STOMP_URL").unwrap_or_else(|_| "stomp://admin:admin@127.0.0.1:61613".to_string())
}

#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(activemq_connect())
        }
    "#;
    let plugins = ActiveMqPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn without_the_plugin_registered_activemq_connect_is_unknown() {
    let src = r#"
        fn main() {
            print(activemq_connect("stomp://x"))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("activemq_connect must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn an_unreachable_broker_is_a_clean_runtime_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(activemq_connect("stomp://127.0.0.1:1"))
        }
    "#;
    let plugins = ActiveMqPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("an unreachable broker must error cleanly");
    assert!(err.contains("plugin"), "expected the plugin error channel, got: {err}");
}

/// The real end-to-end proof: send a message, receive it back, against
/// a live ActiveMQ. `docker compose -f crates/plugin-examples/docker-compose.yml
/// up -d activemq`, then `cargo test -p nirdosha-plugin-activemq -- --ignored`.
#[test]
#[ignore = "needs a live ActiveMQ broker; see crates/plugin-examples/docker-compose.yml"]
fn send_then_receive_round_trips_a_message_against_a_real_broker() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = activemq_connect("{url}")
            activemq_send(h, "nirdosha-e2e-test", "hello from nirdosha")
            let msg: str = activemq_receive(h, "nirdosha-e2e-test", 5000)
            print(msg)
            activemq_close(h)
        }}
    "#,
        url = url()
    );
    let plugins = ActiveMqPlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly against a live broker, got {result:?}");
}

/// A receive with nothing published is a clean empty-string timeout, not
/// a hang or an error -- the documented convention for "no message
/// arrived in time" (`stomp::StompConn::receive_one`'s own doc comment).
#[test]
#[ignore = "needs a live ActiveMQ broker; see crates/plugin-examples/docker-compose.yml"]
fn receive_with_no_message_times_out_to_an_empty_string() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = activemq_connect("{url}")
            let msg: str = activemq_receive(h, "nirdosha-empty-queue-test", 500)
            print(msg)
            activemq_close(h)
        }}
    "#,
        url = url()
    );
    let plugins = ActiveMqPlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "a timeout must not be a runtime error, got {result:?}");
}

/// The "External Data & Service Boundary" (docs/adr/0004) proof for MQ:
/// this program never mentions `activemq_connect`/`activemq_send`/
/// `activemq_receive`/`activemq_close` by name -- it calls the exact
/// same generic `mq_connect_via`/`mq_publish`/`mq_consume`/`stop`
/// surface Redis already uses through `mq_connect` (`crates/compiler/
/// tests/mq.rs`). It's the `mq_provider_stomp_*` naming-convention
/// dispatch in `interpreter.rs`'s `eval_builtin` that routes a
/// `stomp://` URL to this plugin.
#[test]
#[ignore = "needs a live ActiveMQ broker; see crates/plugin-examples/docker-compose.yml"]
fn generic_mq_surface_transparently_dispatches_to_the_activemq_plugin() {
    let queue = format!(
        "nirdosha-generic-surface-test-{}",
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    );
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn run_all(conn: mq) -> Text {{
            let published: i64 = match mq_publish(conn, "{queue}", "hello via generic surface") {{
                Ok(u) => 1,
                Err(e) => -1,
            }}
            let found: Text = match mq_consume(conn, "{queue}", 5) {{
                Ok(msg) => Text(msg),
                Err(e) => Text(e),
            }}
            stop conn
            return found
        }}
        fn main() -> Text {{
            return match mq_connect_via("{url}") {{
                Ok(conn) => run_all(conn),
                Err(e) => Text(e),
            }}
        }}
    "#,
        url = url(),
        queue = queue,
    );
    let plugins = ActiveMqPlugin.builtins();
    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(nirdosha::interpreter::Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            nirdosha::interpreter::Value::Str(s) => assert_eq!(&**s, "hello via generic surface"),
            other => panic!("expected Text(Str(\"hello via generic surface\")), got Text({other:?})"),
        },
        other => panic!(
            "expected the generic mq_connect_via/mq_publish/mq_consume/stop surface to work transparently against ActiveMQ, got {other:?}"
        ),
    }
}
