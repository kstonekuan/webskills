use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, ExitStatus};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;

const DEFAULT_OUTPUT_DIRECTORY_PATH: &str = ".webskills/generated";
const DEFAULT_TIMEOUT_MILLISECONDS: u64 = 15_000;

#[derive(Debug, Parser)]
#[command(name = "webskills")]
#[command(about = "Generate and install agent skills from websites.")]
#[command(
    long_about = "Generate and install agent skills from websites.\n\nThe `add` command first tries a direct install via `skills add <url>`. If that fails, it falls back to deterministic extraction and installs the generated skill directory."
)]
struct CommandLineArguments {
    #[command(subcommand)]
    subcommand: WebSkillsSubcommand,
}

#[derive(Debug, Subcommand)]
enum WebSkillsSubcommand {
    #[command(
        about = "Install from a URL, with extraction fallback.",
        long_about = "Install from a URL, with extraction fallback.\n\nExecution order:\n1. Try direct install via `skills add <url>`.\n2. If direct install fails, extract from the URL.\n3. Install the generated skill directory."
    )]
    Add(AddSubcommandArguments),
    #[command(
        about = "Run extraction only and write generated artifacts.",
        long_about = "Run extraction only and write generated artifacts.\n\nThis command never calls `skills add`; it only writes generated `SKILL.md` output."
    )]
    Extract(ExtractSubcommandArguments),
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  webskills add https://example.com/docs\n  webskills add https://example.com/docs --yes --global\n  webskills add https://example.com/docs --agent claude-code cursor --skill find-skills\n  webskills extract --url https://example.com/docs --output .webskills/generated --name docs-skill"
)]
struct AddSubcommandArguments {
    #[arg(value_name = "URL", help = "Target URL to install as a skill.")]
    url: String,
    #[arg(
        long,
        default_value = DEFAULT_OUTPUT_DIRECTORY_PATH,
        help = "Output directory used only when extraction runs."
    )]
    output: PathBuf,
    #[arg(
        long,
        help = "Optional skill name override used only when extraction runs."
    )]
    name: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_TIMEOUT_MILLISECONDS,
        help = "HTTP timeout in milliseconds used only when extraction runs."
    )]
    timeout_ms: u64,
    #[arg(
        short = 'y',
        long,
        default_value_t = false,
        help = "Forward `--yes` to `skills add` and `npx` to skip confirmation prompts."
    )]
    yes: bool,
    #[arg(
        short = 'g',
        long,
        default_value_t = false,
        help = "Forward `--global` to `skills add`."
    )]
    global: bool,
    #[arg(
        short = 'a',
        long = "agent",
        num_args = 1..,
        help = "Forward `--agent` values to `skills add`."
    )]
    agent: Vec<String>,
    #[arg(
        short = 's',
        long = "skill",
        num_args = 1..,
        help = "Forward `--skill` values to `skills add`."
    )]
    skill: Vec<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  webskills extract --url https://example.com/docs --output .webskills/generated\n  webskills extract --url https://example.com/docs --output .webskills/generated --name docs-skill --timeout-ms 30000"
)]
struct ExtractSubcommandArguments {
    #[arg(
        long,
        value_name = "URL",
        help = "Target URL to extract into deterministic skill artifacts."
    )]
    url: String,
    #[arg(long, help = "Directory where extracted artifacts will be written.")]
    output: PathBuf,
    #[arg(long, help = "Optional skill name override for generated output.")]
    name: Option<String>,
    #[arg(
        long,
        default_value_t = DEFAULT_TIMEOUT_MILLISECONDS,
        help = "HTTP timeout in milliseconds used during extraction."
    )]
    timeout_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum AddSubcommandResponse {
    DirectInstall,
    ExtractionInstall {
        #[serde(rename = "skillDirectoryPath")]
        skill_directory_path: String,
        #[serde(rename = "pipelineStageUsed")]
        pipeline_stage_used: webskills_core::PipelineStageUsed,
        #[serde(rename = "contentSha256")]
        content_sha256: String,
    },
}

#[derive(Debug, Clone)]
struct SkillsAddOptions {
    skip_confirmation_prompts: bool,
    install_globally: bool,
    agent_filter_values: Vec<String>,
    skill_filter_values: Vec<String>,
}

