# Nirdosha — feature catalogue

One `.nir` file per language feature, each independently runnable
(`nirdosha <file>`) and verified against `target/debug/nirdosha`. Every
file is self-contained — no external services required, though a few
(25/28/47) degrade gracefully (a real `Err`, not a crash) when Redis or
network access isn't available.

`45_module_namespacing.nir` `use`s `45_module_namespacing_helper.nir` —
that's the one file here meant to be read, not run, on its own.

| # | File | Feature |
|---|---|---|
| 01 | `01_scalar_types_and_literals.nir` | `i8`–`i64`/`u8`–`usize`/`f64`/`bool`/`str`/`unit`, literal widths |
| 02 | `02_operators.nir` | arithmetic/comparison/logical/unary, `Vector`/`Matrix` elementwise & linear-algebra forms |
| 03 | `03_control_flow_and_functions.nir` | `fn`, `let`/reassignment, `if`/`else` (stmt & expr), `while`, recursion |
| 04 | `04_first_class_functions.nir` | plain function names as `fn(T)->R` values, higher-order functions |
| 05 | `05_structs_enums_match.nir` | `struct`/`enum`/`match` — Row 11 layer 1 |
| 06 | `06_generics.nir` | generic `struct`/`enum` — Row 11 layer 6 |
| 07 | `07_option_result_prelude.nir` | built-in `Option(T)`/`Result(T,E)` — Row 11 layer 7 |
| 08 | `08_str_boundary_convention.nir` | the `str`-at-fn-boundary ban ("enum favoring") + `Text` carrier convention |
| 09 | `09_money_and_currency.nir` | prelude `Money`/`CurrencyCode` |
| 10 | `10_measure_and_unit.nir` | prelude `Measure`/`UnitCode` |
| 11 | `11_decimal_dec128.nir` | `dec128` fixed-point decimal arithmetic |
| 12 | `12_vector_matrix_linalg.nir` | `Vector(T,N)`/`Matrix(T,R,C)` + dense linear-algebra builtins |
| 13 | `13_deterministic_simulation.nir` | `rand_seed`/`rand_f64`/`rand_gaussian`, geometry (ECEF/ENU/bearing/distance), Kalman filter steps |
| 14 | `14_ownership_box.nir` | `box T`, `*` deref, affine move semantics |
| 15 | `15_borrowing.nir` | `&T` shared borrow |
| 16 | `16_effects.nir` | `effect(pure \| rng \| io \| concurrent \| network)` |
| 17 | `17_audited_block.nir` | `audited "justification" { ... }` guard-suppression escape hatch |
| 18 | `18_threads.nir` | `thread T`, `spawn`, `join` |
| 19 | `19_channels.nir` | `chan T`, `send`, `recv` |
| 20 | `20_sandbox.nir` | `sandbox`, `stop` — real separate OS process |
| 21 | `21_sandbox_channels.nir` | `sandbox` + `chan` — cross-process IPC |
| 22 | `22_tcp_client.nir` | `tcp`, `connect`, `send`/`recv`/`stop` |
| 23 | `23_tcp_listener.nir` | `tcp_listener`, `listen`, `accept` |
| 24 | `24_file_io.nir` | `file`, `open` (`"r"`/`"w"`/`"a"`) |
| 25 | `25_json.nir` | `json_parse`/`json_get_*`/`json_array_*`/`json_set_str` |
| 26 | `26_http_and_https.nir` | `http_get`/`http_post`/`https_get`/`https_post` |
| 27 | `27_database.nir` | `db_connect`/`db_query`/`db_execute` (SQLite/Postgres) |
| 28 | `28_message_queue.nir` | `mq_connect`/`mq_publish`/`mq_consume` (Redis) |
| 29 | `29_crypto_hashing.nir` | `sha256_hex`, `constant_time_str_eq` |
| 30 | `30_identity_oidc.nir` | `oidc_validate_token`/`check_role`/`extract_claim`/`identity_expired` |
| 31 | `31_mock_identity_provider.nir` | `mock_issue_token` — the mock-IdP inverse of `oidc_validate_token` |
| 32 | `32_sessions_and_api_keys.nir` | `create_application_session`/`session_cookie`/`new_refresh_token`/`exchange_refresh_token`/`check_revocation`/`validate_api_key` |
| 33 | `33_privileged_functions.nir` | `requires(role:...)`/`requires(claim:...,...)` + `acquire` |
| 34 | `34_requires_public.nir` | `requires(public)` |
| 35 | `35_validate_contracts.nir` | `validate { pre:/post: }` Hoare contracts |
| 36 | `36_transact.nir` | `transact { precheck?/network/verify/commit/compensate?/log? }` |
| 37 | `37_transact_cross_process.nir` | `transact`'s cross-process layer + `retry`/`timeout` |
| 38 | `38_workflow.nir` | `workflow { data/state/on_entry/on <Event> -> <State>/terminal/owner }` |
| 39 | `39_screen_ui.nir` | `screen <Struct> { title/field/action }` |
| 40 | `40_dashboard.nir` | `dashboard { tile/chart }` |
| 41 | `41_dashboard_visual.nir` | `dashboard { visual ... { render: "graph"\|"heatmap"\|"timeline" } }` |
| 42 | `42_workspace_panel.nir` | `workspace { subject/panel }` composite multi-panel screens |
| 43 | `43_layout.nir` | `layout { row/column/grid/group/tabs/divider/timeline }` |
| 44 | `44_module_nav_grouping.nir` | `module "Display Name" { ... }` — legacy nav grouping, not scoping |
| 45 | `45_module_namespacing.nir` (+ `..._helper.nir`) | `module Ident { pub ... }` real namespacing + `use` |
| 46 | `46_db_schema_and_role_mapping_conventions.nir` | `serve --db` auto schema migrations + `RoleMapping` identity cache (pure convention, no new syntax) |
| 47 | `47_external_service_boundary.nir` | plugin-backed `db`/`mq` by URL scheme (`db_connect`/`mq_connect_via`) |

## Deliberately not given their own file

Properties of the toolchain/compiler rather than `.nir` syntax you write:

- **Execution modes** (`nirdosha <file>` / `build` / `emit-llvm` /
  `emit-ast` / `emit-ui` / `serve`, `--format=json`) — every feature
  file above is itself run through the interpret path; several
  (workflow/screen/dashboard-bearing ones) are also checked with
  `emit-ui`.
- **Static guarantees** (type checking, ownership/move-checking,
  interval analysis, Z3 bounds proving) — properties every file above
  is already subject to, not a separate construct to demonstrate.
  `17_audited_block.nir` is the one place these become visibly
  controllable.
- **Determinism** (per-instance RNG) — the guarantee
  `13_deterministic_simulation.nir`'s `rand_seed` relies on.
- **What's compiled vs. interpreter-only** — a property of each
  construct above, not a construct of its own (see `LANGUAGE.md` §10).
- **`--theme`/design-token theming, live reload** — a CLI flag +
  external `theme.json`, no `.nir` syntax at all.
