# Token Tracker

A Rust tool for tracking token usage.

## Requirements

- Rust 1.85 or later

## Run

```sh
cargo run
```

## Development

```sh
cargo fmt --check
cargo check
cargo test
```

## Structure

- `src/core/` — shared domain types and rules
- `src/application/` — collection and query use cases
- `src/adapters/` — parsers for Pi, Codex
- `src/lib.rs` — reusable API for any frontend
- `src/main.rs` — current CLI presentation entry point

Dependencies should point inward: presentation and adapters may use the
application/core modules, while the core remains independent.

This project is in early development.
