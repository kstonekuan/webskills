use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use dom_smoothie::{CandidateSelectMode, Config as DomSmoothieConfig, Readability, TextMode};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::ACCEPT;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

const MAX_FETCHED_DOCUMENT_CHAR_COUNT: usize = 2_000_000;
const MIN_PRIMARY_CONTENT_TEXT_CHARACTER_COUNT: usize = 200;
const REFERENCE_DIRECTORY_NAME: &str = "references";
const PRIMARY_REFERENCE_FILE_NAME: &str = "REFERENCE.md";
const EXPLICIT_MARKDOWN_CANDIDATE_FILE_NAMES: [&str; 4] =
    ["llms.txt", "llm.txt", "docs.md", "README.md"];
const MINIMUM_DESCRIPTION_CANDIDATE_CHARACTER_COUNT: usize = 20;

#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    pub target_url: String,
    pub output_directory_path: PathBuf,
    pub optional_skill_name: Option<String>,
    pub timeout_milliseconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResponse {
    pub skill_directory_path: String,
    pub pipeline_stage_used: PipelineStageUsed,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStageUsed {
    ExplicitMarkdown,
    MarkdownNegotiation,
    HtmlFallback,
}

#[derive(Debug, Clone)]
struct FetchedDocument {
    original_requested_url: Url,
    final_response_url: Url,
    content_type_header: Option<String>,
    body_text: String,
}

#[derive(Debug, Clone)]
struct SelectedSourceDocument {
    original_source_url: Url,
    final_source_url: Url,
    pipeline_stage_used: PipelineStageUsed,
    primary_markdown_content: String,
}

#[derive(Debug, Clone)]
struct StructuredMarkdownExtraction {
    derived_description: String,
    explicit_instruction_lines: Vec<String>,
    discovered_command_lines: Vec<String>,
    discovered_api_usage_lines: Vec<String>,
    source_excerpt_lines: Vec<String>,
}

#[derive(Debug)]
struct SkillMarkdownBuildInput<'a> {
    skill_slug: &'a str,
    title_value: &'a str,
    description_value: &'a str,
    explicit_instruction_lines: &'a [String],
    discovered_command_lines: &'a [String],
    discovered_api_usage_lines: &'a [String],
    discovered_links: &'a [String],
    original_source_url: &'a str,
    final_source_url: &'a str,
    content_sha256: &'a str,
    pipeline_stage_used: PipelineStageUsed,
    source_excerpt_lines: &'a [String],
}

/// Runs the deterministic extraction pipeline and writes generated artifacts.
///
/// # Errors
///
/// Returns an error if URL parsing fails, remote content cannot be fetched or converted,
/// or output artifacts cannot be written.
pub fn execute_extraction_pipeline(
    extraction_request: ExtractionRequest,
) -> Result<ExtractionResponse> {
    let parsed_target_url = Url::parse(&extraction_request.target_url).with_context(|| {
        format!(
            "Failed to parse target URL: {}",
            extraction_request.target_url
        )
    })?;

    if parsed_target_url.scheme() != "http" && parsed_target_url.scheme() != "https" {
        bail!("Target URL must use http or https.");
    }

    let http_client = build_http_client(extraction_request.timeout_milliseconds)?;
    let discovered_links =
        collect_discovered_links_from_input_page(&http_client, &parsed_target_url)
            .context("Failed to collect discovered links from input page")?;

    let selected_source_document =
        select_source_document_via_pipeline(&http_client, &parsed_target_url)
            .context("Failed to resolve extraction source document")?;

    let normalized_primary_markdown_content = normalize_markdown_for_deterministic_storage(
        &selected_source_document.primary_markdown_content,
    );
    let content_sha256 = compute_sha256_hex_digest(&normalized_primary_markdown_content);

    let stable_skill_slug = derive_stable_skill_slug(
        extraction_request.optional_skill_name.as_deref(),
        &parsed_target_url,
    );
    let versioned_skill_directory_name =
        derive_versioned_skill_directory_name(&stable_skill_slug, &content_sha256);
    let skill_directory_path = extraction_request
        .output_directory_path
        .join(versioned_skill_directory_name);

    let structured_markdown_extraction = extract_structured_sections_from_markdown(
        &normalized_primary_markdown_content,
        parsed_target_url.host_str().unwrap_or("unknown-source"),
    );

    let generated_skill_markdown_document =
        build_skill_markdown_document(SkillMarkdownBuildInput {
            skill_slug: &stable_skill_slug,
            title_value: &format!(
                "{} Web Skill",
                parsed_target_url.host_str().unwrap_or("Web")
            ),
            description_value: &build_skill_trigger_description(
                &structured_markdown_extraction.derived_description,
                parsed_target_url.host_str().unwrap_or("unknown-source"),
            ),
            explicit_instruction_lines: &structured_markdown_extraction.explicit_instruction_lines,
            discovered_command_lines: &structured_markdown_extraction.discovered_command_lines,
            discovered_api_usage_lines: &structured_markdown_extraction.discovered_api_usage_lines,
            discovered_links: &discovered_links,
            original_source_url: selected_source_document.original_source_url.as_str(),
            final_source_url: selected_source_document.final_source_url.as_str(),
            content_sha256: &content_sha256,
            pipeline_stage_used: selected_source_document.pipeline_stage_used,
            source_excerpt_lines: &structured_markdown_extraction.source_excerpt_lines,
        });

    write_generated_skill_artifacts(
        &skill_directory_path,
        &generated_skill_markdown_document,
        &normalized_primary_markdown_content,
    )?;

    Ok(ExtractionResponse {
        skill_directory_path: skill_directory_path.to_string_lossy().to_string(),
        pipeline_stage_used: selected_source_document.pipeline_stage_used,
        content_sha256,
    })
}

fn build_http_client(timeout_milliseconds: u64) -> Result<Client> {
    let timeout_duration = Duration::from_millis(timeout_milliseconds.max(1));
    Client::builder()
        .user_agent(format!("webskills/{}", env!("CARGO_PKG_VERSION")))
        .timeout(timeout_duration)
        .build()
        .context("Failed to build HTTP client")
}

fn select_source_document_via_pipeline(
    http_client: &Client,
    parsed_target_url: &Url,
) -> Result<SelectedSourceDocument> {
    if let Some(explicit_markdown_source_document) =
        probe_explicit_markdown_surfaces(http_client, parsed_target_url)?
    {
        return Ok(explicit_markdown_source_document);
    }

    if let Some(markdown_negotiation_source_document) =
        attempt_markdown_negotiation(http_client, parsed_target_url)?
    {
        return Ok(markdown_negotiation_source_document);
    }

    if let Some(html_fallback_source_document) =
        attempt_html_fallback(http_client, parsed_target_url)?
    {
        return Ok(html_fallback_source_document);
    }

    bail!("Unable to fetch any usable source document for extraction pipeline.")
}
fn probe_explicit_markdown_surfaces(
    http_client: &Client,
    parsed_target_url: &Url,
) -> Result<Option<SelectedSourceDocument>> {
    let explicit_markdown_candidate_urls =
        build_explicit_markdown_candidate_urls(parsed_target_url)?;
    probe_candidate_urls(
        http_client,
        &explicit_markdown_candidate_urls,
        PipelineStageUsed::ExplicitMarkdown,
    )
}

fn build_explicit_markdown_candidate_urls(parsed_target_url: &Url) -> Result<Vec<Url>> {
    let requested_path = parsed_target_url.path();
    let target_directory_path =
        derive_target_directory_path_for_explicit_markdown_probe(parsed_target_url);

    let mut explicit_markdown_candidate_urls: Vec<Url> = Vec::new();

    if requested_path != "/" && target_directory_path != "/" {
        for candidate_file_name in EXPLICIT_MARKDOWN_CANDIDATE_FILE_NAMES {
            let target_directory_candidate_path =
                format!("{target_directory_path}{candidate_file_name}");
            let target_directory_candidate_url =
                build_same_origin_url(parsed_target_url, &target_directory_candidate_path)?;
            push_unique_candidate_url(
                &mut explicit_markdown_candidate_urls,
                target_directory_candidate_url,
            );
        }
    }

    if requested_path == "/" {
        let origin_url = build_origin_url(parsed_target_url)?;
        for candidate_file_name in EXPLICIT_MARKDOWN_CANDIDATE_FILE_NAMES {
            let origin_candidate_path = format!("/{candidate_file_name}");
            let origin_candidate_url = build_same_origin_url(&origin_url, &origin_candidate_path)?;
            push_unique_candidate_url(&mut explicit_markdown_candidate_urls, origin_candidate_url);
        }
    }

    Ok(explicit_markdown_candidate_urls)
}