#[derive(Debug)]
enum SkillsAddCommandOutcome {
    Success,
    Failed(ExitStatus),
}

fn main() {
    if let Err(execution_error) = run_cli_entrypoint() {
        eprintln!("webskills error: {execution_error:#}");
        std::process::exit(1);
    }
}

fn run_cli_entrypoint() -> Result<()> {
    let command_line_arguments = CommandLineArguments::parse();

    match command_line_arguments.subcommand {
        WebSkillsSubcommand::Add(add_subcommand_arguments) => {
            run_add_subcommand(add_subcommand_arguments)?;
        }
        WebSkillsSubcommand::Extract(extract_subcommand_arguments) => {
            let extraction_response = run_extraction_subcommand(
                extract_subcommand_arguments.url,
                extract_subcommand_arguments.output,
                extract_subcommand_arguments.name,
                extract_subcommand_arguments.timeout_ms,
            )?;
            println!("{}", serde_json::to_string(&extraction_response)?);
        }
    }

    Ok(())
}

fn run_add_subcommand(add_subcommand_arguments: AddSubcommandArguments) -> Result<()> {
    let AddSubcommandArguments {
        url: target_url,
        output: output_directory_path,
        name: optional_skill_name,
        timeout_ms: timeout_milliseconds,
        yes,
        global,
        agent: agent_filter_values,
        skill: skill_filter_values,
    } = add_subcommand_arguments;

    let skills_add_options = SkillsAddOptions {
        skip_confirmation_prompts: yes,
        install_globally: global,
        agent_filter_values,
        skill_filter_values,
    };

    ensure_interactive_installation_is_supported(&skills_add_options)?;

    println!("Attempting direct skills install from URL: {target_url}");
    match run_skills_add_command(&target_url, &skills_add_options)? {
        SkillsAddCommandOutcome::Success => {
            let direct_install_response = AddSubcommandResponse::DirectInstall;
            println!("{}", serde_json::to_string(&direct_install_response)?);
            return Ok(());
        }
        SkillsAddCommandOutcome::Failed(direct_install_exit_status) => {
            println!(
                "Direct skills install failed ({}). Falling back to extraction pipeline.",
                format_process_exit_status(direct_install_exit_status)
            );
        }
    }

    let extraction_response = run_extraction_subcommand(
        target_url,
        output_directory_path,
        optional_skill_name,
        timeout_milliseconds,
    )?;
    print_extraction_summary(&extraction_response)?;

    match run_skills_add_command(
        &extraction_response.skill_directory_path,
        &skills_add_options,
    )? {
        SkillsAddCommandOutcome::Success => {}
        SkillsAddCommandOutcome::Failed(generated_install_exit_status) => {
            return Err(anyhow!(
                "Skills installation for generated skill failed ({})",
                format_process_exit_status(generated_install_exit_status)
            ));
        }
    }

    let extraction_install_response = AddSubcommandResponse::ExtractionInstall {
        skill_directory_path: extraction_response.skill_directory_path,
        pipeline_stage_used: extraction_response.pipeline_stage_used,
        content_sha256: extraction_response.content_sha256,
    };
    println!("{}", serde_json::to_string(&extraction_install_response)?);
    Ok(())
}

fn run_extraction_subcommand(
    target_url: String,
    output_directory_path: PathBuf,
    optional_skill_name: Option<String>,
    timeout_milliseconds: u64,
) -> Result<webskills_core::ExtractionResponse> {
    let extraction_request = webskills_core::ExtractionRequest {
        target_url,
        output_directory_path,
        optional_skill_name,
        timeout_milliseconds,
    };

    webskills_core::execute_extraction_pipeline(extraction_request)
}

fn print_extraction_summary(
    extraction_response: &webskills_core::ExtractionResponse,
) -> Result<()> {
    let normalized_pipeline_stage_used_value =
        serde_json::to_string(&extraction_response.pipeline_stage_used)?.replace('"', "");
    println!(
        "Generated skill directory: {}",
        extraction_response.skill_directory_path
    );
    println!("Pipeline stage used: {normalized_pipeline_stage_used_value}");
    println!("Content SHA-256: {}", extraction_response.content_sha256);
    Ok(())
}

