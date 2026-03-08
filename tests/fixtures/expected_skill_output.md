---
name: example-com-docs
description: "Use when the user needs information or instructions from example.com related to: Example docs for deterministic extraction."
---

# example.com Web Skill

## References

Load the full extracted page content when you need deeper detail:

- [REFERENCE.md](references/REFERENCE.md)

Treat `REFERENCE.md` as the source of truth for extracted content. Treat `Discovered Pages` as navigation hints only; those pages were not extracted into this skill.

## Instructions

1. Run pnpm check before committing.
2. Use this endpoint for metadata retrieval.

## Commands / API Usage

```bash
pnpm check
cargo clippy --all-targets --all-features
```

### API References

- GET /v1/skills
- https://example.com/api/v1/skills

## Discovered Pages

- https://example.com/docs/intro
- https://example.com/docs/setup

## Source

- Original Source URL: https://example.com/docs
- Final Source URL: https://example.com/docs
- Content SHA-256: 0123456789abcdef
- Pipeline Stage: explicit_markdown

## Source Excerpts

> These docs explain setup and usage.
> Run commands in order for deterministic results.