fn derive_target_directory_path_for_explicit_markdown_probe(parsed_target_url: &Url) -> String {
    let requested_path = parsed_target_url.path();
    if requested_path == "/" {
        return "/".to_string();
    }

    if requested_path.ends_with('/') {
        return requested_path.to_string();
    }

    let Some((directory_path_prefix, _)) = requested_path.rsplit_once('/') else {
        return "/".to_string();
    };

    if directory_path_prefix.is_empty() {
        "/".to_string()
    } else {
        format!("{directory_path_prefix}/")
    }
}

fn push_unique_candidate_url(candidate_urls: &mut Vec<Url>, candidate_url: Url) {
    if candidate_urls
        .iter()
        .any(|existing_candidate_url| existing_candidate_url == &candidate_url)
    {
        return;
    }

    candidate_urls.push(candidate_url);
}

fn probe_candidate_urls(
    http_client: &Client,
    candidate_urls: &[Url],
    pipeline_stage_used: PipelineStageUsed,
) -> Result<Option<SelectedSourceDocument>> {
    for candidate_url in candidate_urls {
        let Ok(optional_fetched_document) = fetch_document_if_successful(
            http_client,
            candidate_url,
            "text/plain, text/html, */*;q=0.1",
        ) else {
            continue;
        };

        if let Some(fetched_document) = optional_fetched_document {
            if matches!(pipeline_stage_used, PipelineStageUsed::ExplicitMarkdown)
                && (is_html_content_type(
                    fetched_document
                        .content_type_header
                        .as_deref()
                        .unwrap_or_default(),
                ) || looks_like_html_document(&fetched_document.body_text))
            {
                continue;
            }

            if let Some(primary_markdown_content) =
                convert_fetched_document_to_markdown(&fetched_document)
            {
                return Ok(Some(SelectedSourceDocument {
                    original_source_url: fetched_document.original_requested_url,
                    final_source_url: fetched_document.final_response_url,
                    pipeline_stage_used,
                    primary_markdown_content,
                }));
            }
        }
    }

    Ok(None)
}

fn attempt_markdown_negotiation(
    http_client: &Client,
    parsed_target_url: &Url,
) -> Result<Option<SelectedSourceDocument>> {
    let Ok(optional_fetched_document) =
        fetch_document_if_successful(http_client, parsed_target_url, "text/markdown")
    else {
        return Ok(None);
    };

    let Some(fetched_document) = optional_fetched_document else {
        return Ok(None);
    };

    let content_type_header = fetched_document
        .content_type_header
        .as_deref()
        .unwrap_or_default();

    if is_html_content_type(content_type_header)
        || looks_like_html_document(&fetched_document.body_text)
    {
        return Ok(None);
    }

    Ok(Some(SelectedSourceDocument {
        original_source_url: fetched_document.original_requested_url,
        final_source_url: fetched_document.final_response_url,
        pipeline_stage_used: PipelineStageUsed::MarkdownNegotiation,
        primary_markdown_content: normalize_markdown_for_deterministic_storage(
            &fetched_document.body_text,
        ),
    }))
}

fn attempt_html_fallback(
    http_client: &Client,
    parsed_target_url: &Url,
) -> Result<Option<SelectedSourceDocument>> {
    let Ok(optional_fetched_document) = fetch_document_if_successful(
        http_client,
        parsed_target_url,
        "text/html, application/xhtml+xml, text/plain",
    ) else {
        return Ok(None);
    };

    let Some(fetched_document) = optional_fetched_document else {
        return Ok(None);
    };

    let primary_markdown_content = if is_html_content_type(
        fetched_document
            .content_type_header
            .as_deref()
            .unwrap_or_default(),
    ) || looks_like_html_document(&fetched_document.body_text)
    {
        convert_html_document_to_markdown_prioritizing_primary_content(
            &fetched_document.body_text,
            Some(fetched_document.final_response_url.as_str()),
        )
    } else {
        normalize_markdown_for_deterministic_storage(&fetched_document.body_text)
    };

    Ok(Some(SelectedSourceDocument {
        original_source_url: fetched_document.original_requested_url,
        final_source_url: fetched_document.final_response_url,
        pipeline_stage_used: PipelineStageUsed::HtmlFallback,
        primary_markdown_content,
    }))
}

fn fetch_document_if_successful(
    http_client: &Client,
    requested_url: &Url,
    accept_header_value: &str,
) -> Result<Option<FetchedDocument>> {
    let response = http_client
        .get(requested_url.as_str())
        .header(ACCEPT, accept_header_value)
        .send()
        .with_context(|| format!("Failed to fetch {requested_url}"))?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let final_response_url = sanitize_final_response_url_for_persistence(response.url());
    let optional_content_type_header = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|header_value| header_value.to_str().ok())
        .map(ToOwned::to_owned);

    let body_text = response.text().context("Failed to read response body")?;

    if body_text.trim().is_empty() {
        return Ok(None);
    }

    if body_text.chars().count() > MAX_FETCHED_DOCUMENT_CHAR_COUNT {
        return Ok(None);
    }

    Ok(Some(FetchedDocument {
        original_requested_url: requested_url.clone(),
        final_response_url,
        content_type_header: optional_content_type_header,
        body_text,
    }))
}

fn sanitize_final_response_url_for_persistence(url_to_sanitize: &Url) -> Url {
    let mut sanitized_response_url = url_to_sanitize.clone();
    sanitized_response_url.set_query(None);
    sanitized_response_url.set_fragment(None);
    sanitized_response_url
}

fn convert_fetched_document_to_markdown(fetched_document: &FetchedDocument) -> Option<String> {
    let content_type_header = fetched_document
        .content_type_header
        .as_deref()
        .unwrap_or_default();

    if is_markdown_content_type(content_type_header) || is_probably_plain_text(content_type_header)
    {
        if looks_like_html_document(&fetched_document.body_text) {
            return Some(
                convert_html_document_to_markdown_prioritizing_primary_content(
                    &fetched_document.body_text,
                    Some(fetched_document.final_response_url.as_str()),
                ),
            );
        }

        return Some(normalize_markdown_for_deterministic_storage(
            &fetched_document.body_text,
        ));
    }

    if is_html_content_type(content_type_header)
        || looks_like_html_document(&fetched_document.body_text)
    {
        return Some(
            convert_html_document_to_markdown_prioritizing_primary_content(
                &fetched_document.body_text,
                Some(fetched_document.final_response_url.as_str()),
            ),
        );
    }

    None
}

fn collect_discovered_links_from_input_page(
    http_client: &Client,
    parsed_target_url: &Url,
) -> Result<Vec<String>> {
    let Ok(optional_fetched_input_document) = fetch_document_if_successful(
        http_client,
        parsed_target_url,
        "text/html, application/xhtml+xml, text/plain",
    ) else {
        return Ok(Vec::new());
    };

    let Some(fetched_input_document) = optional_fetched_input_document else {
        return Ok(Vec::new());
    };

    let mut normalized_discovered_link_values: BTreeSet<String> = BTreeSet::new();
    if is_html_content_type(
        fetched_input_document
            .content_type_header
            .as_deref()
            .unwrap_or_default(),
    ) || looks_like_html_document(&fetched_input_document.body_text)
    {
        for normalized_link_value in extract_discovered_links_from_html_document(
            &fetched_input_document.body_text,
            parsed_target_url,
        ) {
            normalized_discovered_link_values.insert(normalized_link_value);
        }
    } else {
        for normalized_link_value in extract_discovered_links_from_markdown_document(
            &fetched_input_document.body_text,
            parsed_target_url,
        ) {
            normalized_discovered_link_values.insert(normalized_link_value);
        }
    }

    Ok(normalized_discovered_link_values.into_iter().collect())
}

fn extract_discovered_links_from_html_document(
    html_document_text: &str,
    parsed_target_url: &Url,
) -> Vec<String> {
    let parsed_html_document = Html::parse_document(html_document_text);
    let Ok(anchor_selector) = Selector::parse("a[href]") else {
        return Vec::new();
    };

    let mut normalized_discovered_link_values: BTreeSet<String> = BTreeSet::new();
    for anchor_element in parsed_html_document.select(&anchor_selector) {
        if let Some(raw_href_value) = anchor_element.value().attr("href") {
            if let Some(normalized_discovered_link_value) =
                normalize_discovered_link(raw_href_value, parsed_target_url)
            {
                normalized_discovered_link_values.insert(normalized_discovered_link_value);
            }
        }
    }

    normalized_discovered_link_values.into_iter().collect()
}