fn ensure_interactive_installation_is_supported(
    skills_add_options: &SkillsAddOptions,
) -> Result<()> {
    validate_interactive_installation_support(
        skills_add_options.skip_confirmation_prompts,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
}

fn validate_interactive_installation_support(
    skip_confirmation_prompts: bool,
    has_stdin_terminal: bool,
    has_stdout_terminal: bool,
) -> Result<()> {
    if skip_confirmation_prompts || (has_stdin_terminal && has_stdout_terminal) {
        return Ok(());
    }

    Err(anyhow!(
        "Interactive skills installation requires a TTY for stdin/stdout. Re-run with --yes for non-interactive mode."
    ))
}

fn run_skills_add_command(
    target_skill_source: &str,
    skills_add_options: &SkillsAddOptions,
) -> Result<SkillsAddCommandOutcome> {
    let command_argument_values =
        build_skills_add_command_argument_values(target_skill_source, skills_add_options);
    println!("Running: {}", command_argument_values.join(" "));

    let mut skills_add_command = Command::new(&command_argument_values[0]);
    skills_add_command.args(&command_argument_values[1..]);

    let install_command_exit_status = skills_add_command
        .status()
        .context("Failed to execute skills installation command through npx")?;

    if install_command_exit_status.success() {
        return Ok(SkillsAddCommandOutcome::Success);
    }

    Ok(SkillsAddCommandOutcome::Failed(install_command_exit_status))
}

fn build_skills_add_command_argument_values(
    target_skill_source: &str,
    skills_add_options: &SkillsAddOptions,
) -> Vec<String> {
    let normalized_target_skill_source = normalize_skill_source_for_skills_add(target_skill_source);
    let mut command_argument_values = vec![
        "npx".to_string(),
        "skills".to_string(),
        "add".to_string(),
        normalized_target_skill_source,
    ];

    if skills_add_options.skip_confirmation_prompts {
        command_argument_values.insert(1, "--yes".to_string());
    }

    if skills_add_options.install_globally {
        command_argument_values.push("--global".to_string());
    }

    if !skills_add_options.agent_filter_values.is_empty() {
        command_argument_values.push("--agent".to_string());
        command_argument_values.extend(skills_add_options.agent_filter_values.clone());
    }

    if !skills_add_options.skill_filter_values.is_empty() {
        command_argument_values.push("--skill".to_string());
        command_argument_values.extend(skills_add_options.skill_filter_values.clone());
    }

    if skills_add_options.skip_confirmation_prompts {
        command_argument_values.push("--yes".to_string());
    }

    command_argument_values
}

fn normalize_skill_source_for_skills_add(target_skill_source: &str) -> String {
    let target_skill_source_path = PathBuf::from(target_skill_source);
    if !target_skill_source_path.exists() {
        return target_skill_source.to_string();
    }

    if let Ok(canonical_target_skill_source_path) = target_skill_source_path.canonicalize() {
        return canonical_target_skill_source_path
            .to_string_lossy()
            .to_string();
    }

    if target_skill_source_path.is_relative()
        && !target_skill_source.starts_with("./")
        && !target_skill_source.starts_with("../")
    {
        return format!("./{target_skill_source}");
    }

    target_skill_source.to_string()
}

fn format_process_exit_status(process_exit_status: ExitStatus) -> String {
    if let Some(exit_code_value) = process_exit_status.code() {
        return format!("exit code {exit_code_value}");
    }

    "terminated by signal".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::fs;

    #[test]
    fn builds_skills_add_command_arguments_preserve_user_visible_flags() {
        let skills_add_options = SkillsAddOptions {
            skip_confirmation_prompts: true,
            install_globally: true,
            agent_filter_values: vec!["claude-code".to_string(), "cursor".to_string()],
            skill_filter_values: vec!["find-skills".to_string()],
        };

        let command_argument_values = build_skills_add_command_argument_values(
            "https://example.com/docs",
            &skills_add_options,
        );

        assert_eq!(command_argument_values.first(), Some(&"npx".to_string()));
        assert_eq!(command_argument_values.last(), Some(&"--yes".to_string()));
        assert_eq!(
            command_argument_values
                .iter()
                .filter(|argument_value| *argument_value == "--yes")
                .count(),
            2
        );
        assert!(command_argument_values.contains(&"https://example.com/docs".to_string()));
        assert!(command_argument_values.contains(&"--global".to_string()));
        assert!(command_argument_values.contains(&"--agent".to_string()));
        assert!(command_argument_values.contains(&"claude-code".to_string()));
        assert!(command_argument_values.contains(&"cursor".to_string()));
        assert!(command_argument_values.contains(&"--skill".to_string()));
        assert!(command_argument_values.contains(&"find-skills".to_string()));
    }

    #[test]
    fn builds_skills_add_command_arguments_with_absolute_path_for_existing_local_source() {
        let current_working_directory_path =
            std::env::current_dir().expect("expected current directory");
        let relative_local_skill_directory_path =
            format!(".webskills/test-skill-path-{}", std::process::id());
        let local_skill_directory_path =
            current_working_directory_path.join(&relative_local_skill_directory_path);
        fs::create_dir_all(&local_skill_directory_path)
            .expect("expected local skill directory to be created");
        let canonical_local_skill_directory_path = local_skill_directory_path
            .canonicalize()
            .expect("expected local skill directory to canonicalize");

        let skills_add_options = SkillsAddOptions {
            skip_confirmation_prompts: false,
            install_globally: false,
            agent_filter_values: Vec::new(),
            skill_filter_values: Vec::new(),
        };
        let command_argument_values = build_skills_add_command_argument_values(
            &relative_local_skill_directory_path,
            &skills_add_options,
        );

        assert_eq!(command_argument_values[0], "npx");
        assert_eq!(command_argument_values[1], "skills");
        assert_eq!(command_argument_values[2], "add");
        assert_eq!(
            command_argument_values[3],
            canonical_local_skill_directory_path
                .to_string_lossy()
                .to_string()
        );

        fs::remove_dir_all(&local_skill_directory_path)
            .expect("expected temporary local skill directory to be removed");
    }

    #[test]
    fn normalize_skill_source_for_skills_add_keeps_nonexistent_relative_source_unchanged() {
        let normalized_skill_source = normalize_skill_source_for_skills_add(
            ".webskills/nonexistent-generated-skill-directory",
        );
        assert_eq!(
            normalized_skill_source,
            ".webskills/nonexistent-generated-skill-directory"
        );
    }

    #[test]
    fn interactive_mode_validation_rejects_non_tty_without_yes() {
        let validation_result = validate_interactive_installation_support(false, true, false);
        assert!(validation_result.is_err());
    }

    #[test]
    fn interactive_mode_validation_allows_non_tty_with_yes() {
        let validation_result = validate_interactive_installation_support(true, false, false);
        assert!(validation_result.is_ok());
    }

    #[test]
    fn add_command_parsing_accepts_parity_and_webskills_flags() {
        let parsed_command_line_arguments = CommandLineArguments::try_parse_from([
            "webskills",
            "add",
            "https://example.com/docs",
            "--output",
            ".webskills/custom",
            "--name",
            "example-skill",
            "--timeout-ms",
            "2000",
            "--yes",
            "--global",
            "--agent",
            "claude-code",
            "cursor",
            "--skill",
            "find-skills",
        ])
        .expect("expected add command parsing to succeed");

        let WebSkillsSubcommand::Add(add_subcommand_arguments) =
            parsed_command_line_arguments.subcommand
        else {
            panic!("expected add subcommand");
        };

        assert!(add_subcommand_arguments.yes);
        assert!(add_subcommand_arguments.global);
        assert_eq!(
            add_subcommand_arguments.output,
            PathBuf::from(".webskills/custom")
        );
        assert_eq!(
            add_subcommand_arguments.name,
            Some("example-skill".to_string())
        );
        assert_eq!(add_subcommand_arguments.timeout_ms, 2000);
        assert_eq!(
            add_subcommand_arguments.agent,
            vec!["claude-code".to_string(), "cursor".to_string()]
        );
        assert_eq!(
            add_subcommand_arguments.skill,
            vec!["find-skills".to_string()]
        );
    }
}
