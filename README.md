# WebSkills

WebSkills turns any webpage into an agent skill.

WebSkills first tries native `npx skills add` installation from a URL. If the site does not already expose an agent-ready surface, it falls back to deterministic single-page extraction and installs the generated skill locally.

It is built for pages that are useful to agents but are not yet packaged as skills: docs pages, wiki/reference pages, help centers, specs, and technical articles.

WebSkills fits into the broader [Agent Skills ecosystem](https://agentskills.io/) by packaging pages that are not already exposed through surfaces like `skill.md`, `/.well-known/skills/`, `llms.txt`, MCP endpoints, or markdown negotiation.

## Quickstart

Install the CLI locally from the repository:

```bash
cargo install --path .
```

Then install from URL first, with auto-fallback to extraction if needed:

```bash
webskills add https://example.com/page
```

Common cases:

```bash
# Forward install flags to `skills add`
webskills add https://example.com/page --yes --global --agent claude-code cursor

# Extract without running any install command
webskills extract --url https://example.com/page --output .webskills/generated
```

## Why WebSkills?

Use WebSkills when a page matters to your agents, but the site does not already provide a clean install surface.

WebSkills gives you:

- direct install when the URL already works with `skills add`
- deterministic extraction from a single source page when it does not
- stable, reviewable skill output in `SKILL.md`
- local reference copy for traceability in `references/REFERENCE.md`
- a read-only pipeline against publicly reachable web content

## How It Works

1. `add` tries `npx skills add <url>` first.
2. If direct install fails, WebSkills runs its extraction pipeline.
3. The extracted page is written as a local skill directory and then installed through `skills add`.

By default, `add` stays aligned with `skills add`: if installation requires confirmation, it remains interactive. Use `-y` or `--yes` to skip those prompts. Use `extract` when you only want generated artifacts.

## CLI Reference

```bash
webskills add <url> [--output <dir>] [--name <skill-name>] [--timeout-ms <n>] [--yes] [--global] [--agent <agents...>] [--skill <skills...>]
webskills extract --url <url> --output <dir> [--name <skill-name>] [--timeout-ms <n>]
```

Behavior notes:

- `--yes`, `--global`, `--agent`, and `--skill` are forwarded to `skills add`.
- Without `--yes`, `add` keeps the `skills add` interaction flow and requires a TTY for prompts.
- When `--yes` is set, WebSkills also forwards `--yes` to `npx` for non-interactive resolution.

For removing a skill or other skill-management operations, use `npx skills` directly.

## Output

When extraction runs through `extract` or fallback from `add`, generated artifacts are written under the configured output directory. By default that is:

```text
.webskills/generated/<skill-slug>-<sha-prefix>/
```

If you pass `--output`, artifacts are written under:

```text
<output>/<skill-slug>-<sha-prefix>/
```

Contents:

```text
SKILL.md
references/REFERENCE.md
```

## Access Requirements And Caveats

WebSkills can only extract content that is publicly reachable from the machine running the CLI.

Extraction may fail or degrade when:

- the target is behind bot mitigation or WAF challenges
- the site is geo/IP/ASN restricted
- content requires login, session cookies, or JavaScript-gated auth
- markdown endpoints redirect incorrectly or loop
- the origin aggressively rate limits (`429`) or terminates early

## Extraction Pipeline Reference

Fallback order used by extraction:

1. Explicit markdown surfaces
   - Path-local candidates first (target directory):
   - `<target-directory>/llms.txt`
   - `<target-directory>/llm.txt`
   - `<target-directory>/docs.md`
   - `<target-directory>/README.md`
   - If the input URL is the site root, origin-level fallback candidates are:
   - `/llms.txt`
   - `/llm.txt`
   - `/docs.md`
   - `/README.md`

2. Markdown negotiation
   - Request the input URL with `Accept: text/markdown`

3. HTML fallback
   - Fetch HTML and convert to markdown deterministically

Behavior notes:

- Explicit markdown probing skips HTML-like responses so challenge/error pages are not treated as markdown sources.
- If one candidate errors, WebSkills continues with remaining candidates and stages.
- If every stage fails, extraction returns:
  `Unable to fetch any usable source document for extraction pipeline.`

## Build From Source

Requirements:

- Rust toolchain (`cargo`, `rustfmt`, `clippy`)
- `cargo-dist` for release maintainers

Install the CLI locally from the repository:

```bash
cargo install --path .
```

Run the CLI directly from a source checkout:

```bash
cargo run -- add https://example.com/page
cargo run -- extract --url https://example.com/page --output .webskills/generated
```

Build release artifacts and installers with cargo-dist:

```bash
cargo install cargo-dist --locked --version 0.31.0
dist build
```

## Quality Checks

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Contributing

Contributor workflow and code quality conventions are documented in [CONTRIBUTING.md](./CONTRIBUTING.md).

## Release Automation

- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `dist-workspace.toml`

## Inspired By

WebSkills builds on existing work in agent skills and docs-for-agents patterns:

- Agent Skills standard: https://agentskills.io/
- Skills CLI (`npx skills add <name-or-url>`): https://skills.sh/docs/cli
- Mintlify AI docs patterns (`/skill.md`, `/.well-known/skills/`, `/llms.txt`, `/mcp`): https://mintlify.com/docs/ai/skillmd
- Cloudflare markdown negotiation (`Accept: text/markdown`): https://developers.cloudflare.com/fundamentals/reference/markdown-for-agents/

## License

MIT
