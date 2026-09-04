# Trusted plugins

A lightweight, self-declared trust convention for Kind-A Nirdosha
plugins (`docs/ECOSYSTEM.md` §G1 / rfcs/0004-native-plugin-sandboxing.md)
— explicitly **not** new registry infrastructure (§G1 already rejects
building a bespoke registry/hosting service), just a repo file a
maintainer reviews before adding a row, the same spirit as GitHub's
Verified Publisher badge.

## What this is, and isn't

A Kind-A plugin is arbitrary native Rust code, statically linked
directly into whatever binary calls `run_with_plugins`/`serve::run` —
full process trust, no sandbox (rfcs/0004-native-plugin-sandboxing.md's
own open-question section explains why that's a deliberate, disclosed
gap, not solved by this file). Appearing on this list means:

- A maintainer has read the plugin's source at the version listed.
- Its declared `[package.metadata.nirdosha]` builtins and their
  `effects` (rfcs/0003-plugin-abi-v2.md) match what the code actually
  does, as best a human review can tell.
- It has no obvious malicious behavior or gratuitous unsafe code.

Appearing on this list does **not** mean:

- The plugin is free of bugs, or its declared effects can't be wrong —
  nothing *enforces* the declaration is true (rfcs/0004's own honest
  limitation).
- Nirdosha's maintainers audit every future version — a listing
  reflects the version pinned below; re-review on update is manual,
  not automatic.
- Any runtime isolation exists. A trusted plugin can still corrupt the
  host process's memory the same as any other native Rust dependency
  with a genuine bug.

## Listed plugins

| Crate | Version reviewed | Declared effects | Notes |
|---|---|---|---|
| `nirdosha-plugin-rot13` | 0.1.0 | (none — pure) | The reference plugin; trivial, no I/O. |
| `nirdosha-plugin-mysql` | 0.1.0 | network | Sync `mysql` crate. See `crates/plugin-example-mysql/README.md`. |
| `nirdosha-plugin-activemq` | 0.1.0 | network | Hand-rolled STOMP client, not a third-party crate — see its README for why. |
| `nirdosha-plugin-cassandra` | 0.1.0 | network | Async `scylla` driver, bridged via `nirdosha-plugin-support`. |
| `nirdosha-plugin-neo4j` | 0.1.0 | network | Async `neo4rs` driver, same bridge. |
| `nirdosha-plugin-hbase` | 0.1.0 | network | `hbase-thrift` — see its README's "why this is the hardest one" for real, disclosed rough edges. |

All six are reference plugins in this repository (`crates/plugin-example-*/`),
reviewed as part of building them, not third-party submissions — this
table's real purpose is to be the template a genuine third-party
plugin's listing follows, and the day-one gate a future auto-discovery
step (RFC 0001) requires before linking an unfamiliar plugin
automatically.

## Requesting a listing

Open a PR adding a row above, with:

1. The crate name, exact version (a git tag/commit for an unpublished
   crate), and a link to its source.
2. Its full `[package.metadata.nirdosha]` builtins list, including
   `effects`.
3. A one-line description of what it does and what external system (if
   any) it talks to.

A maintainer reviews the source before merging — see
[`GOVERNANCE.md`](./GOVERNANCE.md) for who that is today. Expect this
to take real review time, not to be a formality; that's the entire
point of the list existing.