fn extract_discovered_links_from_markdown_document(
    markdown_document_text: &str,
    parsed_target_url: &Url,
) -> Vec<String> {
    let markdown_link_pattern =
        Regex::new(r"\[[^\]]+\]\(([^)]+)\)").expect("Markdown link regex must compile");

    let mut normalized_discovered_link_values: BTreeSet<String> = BTreeSet::new();
    for capture_values in markdown_link_pattern.captures_iter(markdown_document_text) {
        if let Some(raw_link_capture_value) = capture_values.get(1) {
            if let Some(normalized_discovered_link_value) =
                normalize_discovered_link(raw_link_capture_value.as_str(), parsed_target_url)
            {
                normalized_discovered_link_values.insert(normalized_discovered_link_value);
            }
        }
    }

    normalized_discovered_link_values.into_iter().collect()
}

fn normalize_discovered_link(raw_link_value: &str, parsed_target_url: &Url) -> Option<String> {
    let trimmed_raw_link_value = raw_link_value.trim();

    if trimmed_raw_link_value.is_empty()
        || trimmed_raw_link_value.starts_with('#')
        || trimmed_raw_link_value.starts_with("mailto:")
        || trimmed_raw_link_value.starts_with("tel:")
        || trimmed_raw_link_value.starts_with("javascript:")
    {
        return None;
    }

    let mut joined_link_url = parsed_target_url.join(trimmed_raw_link_value).ok()?;
    if joined_link_url.scheme() != "http" && joined_link_url.scheme() != "https" {
        return None;
    }

    joined_link_url.set_fragment(None);
    Some(joined_link_url.to_string())
}

fn extract_structured_sections_from_markdown(
    normalized_primary_markdown_content: &str,
    source_hostname: &str,
) -> StructuredMarkdownExtraction {
    let explicit_instruction_lines =
        extract_explicit_instruction_lines(normalized_primary_markdown_content);
    let discovered_command_lines =
        extract_discovered_command_lines(normalized_primary_markdown_content);
    let discovered_api_usage_lines =
        extract_discovered_api_usage_lines(normalized_primary_markdown_content);
    let source_excerpt_lines = extract_source_excerpt_lines(normalized_primary_markdown_content);
    let derived_description = derive_skill_description(&source_excerpt_lines, source_hostname);

    StructuredMarkdownExtraction {
        derived_description,
        explicit_instruction_lines,
        discovered_command_lines,
        discovered_api_usage_lines,
        source_excerpt_lines,
    }
}

fn extract_explicit_instruction_lines(normalized_primary_markdown_content: &str) -> Vec<String> {
    let mut explicit_instruction_lines: Vec<String> = Vec::new();
    let mut inside_fenced_code_block = false;

    for markdown_line in normalized_primary_markdown_content.lines() {
        let trimmed_markdown_line = markdown_line.trim();

        if trimmed_markdown_line.starts_with("```") {
            inside_fenced_code_block = !inside_fenced_code_block;
            continue;
        }

        if inside_fenced_code_block || trimmed_markdown_line.is_empty() {
            continue;
        }

        if is_heading_line(trimmed_markdown_line) {
            continue;
        }

        if is_explicit_instruction_like_line(trimmed_markdown_line) {
            explicit_instruction_lines.push(trimmed_markdown_line.to_string());
        }
    }

    if !explicit_instruction_lines.is_empty() {
        return explicit_instruction_lines;
    }

    extract_source_excerpt_lines(normalized_primary_markdown_content)
}

fn extract_discovered_command_lines(normalized_primary_markdown_content: &str) -> Vec<String> {
    let code_block_pattern = Regex::new(r"(?s)```(?:bash|sh|shell|zsh|console)?\s*\n(.*?)```")
        .expect("Code block regex must compile");

    let mut command_line_values: BTreeSet<String> = BTreeSet::new();
    for capture_values in code_block_pattern.captures_iter(normalized_primary_markdown_content) {
        if let Some(captured_code_block_content) = capture_values.get(1) {
            for code_block_line in captured_code_block_content.as_str().lines() {
                let mut normalized_command_candidate_line = code_block_line.trim().to_string();
                if normalized_command_candidate_line.starts_with('$') {
                    normalized_command_candidate_line = normalized_command_candidate_line
                        .trim_start_matches('$')
                        .trim()
                        .to_string();
                }

                if is_command_like_line(&normalized_command_candidate_line) {
                    command_line_values.insert(normalized_command_candidate_line);
                }
            }
        }
    }

    command_line_values.into_iter().collect()
}

