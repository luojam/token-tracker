The suite starts at `integration.rs`, grouped by component:

- `adapters/`: discovery and parsing.
- `application/`: sync, reconciliation, and reports.
- `pricing/`: rates, costs, and precision.
- `storage/`: persistence and failure handling.
- `cli/`: command wiring, privacy, and failures.

Reuse `support/` and fixtures. Name tests after behavior and keep expected values
independent of the implementation. Test lifecycles at the application layer, not
again per adapter or CLI. Reserve inline unit tests for private behavior.

Run all tests with `cargo test`, or filter by module:

```sh
cargo test --test integration adapters::codex
cargo test --test integration storage::
```
