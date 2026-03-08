---
name: Agent
description: Use when creating, validating, or integrating reusable skill packages for AI agents. Reach for this skill when building domain-specific instructions, packaging workflows, or setting up skill discovery in agent systems.
metadata:
    mintlify-proj: agent
    version: "1.0"
---

# Agent Skills

## Product summary

Agent Skills is a lightweight, open format for extending AI agent capabilities with specialized knowledge and workflows. Skills are directories containing a `SKILL.md` file (YAML frontmatter + Markdown instructions) plus optional supporting files (scripts, references, assets). Use the `skills-ref` CLI to validate skills and generate prompt XML for agent integration. Primary documentation: https://agentskills.io

## When to use

Reach for this skill when:
- **Creating a new skill**: You need to package domain expertise, repeatable workflows, or new agent capabilities into a reusable format
- **Validating skills**: You're checking that a skill's SKILL.md file meets format requirements before deployment
- **Integrating skills into agents**: You're setting up skill discovery, loading metadata, or injecting skills into agent system prompts
- **Authoring instructions**: You're writing the Markdown body of a skill to guide agent behavior
- **Managing skill resources**: You're organizing scripts, templates, reference files, or assets alongside skill instructions

## Quick reference

### SKILL.md frontmatter (required fields)

| Field | Constraints | Example |
|-------|-----------|---------|
| `name` | Max 64 chars, lowercase + hyphens only, no leading/trailing hyphens | `pdf-processing` |
| `description` | Max 1024 chars, non-empty, describes what skill does and when to use | `Extract text and tables from PDFs, fill forms, merge documents.` |

### SKILL.md optional frontmatter fields

| Field | Purpose | Example |
|-------|---------|---------|
| `license` | License name or reference to bundled license file | `MIT` or `licenses/LICENSE.txt` |
| `compatibility` | Environment requirements (max 500 chars) | `Requires Python 3.8+, pdfplumber library` |
| `metadata` | Arbitrary key-value pairs for custom properties | `author: org-name` `version: "1.0"` |
| `allowed-tools` | Space-delimited pre-approved tools (experimental) | `Bash(git:*) Bash(jq:*)` |

### Directory structure

```
skill-name/
├── SKILL.md              # Required: frontmatter + instructions
├── scripts/              # Optional: executable code
├── references/           # Optional: detailed reference material
└── assets/               # Optional: templates, images, data files
```

### skills-ref CLI commands

```bash
# Validate a skill directory
skills-ref validate ./my-skill

# Generate <available_skills> XML for agent prompts
skills-ref to-prompt /path/to/skill1 /path/to/skill2
```

### File references in SKILL.md

Use relative paths from skill root:
```markdown
See [the guide](references/REFERENCE.md) for details.
Run the script: `scripts/extract.py`
```

## Decision guidance

### When to use filesystem-based vs tool-based agent integration

| Approach | When to use | Activation | Resource access |
|----------|-----------|-----------|-----------------|
| **Filesystem-based** | Agent has shell/bash environment; maximum capability needed | Agent issues `cat /path/to/skill/SKILL.md` | Shell commands access bundled files |
| **Tool-based** | Agent lacks dedicated computer environment; sandboxed execution | Developer implements custom tools | Tools provide access to bundled assets |

### When to include optional frontmatter fields

| Field | Include when | Skip if |
|-------|-------------|---------|
| `license` | Skill uses third-party code or has specific licensing | Skill is internal-only or unlicensed |
| `compatibility` | Skill requires specific system packages, Python versions, or network access | Skill is pure Markdown instructions |
| `metadata` | Tracking author, version, or custom properties is important | Not needed for simple skills |
| `allowed-tools` | Restricting which tools the skill can invoke is required | Agent doesn't support tool restrictions |

### When to move content to separate files

| Content type | Keep in SKILL.md | Move to separate file |
|-------------|-----------------|----------------------|
| Core instructions | < 500 lines | Detailed reference material |
| Quick examples | Yes | Exhaustive API documentation |
| Decision trees | Yes | Large lookup tables or schemas |
| Step-by-step workflows | Yes | Multi-page tutorials |

## Workflow