fn extract_discovered_api_usage_lines(normalized_primary_markdown_content: &str) -> Vec<String> {
    let http_method_usage_pattern =
        Regex::new(r"\b(GET|POST|PUT|PATCH|DELETE)\s+(/[\w\-./{}:?=&]+)")
            .expect("HTTP method usage regex must compile");
    let absolute_url_usage_pattern =
        Regex::new(r#"https?://[^\s`"']+"#).expect("Absolute URL regex must compile");

    let mut discovered_api_usage_lines: BTreeSet<String> = BTreeSet::new();

    for capture_values in
        http_method_usage_pattern.captures_iter(normalized_primary_markdown_content)
    {
        if let (Some(http_method_capture), Some(http_path_capture)) =
            (capture_values.get(1), capture_values.get(2))
        {
            discovered_api_usage_lines.insert(format!(
                "{} {}",
                http_method_capture.as_str(),
                http_path_capture.as_str()
            ));
        }
    }

    for capture_values in
        absolute_url_usage_pattern.captures_iter(normalized_primary_markdown_content)
    {
        if let Some(absolute_url_capture) = capture_values.get(0) {
            if let Some(normalized_absolute_url_value) =
                normalize_extracted_absolute_url_for_listing(absolute_url_capture.as_str())
            {
                discovered_api_usage_lines.insert(normalized_absolute_url_value);
            }
        }
    }

    discovered_api_usage_lines.into_iter().collect()
}

fn normalize_extracted_absolute_url_for_listing(raw_absolute_url_value: &str) -> Option<String> {
    let mut normalized_absolute_url_value = raw_absolute_url_value.trim().to_string();
    if normalized_absolute_url_value.is_empty() {
        return None;
    }

    while matches!(
        normalized_absolute_url_value.chars().last(),
        Some('.' | ',' | ';' | ':' | '!' | '?')
    ) {
        normalized_absolute_url_value.pop();
    }

    while should_remove_trailing_closing_parenthesis(&normalized_absolute_url_value) {
        normalized_absolute_url_value.pop();
    }

    if normalized_absolute_url_value.is_empty() {
        return None;
    }

    Some(normalized_absolute_url_value)
}

fn should_remove_trailing_closing_parenthesis(candidate_absolute_url_value: &str) -> bool {
    if !candidate_absolute_url_value.ends_with(')') {
        return false;
    }

    let opening_parenthesis_count = candidate_absolute_url_value
        .chars()
        .filter(|character_value| *character_value == '(')
        .count();
    let closing_parenthesis_count = candidate_absolute_url_value
        .chars()
        .filter(|character_value| *character_value == ')')
        .count();

    closing_parenthesis_count > opening_parenthesis_count
}

fn extract_source_excerpt_lines(normalized_primary_markdown_content: &str) -> Vec<String> {
    let mut source_excerpt_lines: Vec<String> = Vec::new();

    for markdown_line in normalized_primary_markdown_content.lines() {
        let trimmed_markdown_line = markdown_line.trim();
        if trimmed_markdown_line.is_empty()
            || trimmed_markdown_line.starts_with("```")
            || is_heading_line(trimmed_markdown_line)
        {
            continue;
        }

        source_excerpt_lines.push(trimmed_markdown_line.to_string());
    }

    source_excerpt_lines
}

fn derive_skill_description(source_excerpt_lines: &[String], source_hostname: &str) -> String {
    let fallback_description =
        format!("Deterministic single-page skill generated from {source_hostname}.");

    for source_excerpt_line in source_excerpt_lines {
        let normalized_description_candidate_line =
            normalize_source_excerpt_line_for_description(source_excerpt_line);
        if !is_usable_description_candidate_line(&normalized_description_candidate_line) {
            continue;
        }

        let first_sentence_value =
            extract_first_sentence_value(&normalized_description_candidate_line).trim();
        if first_sentence_value.is_empty() {
            continue;
        }

        return truncate_to_maximum_character_count(first_sentence_value, 160);
    }

    fallback_description
}

fn extract_first_sentence_value(source_text_value: &str) -> &str {
    for (character_index, source_character) in source_text_value.char_indices() {
        if matches!(source_character, '.' | '!' | '?') {
            let sentence_end_index = character_index + source_character.len_utf8();
            return &source_text_value[..sentence_end_index];
        }
    }

    source_text_value
}

fn build_skill_trigger_description(page_description: &str, source_hostname: &str) -> String {
    let trimmed_page_description = page_description.trim();
    if trimmed_page_description.is_empty() {
        return format!(
            "Use when the user needs information or instructions from {source_hostname} extracted from a single public webpage."
        );
    }

    let normalized_page_description = trimmed_page_description
        .trim_end_matches(['.', '!', '?'])
        .trim();

    truncate_to_maximum_character_count(
        &format!(
            "Use when the user needs information or instructions from {source_hostname} related to: {normalized_page_description}."
        ),
        300,
    )
}

fn build_skill_markdown_document(build_input: SkillMarkdownBuildInput<'_>) -> String {
    let mut generated_markdown_document = String::new();
    append_markdown_frontmatter_and_intro(&mut generated_markdown_document, &build_input);
    append_markdown_references_section(&mut generated_markdown_document);
    append_markdown_instruction_section(&mut generated_markdown_document, &build_input);
    append_markdown_command_and_api_section(&mut generated_markdown_document, &build_input);
    append_markdown_discovered_pages_section(&mut generated_markdown_document, &build_input);
    append_markdown_source_section(&mut generated_markdown_document, &build_input);
    append_markdown_source_excerpt_section(&mut generated_markdown_document, &build_input);
    generated_markdown_document
}

fn append_markdown_frontmatter_and_intro(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    generated_markdown_document.push_str("---\n");
    writeln!(
        generated_markdown_document,
        "name: {}",
        build_input.skill_slug
    )
    .expect("Writing to string should not fail");
    writeln!(
        generated_markdown_document,
        "description: \"{}\"",
        escape_for_yaml_double_quoted_string(build_input.description_value)
    )
    .expect("Writing to string should not fail");
    generated_markdown_document.push_str("---\n\n");

    writeln!(generated_markdown_document, "# {}", build_input.title_value)
        .expect("Writing to string should not fail");
    generated_markdown_document.push('\n');
}

fn append_markdown_instruction_section(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    generated_markdown_document.push_str("## Instructions\n\n");
    let normalized_instruction_lines = build_input
        .explicit_instruction_lines
        .iter()
        .map(|explicit_instruction_line| {
            normalize_instruction_line_for_numbered_output(explicit_instruction_line)
        })
        .filter(|normalized_instruction_line| !normalized_instruction_line.is_empty())
        .collect::<Vec<String>>();

    if normalized_instruction_lines.is_empty() {
        generated_markdown_document.push_str(
            "No explicit actionable instructions were found in the extracted source content.\n\n",
        );
    } else {
        for (line_index, normalized_instruction_line) in
            normalized_instruction_lines.iter().enumerate()
        {
            writeln!(
                generated_markdown_document,
                "{}. {}",
                line_index + 1,
                normalized_instruction_line
            )
            .expect("Writing to string should not fail");
        }
        generated_markdown_document.push('\n');
    }
}

fn normalize_instruction_line_for_numbered_output(explicit_instruction_line: &str) -> String {
    let source_line_without_quote_marker =
        strip_leading_markdown_quote_marker(explicit_instruction_line);
    let source_line_without_list_marker =
        strip_leading_markdown_list_marker(source_line_without_quote_marker);
    collapse_whitespace_segments(&source_line_without_list_marker)
}

fn strip_leading_markdown_quote_marker(source_line_value: &str) -> &str {
    let trimmed_source_line_value = source_line_value.trim_start();
    if let Some(source_line_without_quote_marker) = trimmed_source_line_value.strip_prefix('>') {
        return source_line_without_quote_marker.trim_start();
    }

    trimmed_source_line_value
}

fn strip_leading_markdown_list_marker(source_line_value: &str) -> String {
    let trimmed_source_line_value = source_line_value.trim_start();
    let source_line_without_unordered_list_marker = trimmed_source_line_value
        .strip_prefix("- ")
        .or_else(|| trimmed_source_line_value.strip_prefix("* "))
        .unwrap_or(trimmed_source_line_value)
        .trim_start();

    let ordered_list_prefix_pattern =
        Regex::new(r"^\d+\.\s+").expect("Ordered list prefix regex must compile");
    ordered_list_prefix_pattern
        .replace(source_line_without_unordered_list_marker, "")
        .to_string()
}

fn normalize_source_excerpt_line_for_description(source_excerpt_line: &str) -> String {
    let source_line_without_quote_marker = strip_leading_markdown_quote_marker(source_excerpt_line);
    let source_line_without_list_marker =
        strip_leading_markdown_list_marker(source_line_without_quote_marker);
    let markdown_heading_prefix_pattern =
        Regex::new(r"^#{1,6}\s+").expect("Markdown heading regex must compile");
    let source_line_without_markdown_heading_prefix = markdown_heading_prefix_pattern
        .replace(&source_line_without_list_marker, "")
        .to_string();
    let markdown_frontmatter_key_prefix_pattern =
        Regex::new(r"^(title|description|summary|subtitle)\s*:\s+")
            .expect("Frontmatter key prefix regex must compile");
    let source_line_without_frontmatter_key_prefix = markdown_frontmatter_key_prefix_pattern
        .replace(&source_line_without_markdown_heading_prefix, "")
        .to_string();

    let markdown_link_pattern =
        Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("Markdown inline link regex must compile");
    let source_line_without_markdown_links =
        markdown_link_pattern.replace_all(&source_line_without_frontmatter_key_prefix, "$1");

    let source_line_without_inline_code_markers =
        source_line_without_markdown_links.replace('`', "");

    let markdown_escape_pattern =
        Regex::new(r"\\([\\`*_{}\[\]()#+\-.!])").expect("Markdown escape regex must compile");
    let source_line_without_markdown_escape_characters = markdown_escape_pattern
        .replace_all(&source_line_without_inline_code_markers, "$1")
        .to_string();

    collapse_whitespace_segments(&source_line_without_markdown_escape_characters)
}

fn is_usable_description_candidate_line(description_candidate_line: &str) -> bool {
    let trimmed_description_candidate_line = description_candidate_line.trim();
    if trimmed_description_candidate_line.chars().count()
        < MINIMUM_DESCRIPTION_CANDIDATE_CHARACTER_COUNT
    {
        return false;
    }

    if !trimmed_description_candidate_line
        .chars()
        .any(|character_value| character_value.is_ascii_alphabetic())
    {
        return false;
    }

    let lowercase_description_candidate_line =
        trimmed_description_candidate_line.to_ascii_lowercase();
    !lowercase_description_candidate_line.starts_with("added in version ")
        && !lowercase_description_candidate_line.starts_with("changed in version ")
        && !lowercase_description_candidate_line.starts_with("deprecated since version ")
        && lowercase_description_candidate_line != "documentation index"
        && lowercase_description_candidate_line != "table of contents"
        && lowercase_description_candidate_line != "contents"
}

fn append_markdown_references_section(generated_markdown_document: &mut String) {
    generated_markdown_document.push_str("## References\n\n");
    generated_markdown_document
        .push_str("Load the full extracted page content when you need deeper detail:\n\n");
    writeln!(
        generated_markdown_document,
        "- [{PRIMARY_REFERENCE_FILE_NAME}]({REFERENCE_DIRECTORY_NAME}/{PRIMARY_REFERENCE_FILE_NAME})"
    )
    .expect("Writing to string should not fail");
    generated_markdown_document.push('\n');
    generated_markdown_document.push_str(
        "Treat `REFERENCE.md` as the source of truth for extracted content. Treat `Discovered Pages` as navigation hints only; those pages were not extracted into this skill.\n\n",
    );
}

fn append_markdown_command_and_api_section(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    if !build_input.discovered_command_lines.is_empty() {
        generated_markdown_document.push_str("## Commands / API Usage\n\n");
        generated_markdown_document.push_str("```bash\n");
        for discovered_command_line in build_input.discovered_command_lines {
            generated_markdown_document.push_str(discovered_command_line);
            generated_markdown_document.push('\n');
        }
        generated_markdown_document.push_str("```\n\n");
    }

    if !build_input.discovered_api_usage_lines.is_empty() {
        generated_markdown_document.push_str("### API References\n\n");
        for discovered_api_usage_line in build_input.discovered_api_usage_lines {
            writeln!(generated_markdown_document, "- {discovered_api_usage_line}")
                .expect("Writing to string should not fail");
        }
        generated_markdown_document.push('\n');
    }
}

fn append_markdown_discovered_pages_section(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    generated_markdown_document.push_str("## Discovered Pages\n\n");
    if build_input.discovered_links.is_empty() {
        generated_markdown_document.push_str(
            "No links were discovered on the input page or they could not be normalized.\n\n",
        );
    } else {
        for discovered_link in build_input.discovered_links {
            writeln!(generated_markdown_document, "- {discovered_link}")
                .expect("Writing to string should not fail");
        }
        generated_markdown_document.push('\n');
    }
}

fn append_markdown_source_section(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    generated_markdown_document.push_str("## Source\n\n");
    writeln!(
        generated_markdown_document,
        "- Original Source URL: {}",
        build_input.original_source_url
    )
    .expect("Writing to string should not fail");
    writeln!(
        generated_markdown_document,
        "- Final Source URL: {}",
        build_input.final_source_url
    )
    .expect("Writing to string should not fail");
    writeln!(
        generated_markdown_document,
        "- Content SHA-256: {}",
        build_input.content_sha256
    )
    .expect("Writing to string should not fail");
    writeln!(
        generated_markdown_document,
        "- Pipeline Stage: {}",
        serialize_pipeline_stage_for_markdown(build_input.pipeline_stage_used)
    )
    .expect("Writing to string should not fail");
    generated_markdown_document.push('\n');
}

fn append_markdown_source_excerpt_section(
    generated_markdown_document: &mut String,
    build_input: &SkillMarkdownBuildInput<'_>,
) {
    generated_markdown_document.push_str("## Source Excerpts\n\n");
    if build_input.source_excerpt_lines.is_empty() {
        generated_markdown_document
            .push_str("No excerpt lines were available from source content.\n");
    } else {
        for source_excerpt_line in build_input.source_excerpt_lines {
            writeln!(generated_markdown_document, "> {source_excerpt_line}")
                .expect("Writing to string should not fail");
        }
    }
}

fn write_generated_skill_artifacts(
    skill_directory_path: &Path,
    generated_skill_markdown_document: &str,
    normalized_primary_markdown_content: &str,
) -> Result<()> {
    fs::create_dir_all(skill_directory_path).with_context(|| {
        format!(
            "Failed to create skill directory: {}",
            skill_directory_path.display()
        )
    })?;

    let skill_markdown_output_file_path = skill_directory_path.join("SKILL.md");
    fs::write(
        &skill_markdown_output_file_path,
        generated_skill_markdown_document,
    )
    .with_context(|| {
        format!(
            "Failed to write skill markdown output: {}",
            skill_markdown_output_file_path.display()
        )
    })?;

    let references_directory_path = skill_directory_path.join(REFERENCE_DIRECTORY_NAME);
    fs::create_dir_all(&references_directory_path).with_context(|| {
        format!(
            "Failed to create references directory: {}",
            references_directory_path.display()
        )
    })?;

    let primary_reference_output_file_path =
        references_directory_path.join(PRIMARY_REFERENCE_FILE_NAME);
    fs::write(
        &primary_reference_output_file_path,
        normalized_primary_markdown_content,
    )
    .with_context(|| {
        format!(
            "Failed to write primary reference output: {}",
            primary_reference_output_file_path.display()
        )
    })?;

    Ok(())
}

fn derive_stable_skill_slug(optional_skill_name: Option<&str>, parsed_target_url: &Url) -> String {
    if let Some(provided_skill_name) = optional_skill_name {
        let normalized_provided_skill_name = sanitize_skill_name(provided_skill_name);
        if !normalized_provided_skill_name.is_empty() {
            return normalized_provided_skill_name;
        }
    }

    let hostname_component = parsed_target_url
        .host_str()
        .unwrap_or("unknown-host")
        .replace('.', "-");

    let path_component = parsed_target_url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .take(3)
                .collect::<Vec<&str>>()
                .join("-")
        })
        .unwrap_or_default();

    let combined_component = if path_component.is_empty() {
        hostname_component
    } else {
        format!("{hostname_component}-{path_component}")
    };

    let normalized_combined_component = sanitize_skill_name(&combined_component);
    if normalized_combined_component.is_empty() {
        "webskills-generated-skill".to_string()
    } else {
        normalized_combined_component
    }
}

