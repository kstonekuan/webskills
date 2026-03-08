# Contributing

## Code Quality

```bash
cargo fmt --all -- --check                  # Formatting
cargo clippy --all-targets --all-features -- -D warnings  # Linting
cargo test                                  # Tests
```

## Code Style & Philosophy

### Typing & Pattern Matching

- Prefer **explicit types** over raw dicts -- make invalid states unrepresentable where practical
- Prefer **typed variants over string literals** when the set of valid values is known
- Use **exhaustive pattern matching** so the compiler can verify all cases are handled
- Structure types to enable exhaustive matching when handling variants
- Prefer **shared internal functions over factory patterns** when extracting common logic from related call sites

### Self-Documenting Code

- **Verbose naming**: Variable and function naming should read like documentation
- **Strategic comments**: Only for non-obvious logic or architectural decisions; avoid restating what code shows