### Creating a new skill

1. **Create the directory structure**: Make a folder named `skill-name` (lowercase, hyphens only)

2. **Write the SKILL.md file**:
   - Start with YAML frontmatter: `name` and `description` (required)
   - Write the Markdown body with clear instructions
   - Keep the main file under 500 lines
   - Use relative paths for file references: `scripts/extract.py`, `references/GUIDE.md`

3. **Organize supporting files**:
   - `scripts/`: Executable code (Python, Bash, etc.)
   - `references/`: Detailed documentation, lookup tables, schemas
   - `assets/`: Templates, images, data files

4. **Validate the skill**:
   ```bash
   skills-ref validate ./skill-name
   ```
   Fix any frontmatter errors (name format, description length, etc.)

5. **Test the skill**: Read the SKILL.md file to verify instructions are clear and complete

### Integrating skills into an agent

1. **Discover skills**: Scan configured directories for folders containing `SKILL.md` files

2. **Load metadata at startup**: Parse only the YAML frontmatter from each skill:
   ```
   name: pdf-processing
   description: Extract text and tables from PDFs...
   ```
   This keeps initial context usage low (~100 tokens per skill)

3. **Generate prompt XML**:
   ```bash
   skills-ref to-prompt /path/to/skills/*
   ```
   Output includes name, description, and location for each skill

4. **Inject into system prompt**: Add the XML to your agent's system prompt so it knows available skills:
   ```xml
   <available_skills>
     <skill>
       <name>pdf-processing</name>
       <description>Extract text and tables from PDFs...</description>
       <location>/path/to/skills/pdf-processing/SKILL.md</location>
     </skill>
   </available_skills>
   ```

5. **Activate on demand**: When a user task matches a skill's description, load the full SKILL.md into context (~5000 tokens max)

6. **Execute resources**: Load referenced files or run scripts only when the skill instructions require them

## Common gotchas

- **Name format violations**: Names must be lowercase with hyphens only, no leading/trailing hyphens. `my-skill` ✓, `MySkill` ✗, `-my-skill` ✗
- **Description too long**: Max 1024 characters. Descriptions longer than this will fail validation. Be concise about what the skill does and when to use it.
- **Forgetting YAML delimiters**: Frontmatter must start and end with `---` on its own line. Missing delimiters break parsing.
- **Absolute paths in file references**: Always use relative paths from skill root (`scripts/extract.py`), never absolute paths (`/home/user/scripts/extract.py`)
- **Bloated SKILL.md**: Keeping the main file under 500 lines is a guideline, not a hard limit, but large files waste context. Move detailed reference material to `references/` subdirectory.
- **Missing location field for filesystem agents**: When injecting skills into filesystem-based agent prompts, include the absolute path to SKILL.md. Tool-based agents can omit this.
- **Not validating before deployment**: Always run `skills-ref validate` before sharing or deploying a skill. Frontmatter errors are silent until validation.
- **Unclear "when to use" description**: The description field is critical for agent matching. Write it as a clear trigger: "Use when the user needs to extract text from PDFs" not "PDF processing skill".

## Verification checklist

Before submitting a skill:

- [ ] SKILL.md frontmatter has `name` (max 64 chars, lowercase + hyphens) and `description` (max 1024 chars)
- [ ] Frontmatter is wrapped in `---` delimiters on separate lines
- [ ] All file references use relative paths from skill root (e.g., `scripts/extract.py`)
- [ ] Main SKILL.md is under 500 lines; detailed material moved to `references/` or `assets/`
- [ ] Skill passes validation: `skills-ref validate ./skill-name`
- [ ] Instructions are clear and actionable (imperative voice, step-by-step)
- [ ] Optional fields (license, compatibility, metadata) are accurate if included
- [ ] Skill description clearly states when an agent should use it (not just what it does)

## Resources

- **Comprehensive navigation**: https://agentskills.io/llms.txt
- **Full specification**: https://agentskills.io/specification
- **Integration guide**: https://agentskills.io/integrate-skills
- **Example skills**: GitHub repository (linked from agentskills.io)

---

> For additional documentation and navigation, see: https://agentskills.io/llms.txt