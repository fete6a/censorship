# Native maintenance and regression tests

This crate replaces the Python D1 release checker and offline test suite. It is
separate from the Worker crate: SQLite, `syn` and native tooling are never linked
into the deployed Wasm. The production Rust sources, schema and policy defaults
are unchanged by this migration.

```sh
cargo test --manifest-path tools/maintenance/Cargo.toml --locked
cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --print-inspection-sql
cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --from-json response.json --pre-migration
cargo run --manifest-path tools/maintenance/Cargo.toml --bin check-d1-schema -- --remote
```

The command exits **0** for ready, **3** for a verified additive migration, or
**2** for incompatible metadata, malformed input, invocation errors or timeout.
Choose exactly one input mode. `--remote` invokes the installed Wrangler with
literal arguments, the repository's absolute config path and a 60-second limit;
it never installs Wrangler or runs migration SQL. `--config PATH` selects another
Wrangler configuration explicitly. Offline modes need neither Node nor Wrangler.
The canonical schema is embedded in the binary when it is built; rebuild the
checker whenever the checked-out schema changes.

Local SQLite uses `rusqlite` with a bundled SQLite engine (which is C), rather
than Python's SQLite adapter. Only trusted canonical SQL runs in these temporary
in-memory databases. Production inspection reads schema metadata and literal
`PRAGMA table_info` results; extracted production trigger definitions are never
executed locally or remotely.

## Test coverage after migration

Related Python cases are consolidated into table-driven Rust tests rather than
retaining duplicate approximations of production SQL. Store tests use `syn` to
extract actual Rust string literals, so edits to production queries are tested.

| Rust target | Replaced Python coverage |
| --- | --- |
| `schema_release` | Legacy/additive/partial schema migrations, preservation and idempotence, trigger definitions and string semantics, unsafe receipt states, strict metadata parsing and read-only inspection |
| `store` | Actual archive/digest/anchor SQL, warning baselines and receipt replay, rowid retention, delayed-ban boundaries, fair retry/backoff, cancellation and group isolation, scan generations/expired leases and historical writes |
| `contracts` | Deployment isolation, configuration, command ordering, administrator exemptions, notification defaults/recipients, bilingual privacy/permission documentation, importer/transcoder integration |
| `queue_simulation` | Deterministic request-count/cache simulation, independent acknowledgement and retry behavior (not a production benchmark) |
| `cli` | Exit codes, malformed input, mode exclusivity, missing files, exact read-only Wrangler invocation and failed subprocess handling |

The existing `tests/worker_runtime.mjs` deliberately remains JavaScript. It loads
the real `worker-build` JS/Wasm modules in a Node VM and doubles Cloudflare/Telegram
services at their JavaScript boundary. Replacing that harness with native Rust
unit tests would stop testing the actual host integration. Its tests still run in
CI after building the Worker. Generated `build/index.js` remains required by the
current `workers-rs`/`wasm-bindgen` deployment path; it is not handwritten business
logic. Wrangler also remains the official development CLI.
