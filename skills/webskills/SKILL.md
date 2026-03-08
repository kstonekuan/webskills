---
name: webskills
description: Generate and install an agent skill from a public webpage with WebSkills. Use when a user wants to turn a public docs, help, reference, wiki, or spec page into a reusable local skill, preview extracted skill artifacts, or install the generated skill with `npx skills add`.
---

# WebSkills

## Inputs Required

1. A public `http://` or `https://` URL.
2. Optional custom skill name slug.
3. Optional custom output directory.

## Default Workflow

1. Generate and install in one command:

```bash
npx webskills add <site-url>
```

2. Confirm success output includes:
- generated skill directory path
- pipeline stage used
- content hash

3. Verify installed skill is available to the target agent.

## Safe Preview Workflow

Use this when the user wants to inspect output before installing:

```bash
npx webskills extract --url <site-url> --output .webskills/generated
```

Then inspect generated files:
- `SKILL.md`

## Manual Install Workflow

If generation was run without install, install the produced directory explicitly:

```bash
npx skills add <generated-skill-directory>
```

## Useful Options

- Set custom output directory:

```bash
npx webskills add <site-url> --output <dir>
```

- Set custom skill slug:

```bash
npx webskills add <site-url> --name <skill-name>
```

- Increase fetch timeout:

```bash
npx webskills add <site-url> --timeout-ms 30000
```

## Agent Behavior Rules

1. Only process the user-provided page as the primary source.
2. Do not invent undocumented capabilities.
3. Keep discovered links as references only; do not treat them as crawled content unless explicitly requested.
4. Prefer deterministic reruns rather than editing generated output by hand.

## Troubleshooting

- If HTTPS certificate validation fails in restricted environments, retry with another reachable public URL.
- If installation is not desired, use `extract` instead of `add`.
- If `npx webskills` is unavailable, run `cargo run -- add <site-url>` from a source checkout or install the published npm package first.