fn derive_versioned_skill_directory_name(stable_skill_slug: &str, content_sha256: &str) -> String {
    let hash_prefix: String = content_sha256.chars().take(12).collect();
    format!("{stable_skill_slug}-{hash_prefix}")
}

fn sanitize_skill_name(raw_skill_name: &str) -> String {
    let lowercase_skill_name = raw_skill_name.to_lowercase();
    let mut sanitized_skill_name = String::with_capacity(lowercase_skill_name.len());
    let mut previous_character_was_separator = false;

    for lowercase_character in lowercase_skill_name.chars() {
        if lowercase_character.is_ascii_alphanumeric() {
            sanitized_skill_name.push(lowercase_character);
            previous_character_was_separator = false;
            continue;
        }

        if !previous_character_was_separator {
            sanitized_skill_name.push('-');
            previous_character_was_separator = true;
        }
    }

    sanitized_skill_name.trim_matches('-').to_string()
}

fn build_origin_url(parsed_target_url: &Url) -> Result<Url> {
    let host_string = parsed_target_url
        .host_str()
        .ok_or_else(|| anyhow!("Missing host in target URL"))?;

    let mut origin_string = format!("{}://{}", parsed_target_url.scheme(), host_string);
    if let Some(port_value) = parsed_target_url.port() {
        origin_string.push(':');
        origin_string.push_str(&port_value.to_string());
    }

    Url::parse(&origin_string)
        .with_context(|| format!("Failed to parse origin URL: {origin_string}"))
}

fn build_same_origin_url(origin_url: &Url, path_value: &str) -> Result<Url> {
    let mut candidate_url = origin_url.clone();
    candidate_url.set_path(path_value);
    candidate_url.set_query(None);
    candidate_url.set_fragment(None);
    Url::parse(candidate_url.as_str())
        .with_context(|| format!("Failed to construct candidate URL for path {path_value}"))
}

fn compute_sha256_hex_digest(input_text_value: &str) -> String {
    let mut sha256_hasher = Sha256::new();
    sha256_hasher.update(input_text_value.as_bytes());
    format!("{:x}", sha256_hasher.finalize())
}

fn convert_html_document_to_markdown_prioritizing_primary_content(
    html_document_text: &str,
    optional_document_url: Option<&str>,
) -> String {
    if let Some(markdown_content_extracted_by_dom_smoothie) =
        extract_markdown_from_html_using_dom_smoothie(html_document_text, optional_document_url)
    {
        return markdown_content_extracted_by_dom_smoothie;
    }

    if let Some(primary_content_html_fragment) =
        extract_primary_content_html_fragment(html_document_text)
    {
        return normalize_markdown_for_deterministic_storage(&html2md::parse_html(
            &primary_content_html_fragment,
        ));
    }

    normalize_markdown_for_deterministic_storage(&html2md::parse_html(html_document_text))
}

fn extract_markdown_from_html_using_dom_smoothie(
    html_document_text: &str,
    optional_document_url: Option<&str>,
) -> Option<String> {
    let dom_smoothie_configuration = DomSmoothieConfig {
        text_mode: TextMode::Markdown,
        candidate_select_mode: CandidateSelectMode::DomSmoothie,
        ..Default::default()
    };

    let mut dom_smoothie_readability = Readability::new(
        html_document_text,
        optional_document_url,
        Some(dom_smoothie_configuration),
    )
    .ok()?;
    let extracted_article = dom_smoothie_readability.parse().ok()?;
    let normalized_markdown_content =
        normalize_markdown_for_deterministic_storage(extracted_article.text_content.as_ref());

    if normalized_markdown_content.chars().count() < MIN_PRIMARY_CONTENT_TEXT_CHARACTER_COUNT {
        return None;
    }

    Some(normalized_markdown_content)
}

fn extract_primary_content_html_fragment(html_document_text: &str) -> Option<String> {
    let parsed_html_document = Html::parse_document(html_document_text);
    let primary_content_selector_values = [
        "main article",
        "article",
        "[role='main'] article",
        "main",
        "[role='main']",
        "#content",
        ".content",
        ".post-content",
        ".entry-content",
        ".article",
        ".markdown-body",
        ".prose",
    ];

    let mut best_scored_candidate_html_fragment: Option<(usize, String)> = None;
    for (selector_priority_index, primary_content_selector_value) in
        primary_content_selector_values.iter().enumerate()
    {
        let Ok(parsed_primary_content_selector) = Selector::parse(primary_content_selector_value)
        else {
            continue;
        };

        for selected_primary_content_element in
            parsed_html_document.select(&parsed_primary_content_selector)
        {
            let collapsed_visible_text = collapse_whitespace_segments(
                &selected_primary_content_element
                    .text()
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let visible_text_character_count = collapsed_visible_text.chars().count();
            if visible_text_character_count < MIN_PRIMARY_CONTENT_TEXT_CHARACTER_COUNT {
                continue;
            }

            let selector_priority_bonus =
                (primary_content_selector_values.len() - selector_priority_index) * 25;
            let candidate_score = visible_text_character_count + selector_priority_bonus;
            let should_replace_existing_candidate = match &best_scored_candidate_html_fragment {
                Some((existing_candidate_score, _)) => candidate_score > *existing_candidate_score,
                None => true,
            };

            if should_replace_existing_candidate {
                best_scored_candidate_html_fragment =
                    Some((candidate_score, selected_primary_content_element.html()));
            }
        }
    }

    best_scored_candidate_html_fragment
        .map(|(_, best_candidate_html_fragment)| best_candidate_html_fragment)
}

fn collapse_whitespace_segments(raw_text_value: &str) -> String {
    raw_text_value
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

fn normalize_markdown_for_deterministic_storage(raw_markdown_content: &str) -> String {
    let line_ending_normalized_content = raw_markdown_content.replace("\r\n", "\n");

    let trimmed_line_values = line_ending_normalized_content
        .lines()
        .map(str::trim_end)
        .collect::<Vec<&str>>();

    trimmed_line_values.join("\n").trim().to_string()
}

fn is_markdown_content_type(content_type_header: &str) -> bool {
    let lowercase_content_type_header = content_type_header.to_ascii_lowercase();
    lowercase_content_type_header.contains("markdown")
        || lowercase_content_type_header.contains("md")
}

fn is_html_content_type(content_type_header: &str) -> bool {
    let lowercase_content_type_header = content_type_header.to_ascii_lowercase();
    lowercase_content_type_header.contains("text/html")
        || lowercase_content_type_header.contains("application/xhtml+xml")
}

fn is_probably_plain_text(content_type_header: &str) -> bool {
    let lowercase_content_type_header = content_type_header.to_ascii_lowercase();
    lowercase_content_type_header.is_empty()
        || lowercase_content_type_header.contains("text/plain")
        || lowercase_content_type_header.contains("text/")
}

fn looks_like_html_document(body_text: &str) -> bool {
    let trimmed_body_text = body_text.trim_start();
    trimmed_body_text.starts_with("<!doctype html")
        || trimmed_body_text.starts_with("<!DOCTYPE html")
        || trimmed_body_text.starts_with("<html")
}

fn is_heading_line(trimmed_markdown_line: &str) -> bool {
    trimmed_markdown_line.starts_with('#')
}

fn is_explicit_instruction_like_line(trimmed_markdown_line: &str) -> bool {
    if trimmed_markdown_line.starts_with("- ")
        || trimmed_markdown_line.starts_with("* ")
        || Regex::new(r"^\d+\.\s+")
            .expect("Numbered list regex must compile")
            .is_match(trimmed_markdown_line)
    {
        return true;
    }

    let lowercase_markdown_line = trimmed_markdown_line.to_ascii_lowercase();
    let explicit_instruction_like_prefix_values = [
        "run ",
        "use ",
        "install ",
        "set ",
        "add ",
        "create ",
        "configure ",
        "step ",
        "ensure ",
    ];

    explicit_instruction_like_prefix_values
        .iter()
        .any(|prefix_value| lowercase_markdown_line.starts_with(prefix_value))
}

fn is_command_like_line(trimmed_command_candidate_line: &str) -> bool {
    if trimmed_command_candidate_line.is_empty() {
        return false;
    }

    let disallowed_prefix_values = ["#", "//", "<!--"];
    if disallowed_prefix_values
        .iter()
        .any(|disallowed_prefix_value| {
            trimmed_command_candidate_line.starts_with(disallowed_prefix_value)
        })
    {
        return false;
    }

    let command_like_prefix_values = [
        "pnpm ", "npm ", "npx ", "cargo ", "uv ", "python ", "python3 ", "node ", "git ", "curl ",
        "wget ", "docker ", "make ",
    ];

    command_like_prefix_values
        .iter()
        .any(|command_like_prefix_value| {
            trimmed_command_candidate_line.starts_with(command_like_prefix_value)
        })
}

fn truncate_to_maximum_character_count(
    raw_text_value: &str,
    maximum_character_count: usize,
) -> String {
    if raw_text_value.chars().count() <= maximum_character_count {
        return raw_text_value.to_string();
    }

    let mut truncated_character_values: Vec<char> = raw_text_value
        .chars()
        .take(maximum_character_count)
        .collect();
    while truncated_character_values.last() == Some(&' ') {
        truncated_character_values.pop();
    }

    format!(
        "{}...",
        truncated_character_values.iter().collect::<String>()
    )
}

fn escape_for_yaml_double_quoted_string(raw_text_value: &str) -> String {
    raw_text_value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn serialize_pipeline_stage_for_markdown(pipeline_stage_used: PipelineStageUsed) -> &'static str {
    match pipeline_stage_used {
        PipelineStageUsed::ExplicitMarkdown => "explicit_markdown",
        PipelineStageUsed::MarkdownNegotiation => "markdown_negotiation",
        PipelineStageUsed::HtmlFallback => "html_fallback",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn sanitize_skill_name_normalizes_mixed_characters() {
        let sanitized_skill_name = sanitize_skill_name("My Custom Skill!!!");
        assert_eq!(sanitized_skill_name, "my-custom-skill");
    }

    #[test]
    fn normalize_discovered_link_removes_fragments() {
        let parsed_target_url = Url::parse("https://example.com/docs/page").unwrap();
        let normalized_discovered_link =
            normalize_discovered_link("/guide/intro#top", &parsed_target_url).unwrap();
        assert_eq!(
            normalized_discovered_link,
            "https://example.com/guide/intro"
        );
    }

    #[test]
    fn sanitize_final_response_url_for_persistence_strips_query_and_fragment_but_preserves_path_and_port(
    ) {
        let response_url_with_port =
            Url::parse("https://example.com:8443/docs/getting-started?sig=abc").unwrap();
        let sanitized_final_response_url =
            sanitize_final_response_url_for_persistence(&response_url_with_port);
        assert_eq!(
            sanitized_final_response_url.as_str(),
            "https://example.com:8443/docs/getting-started"
        );
    }

    #[test]
    fn build_skill_markdown_document_matches_golden_output() {
        let golden_output_markdown =
            include_str!("../tests/fixtures/expected_skill_output.md").replace("\r\n", "\n");
        let generated_markdown_document = build_skill_markdown_document(SkillMarkdownBuildInput {
            skill_slug: "example-com-docs",
            title_value: "example.com Web Skill",
            description_value: "Use when the user needs information or instructions from example.com related to: Example docs for deterministic extraction.",
            explicit_instruction_lines: &[
                "Run pnpm check before committing.".to_string(),
                "Use this endpoint for metadata retrieval.".to_string(),
            ],
            discovered_command_lines: &[
                "pnpm check".to_string(),
                "cargo clippy --all-targets --all-features".to_string(),
            ],
            discovered_api_usage_lines: &[
                "GET /v1/skills".to_string(),
                "https://example.com/api/v1/skills".to_string(),
            ],
            discovered_links: &[
                "https://example.com/docs/intro".to_string(),
                "https://example.com/docs/setup".to_string(),
            ],
            original_source_url: "https://example.com/docs",
            final_source_url: "https://example.com/docs",
            content_sha256: "0123456789abcdef",
            pipeline_stage_used: PipelineStageUsed::ExplicitMarkdown,
            source_excerpt_lines: &[
                "These docs explain setup and usage.".to_string(),
                "Run commands in order for deterministic results.".to_string(),
            ],
        });

        assert_eq!(generated_markdown_document, golden_output_markdown);
    }

    #[test]
    fn extract_discovered_command_lines_reads_fenced_shell_blocks() {
        let source_markdown_fixture = include_str!("../tests/fixtures/source_markdown.md");
        let discovered_command_lines = extract_discovered_command_lines(source_markdown_fixture);

        assert!(discovered_command_lines.contains(&"pnpm check".to_string()));
        assert!(discovered_command_lines
            .contains(&"cargo clippy --all-targets --all-features".to_string()));
    }

    #[test]
    fn extract_explicit_instruction_lines_uses_fallback_excerpt_when_no_instructions() {
        let no_instructions_source_fixture = include_str!("../tests/fixtures/no_instructions.md");
        let explicit_instruction_lines =
            extract_explicit_instruction_lines(no_instructions_source_fixture);

        assert!(!explicit_instruction_lines.is_empty());
        assert!(explicit_instruction_lines[0].contains("background context"));
    }

    #[test]
    fn derive_stable_skill_slug_uses_url_when_name_not_provided() {
        let parsed_target_url =
            Url::parse("https://docs.example.com/reference/start-here").unwrap();
        let derived_stable_skill_slug = derive_stable_skill_slug(None, &parsed_target_url);
        assert_eq!(
            derived_stable_skill_slug,
            "docs-example-com-reference-start-here"
        );
    }

    #[test]
    fn normalize_markdown_for_deterministic_storage_trims_and_normalizes_line_endings() {
        let normalized_markdown_content =
            normalize_markdown_for_deterministic_storage("line one  \r\nline two\r\n\r\n");
        assert_eq!(normalized_markdown_content, "line one\nline two");
    }

    #[test]
    fn derive_versioned_skill_directory_name_uses_hash_prefix() {
        let versioned_skill_directory_name =
            derive_versioned_skill_directory_name("example-skill", "abcdef1234567890");
        assert_eq!(versioned_skill_directory_name, "example-skill-abcdef123456");
    }

    #[test]
    fn extract_discovered_links_from_markdown_document_collects_normalized_urls() {
        let parsed_target_url = Url::parse("https://example.com/docs").unwrap();
        let discovered_link_values = extract_discovered_links_from_markdown_document(
            "[Intro](/intro) [External](https://external.example/path#section)",
            &parsed_target_url,
        );

        assert_eq!(
            discovered_link_values,
            vec![
                "https://example.com/intro".to_string(),
                "https://external.example/path".to_string(),
            ]
        );
    }

    #[test]
    fn extract_structured_sections_uses_hostname_in_fallback_description() {
        let extracted_sections = extract_structured_sections_from_markdown("", "example.com");
        assert!(extracted_sections
            .derived_description
            .contains("example.com"));
    }

    #[test]
    fn is_command_like_line_rejects_comment_lines() {
        assert!(!is_command_like_line("# pnpm check"));
        assert!(is_command_like_line("pnpm check"));
    }

    #[test]
    fn normalize_discovered_link_ignores_non_http_schemes() {
        let parsed_target_url = Url::parse("https://example.com").unwrap();
        assert!(
            normalize_discovered_link("mailto:hello@example.com", &parsed_target_url).is_none()
        );
        assert!(normalize_discovered_link("javascript:alert(1)", &parsed_target_url).is_none());
    }

    #[test]
    fn pipeline_stage_serialization_values_match_expected_contract() {
        assert_eq!(
            serialize_pipeline_stage_for_markdown(PipelineStageUsed::ExplicitMarkdown),
            "explicit_markdown"
        );
        assert_eq!(
            serialize_pipeline_stage_for_markdown(PipelineStageUsed::MarkdownNegotiation),
            "markdown_negotiation"
        );
        assert_eq!(
            serialize_pipeline_stage_for_markdown(PipelineStageUsed::HtmlFallback),
            "html_fallback"
        );
    }

    #[test]
    fn extract_discovered_api_usage_lines_finds_http_methods_and_urls() {
        let discovered_api_usage_lines = extract_discovered_api_usage_lines(
            "Use GET /v1/skills and POST /v1/skills. API base is https://example.com/api.",
        );
        assert!(discovered_api_usage_lines.contains(&"GET /v1/skills".to_string()));
        assert!(
            discovered_api_usage_lines.contains(&"POST /v1/skills.".to_string())
                || discovered_api_usage_lines.contains(&"POST /v1/skills".to_string())
        );
        assert!(
            discovered_api_usage_lines.contains(&"https://example.com/api.".to_string())
                || discovered_api_usage_lines.contains(&"https://example.com/api".to_string())
        );
    }

    #[test]
    fn extract_discovered_api_usage_lines_preserves_balanced_parenthesized_urls() {
        let discovered_api_usage_lines = extract_discovered_api_usage_lines(
            "Reference https://en.wikipedia.org/wiki/Path_(computing).",
        );
        assert!(discovered_api_usage_lines
            .contains(&"https://en.wikipedia.org/wiki/Path_(computing)".to_string()));
    }

    #[test]
    fn build_origin_url_keeps_non_default_port() {
        let parsed_target_url = Url::parse("https://example.com:8443/docs").unwrap();
        let origin_url = build_origin_url(&parsed_target_url).unwrap();
        assert_eq!(origin_url.as_str(), "https://example.com:8443/");
    }

    #[test]
    fn convert_fetched_document_to_markdown_converts_html_content() {
        let fetched_document = FetchedDocument {
            original_requested_url: Url::parse("https://example.com/llm.txt").unwrap(),
            final_response_url: Url::parse("https://example.com/docs/llm.txt").unwrap(),
            content_type_header: Some("text/html".to_string()),
            body_text: "<html><body><h1>Hello</h1></body></html>".to_string(),
        };
        let converted_markdown = convert_fetched_document_to_markdown(&fetched_document).unwrap();
        assert!(converted_markdown.contains("Hello"));
    }

    #[test]
    fn dom_smoothie_markdown_extraction_prefers_article_text() {
        let html_document_text = r#"
            <html>
                <body>
                    <nav>
                        <a href="/">Home</a>
                        <a href="/about">About</a>
                    </nav>
                    <main>
                        <article>
                            <h1>Primary Content Heading</h1>
                            <p>This is the first article paragraph with enough content to pass the extraction threshold and stay stable during markdown generation.</p>
                            <p>This is the second article paragraph to provide additional context and length for extraction scoring.</p>
                        </article>
                    </main>
                </body>
            </html>
        "#;

        let extracted_markdown_content =
            extract_markdown_from_html_using_dom_smoothie(html_document_text, None).unwrap();
        assert!(extracted_markdown_content.contains("Primary Content Heading"));
        assert!(!extracted_markdown_content.contains("Home"));
        assert!(!extracted_markdown_content.contains("About"));
    }

    #[test]
    fn primary_content_fragment_extraction_prefers_article_over_navigation() {
        let html_document_text = r#"
            <html>
                <body>
                    <nav>
                        <a href="/">Home</a>
                        <a href="/ideas">Ideas</a>
                        <a href="/about">About</a>
                    </nav>
                    <main>
                        <article>
                            <h1>Learn In Public</h1>
                            <p>This article contains enough meaningful text to exceed the primary content threshold and should be selected as the best extraction candidate for markdown conversion.</p>
                            <p>It includes multiple sentences and practical guidance so the content selector can confidently prefer this block over a compact navigation section.</p>
                        </article>
                    </main>
                </body>
            </html>
        "#;

        let primary_content_html_fragment =
            extract_primary_content_html_fragment(html_document_text).unwrap();
        assert!(primary_content_html_fragment.contains("<article>"));
        assert!(primary_content_html_fragment.contains("Learn In Public"));
        assert!(!primary_content_html_fragment.contains("<nav>"));
    }

    #[test]
    fn html_conversion_prioritizes_article_content_when_available() {
        let html_document_text = r#"
            <html>
                <body>
                    <nav>
                        <a href="/">Home</a>
                        <a href="/ideas">Ideas</a>
                    </nav>
                    <main>
                        <article>
                            <h1>Primary Content Heading</h1>
                            <p>This paragraph belongs to the primary content region and should be included in markdown output.</p>
                            <p>Another content paragraph helps ensure this block scores above navigation-only sections.</p>
                        </article>
                    </main>
                </body>
            </html>
        "#;

        let converted_markdown_document =
            convert_html_document_to_markdown_prioritizing_primary_content(
                html_document_text,
                None,
            );
        assert!(converted_markdown_document.contains("Primary Content Heading"));
        assert!(!converted_markdown_document.contains("Home"));
        assert!(!converted_markdown_document.contains("Ideas"));
    }

    #[test]
    fn derive_skill_description_truncates_long_values() {
        let description_value = derive_skill_description(
            &["A very long sentence that should still be truncated at some point because it keeps going and going without stopping until it exceeds the maximum allowed description length in this function.".to_string()],
            "example.com",
        );
        assert!(description_value.len() <= 163);
    }

    #[test]
    fn derive_skill_description_prefers_descriptive_sanitized_line() {
        let description_value = derive_skill_description(
            &[
                "Added in version 3.".to_string(),
                "This [cross-origin sharing standard](https://fetch.spec.whatwg.org/) allows servers to declare permitted origins.".to_string(),
            ],
            "example.com",
        );
        assert_eq!(
            description_value,
            "This cross-origin sharing standard allows servers to declare permitted origins."
        );
    }

    #[test]
    fn derive_skill_description_strips_markdown_heading_and_frontmatter_prefixes() {
        let description_value = derive_skill_description(
            &[
                "## Documentation Index".to_string(),
                "title: Markdown for Agents".to_string(),
                "Convert HTML responses into markdown for agent-friendly consumption.".to_string(),
            ],
            "example.com",
        );
        assert_eq!(
            description_value,
            "Convert HTML responses into markdown for agent-friendly consumption."
        );
    }

    #[test]
    fn build_skill_trigger_description_includes_host_and_usage_language() {
        let trigger_description = build_skill_trigger_description(
            "Example docs for deterministic extraction.",
            "example.com",
        );
        assert_eq!(
            trigger_description,
            "Use when the user needs information or instructions from example.com related to: Example docs for deterministic extraction."
        );
    }

    #[test]
    fn explicit_instruction_heuristic_accepts_numbered_steps() {
        assert!(is_explicit_instruction_like_line("1. Run pnpm check"));
    }

    #[test]
    fn explicit_instruction_heuristic_accepts_action_prefixes() {
        assert!(is_explicit_instruction_like_line(
            "Install dependencies first"
        ));
        assert!(is_explicit_instruction_like_line("Use this endpoint"));
    }

    #[test]
    fn explicit_instruction_heuristic_rejects_generic_statements() {
        assert!(!is_explicit_instruction_like_line(
            "This page describes architecture."
        ));
    }

    #[test]
    fn markdown_content_type_detection_identifies_markdown_variants() {
        assert!(is_markdown_content_type("text/markdown; charset=utf-8"));
        assert!(is_markdown_content_type("application/x-markdown"));
    }

    #[test]
    fn html_detection_identifies_doctype_and_html_tag() {
        assert!(looks_like_html_document("<!doctype html><html></html>"));
        assert!(looks_like_html_document("<html><body></body></html>"));
    }

    #[test]
    fn truncate_to_maximum_character_count_appends_ellipsis() {
        let truncated_text_value = truncate_to_maximum_character_count("1234567890", 5);
        assert_eq!(truncated_text_value, "12345...");
    }

    #[test]
    fn extracted_source_excerpts_skip_headings() {
        let source_excerpt_lines = extract_source_excerpt_lines("# Heading\n\nUseful paragraph.");
        assert_eq!(source_excerpt_lines, vec!["Useful paragraph.".to_string()]);
    }

    #[test]
    fn extracted_instruction_lines_include_all_matches() {
        let repeated_instruction_content = (1..=20)
            .map(|instruction_number| format!("- step {instruction_number}"))
            .collect::<Vec<String>>()
            .join("\n");
        let explicit_instruction_lines =
            extract_explicit_instruction_lines(&repeated_instruction_content);
        assert_eq!(explicit_instruction_lines.len(), 20);
    }

    #[test]
    fn discovered_commands_are_sorted_and_unique() {
        let discovered_command_lines = extract_discovered_command_lines(
            "```bash\npnpm check\npnpm check\ncargo clippy --all-targets --all-features\n```",
        );
        assert_eq!(
            discovered_command_lines,
            vec![
                "cargo clippy --all-targets --all-features".to_string(),
                "pnpm check".to_string()
            ]
        );
    }

    #[test]
    fn discovered_api_usage_is_sorted_and_unique() {
        let discovered_api_usage_lines = extract_discovered_api_usage_lines(
            "GET /v1/items GET /v1/items https://example.com/api https://example.com/api",
        );
        let unique_discovered_api_usage_lines: HashSet<String> =
            discovered_api_usage_lines.clone().into_iter().collect();
        assert_eq!(
            unique_discovered_api_usage_lines.len(),
            discovered_api_usage_lines.len()
        );
    }

    #[test]
    fn versioned_directory_name_contains_slug_and_prefix() {
        let versioned_directory_name =
            derive_versioned_skill_directory_name("my-skill", "1234567890abcdef");
        assert_eq!(versioned_directory_name, "my-skill-1234567890ab");
    }

    #[test]
    fn normalize_discovered_link_handles_relative_paths() {
        let parsed_target_url = Url::parse("https://example.com/docs/page").unwrap();
        let normalized_discovered_link =
            normalize_discovered_link("../intro", &parsed_target_url).unwrap();
        assert_eq!(normalized_discovered_link, "https://example.com/intro");
    }

    #[test]
    fn build_same_origin_url_replaces_path_and_clears_query() {
        let origin_url = Url::parse("https://example.com/").unwrap();
        let same_origin_url = build_same_origin_url(&origin_url, "/abc").unwrap();
        assert_eq!(same_origin_url.as_str(), "https://example.com/abc");
    }

    #[test]
    fn build_explicit_markdown_candidate_urls_for_directory_path_stay_within_target_scope() {
        let parsed_target_url = Url::parse("https://example.com/docs/guide/").unwrap();
        let explicit_markdown_candidate_urls =
            build_explicit_markdown_candidate_urls(&parsed_target_url).unwrap();

        assert!(!explicit_markdown_candidate_urls.is_empty());
        assert!(explicit_markdown_candidate_urls
            .iter()
            .all(|candidate_url| candidate_url
                .as_str()
                .starts_with("https://example.com/docs/guide/")));
    }

    #[test]
    fn build_explicit_markdown_candidate_urls_for_file_path_use_parent_directory() {
        let parsed_target_url = Url::parse("https://example.com/docs/guide.html").unwrap();
        let explicit_markdown_candidate_urls =
            build_explicit_markdown_candidate_urls(&parsed_target_url).unwrap();

        let explicit_markdown_candidate_url_values = explicit_markdown_candidate_urls
            .iter()
            .map(Url::as_str)
            .collect::<Vec<&str>>();
        assert_eq!(
            explicit_markdown_candidate_url_values.first(),
            Some(&"https://example.com/docs/llms.txt")
        );
        assert!(!explicit_markdown_candidate_url_values.contains(&"https://example.com/llms.txt"));
    }

    #[test]
    fn build_explicit_markdown_candidate_urls_for_root_path_use_origin_only() {
        let parsed_target_url = Url::parse("https://example.com/").unwrap();
        let explicit_markdown_candidate_urls =
            build_explicit_markdown_candidate_urls(&parsed_target_url).unwrap();

        assert!(!explicit_markdown_candidate_urls.is_empty());
        assert!(explicit_markdown_candidate_urls
            .iter()
            .all(|candidate_url| candidate_url.as_str().starts_with("https://example.com/")));
    }

    #[test]
    fn build_explicit_markdown_candidate_urls_for_root_level_leaf_path_skip_site_wide_fallback() {
        let parsed_target_url = Url::parse("https://example.com/specification").unwrap();
        let explicit_markdown_candidate_urls =
            build_explicit_markdown_candidate_urls(&parsed_target_url).unwrap();

        assert!(explicit_markdown_candidate_urls.is_empty());
    }

    #[test]
    fn normalize_extracted_absolute_url_for_listing_trims_unbalanced_parentheses_and_punctuation() {
        assert_eq!(
            normalize_extracted_absolute_url_for_listing(
                "https://en.wikipedia.org/wiki/Path_(computing)."
            ),
            Some("https://en.wikipedia.org/wiki/Path_(computing)".to_string())
        );
        assert_eq!(
            normalize_extracted_absolute_url_for_listing("https://example.com/foo))."),
            Some("https://example.com/foo".to_string())
        );
    }

    #[test]
    fn append_markdown_instruction_section_normalizes_existing_list_markers() {
        let generated_markdown_document = build_skill_markdown_document(SkillMarkdownBuildInput {
            skill_slug: "test-skill",
            title_value: "Test Skill",
            description_value:
                "Use when the user needs information or instructions from example.com related to: Test Description.",
            explicit_instruction_lines: &[
                "- First instruction".to_string(),
                "1. Second instruction".to_string(),
                "* Third instruction".to_string(),
            ],
            discovered_command_lines: &[],
            discovered_api_usage_lines: &[],
            discovered_links: &[],
            original_source_url: "https://example.com",
            final_source_url: "https://example.com",
            content_sha256: "abc",
            pipeline_stage_used: PipelineStageUsed::HtmlFallback,
            source_excerpt_lines: &[],
        });

        assert!(generated_markdown_document.contains("1. First instruction"));
        assert!(generated_markdown_document.contains("2. Second instruction"));
        assert!(generated_markdown_document.contains("3. Third instruction"));
        assert!(!generated_markdown_document.contains("1. - "));
        assert!(!generated_markdown_document.contains("1. 1. "));
    }
}
