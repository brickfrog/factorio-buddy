//! Bounded Beads issue creation for autonomous runtime reports.
//!
//! This module deliberately exposes one operation: create an open bug in a
//! fixed project root, or return an existing issue whose normalized title is
//! exactly equal. It does not expose a generic Beads command runner.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::{sleep, timeout};

pub const MAX_TITLE_CHARS: usize = 160;
pub const MAX_BEHAVIOR_CHARS: usize = 4_000;
pub const MAX_EVIDENCE_ITEMS: usize = 10;
pub const MAX_EVIDENCE_ITEM_CHARS: usize = 1_000;
pub const MAX_REPRODUCTION_CHARS: usize = 4_000;
pub const MAX_LABELS: usize = 8;
pub const MAX_LABEL_CHARS: usize = 32;
pub const MAX_PRIORITY: u8 = 4;
pub const DEFAULT_BD_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BD_STDOUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_BD_STDERR_BYTES: usize = 64 * 1024;
const ISSUE_CREATION_LOCK_FILE: &str = ".factorio-buddy-file-issue.lock";
const ISSUE_CREATION_LOCK_RETRY: Duration = Duration::from_millis(10);
const PROCESS_SPAWN_RETRIES: usize = 3;

// Beads supports several environment variables that override cwd discovery or
// select a different Dolt backend. Remove known names unconditionally, then
// remove every inherited BEADS_*/BD_* variable defensively when building the
// subprocess.
const BEADS_ROUTING_ENV: &[&str] = &[
    "BEADS_DB",
    "BEADS_DIR",
    "BEADS_GLOBAL",
    "BEADS_REPO",
    "BEADS_SHARED_SERVER_DIR",
    "BEADS_DOLT_DATA_DIR",
    "BEADS_DOLT_SERVER_DATABASE",
    "BEADS_DOLT_SHARED_SERVER",
    "BEADS_DOLT_SERVER_MODE",
    "BEADS_DOLT_SERVER_HOST",
    "BEADS_DOLT_SERVER_PORT",
    "BEADS_DOLT_SERVER_SOCKET",
    "BD_DB",
];

/// Labels the autonomous reporter may attach to a bug.
///
/// Keeping this list explicit prevents model-authored labels from changing
/// repository workflow state or inventing new organizational conventions.
pub const ALLOWED_LABELS: &[&str] = &[
    "agent",
    "automation",
    "beads",
    "belts",
    "crafting",
    "factorio",
    "factorio-2.0",
    "fluids",
    "fuel",
    "gameplay",
    "inventory",
    "logistics",
    "mcp",
    "observability",
    "pathfinding",
    "placement",
    "power",
    "production",
    "research",
    "runtime",
    "transport",
    "ui",
];

/// Model-authored fields for a single bug report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueReportRequest {
    pub title: String,
    pub observed_behavior: String,
    pub expected_behavior: String,
    pub evidence: Vec<String>,
    #[serde(default)]
    pub reproduction: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub priority: u8,
}

/// Runtime-authored metadata. Callers must populate this from trusted process
/// state rather than accepting it from the model request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedIssueContext {
    pub agent_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub commit_sha: Option<String>,
    pub timestamp: String,
    #[serde(default)]
    pub factorio_version: Option<String>,
}

/// Successful issue creation or exact-duplicate resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueReportResult {
    pub success: bool,
    pub id: String,
    pub title: String,
    pub duplicate: bool,
    pub status: String,
    pub issue_type: String,
    pub priority: u8,
}

#[derive(Debug, Error)]
pub enum IssueReportError {
    #[error("invalid issue report field `{field}`: {reason}")]
    InvalidField { field: &'static str, reason: String },

    #[error("configured project root is unavailable: {root:?}: {source}")]
    ProjectRootUnavailable {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("configured project root does not contain a .beads repository: {root:?}")]
    BeadsRepositoryMissing { root: PathBuf },

    #[error("repository issue lock {path:?} failed: {source}")]
    RepositoryLockIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("repository issue lock timed out after {timeout_ms} ms")]
    RepositoryLockTimeout { timeout_ms: u128 },

    #[error("bd executable is unavailable: {source}")]
    BdUnavailable {
        #[source]
        source: std::io::Error,
    },

    #[error("failed to start bd for {operation}: {source}")]
    ProcessSpawn {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("bd {operation} timed out after {timeout_ms} ms")]
    ProcessTimeout {
        operation: &'static str,
        timeout_ms: u128,
    },

    #[error("bd {operation} I/O failed: {source}")]
    ProcessIo {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("bd {operation} exited unsuccessfully (code {code:?}): {stderr}")]
    ProcessFailed {
        operation: &'static str,
        code: Option<i32>,
        stderr: String,
    },

    #[error("bd {operation} returned invalid JSON: {source}")]
    InvalidJson {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },

    #[error("bd {operation} returned an invalid response: {reason}")]
    InvalidResponse {
        operation: &'static str,
        reason: String,
    },

    #[error("bd {operation} output exceeded the {limit_bytes}-byte safety limit")]
    OutputTooLarge {
        operation: &'static str,
        limit_bytes: usize,
    },
}

impl IssueReportError {
    /// Stable machine-facing category suitable for an MCP error payload.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidField { .. } => "invalid_issue_report",
            Self::ProjectRootUnavailable { .. } | Self::BeadsRepositoryMissing { .. } => {
                "beads_repository_unavailable"
            }
            Self::RepositoryLockIo { .. } => "beads_lock_failed",
            Self::RepositoryLockTimeout { .. } => "beads_lock_timeout",
            Self::BdUnavailable { .. } => "bd_unavailable",
            Self::ProcessTimeout { .. } => "bd_timeout",
            Self::ProcessSpawn { .. } | Self::ProcessIo { .. } | Self::ProcessFailed { .. } => {
                "bd_process_failed"
            }
            Self::InvalidJson { .. }
            | Self::InvalidResponse { .. }
            | Self::OutputTooLarge { .. } => "bd_invalid_response",
        }
    }
}

/// Fixed-root adapter for the two Beads operations required by self-reporting:
/// an all-issues lookup and creation of an open bug.
pub struct BeadsIssueReporter {
    project_root: PathBuf,
    bd_executable: PathBuf,
    process_timeout: Duration,
}

impl BeadsIssueReporter {
    /// Create a reporter bound to one trusted project root.
    pub fn new(project_root: impl AsRef<Path>) -> Result<Self, IssueReportError> {
        let root = validate_project_root(project_root.as_ref())?;
        let bd_executable =
            which::which("bd").map_err(|source| IssueReportError::BdUnavailable {
                source: std::io::Error::new(std::io::ErrorKind::NotFound, source.to_string()),
            })?;
        Ok(Self {
            project_root: root,
            bd_executable,
            process_timeout: DEFAULT_BD_TIMEOUT,
        })
    }

    /// The canonical, fixed root used for every Beads subprocess.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Create a bug or return the existing exact normalized-title duplicate.
    pub async fn file_issue(
        &self,
        request: IssueReportRequest,
        context: TrustedIssueContext,
    ) -> Result<IssueReportResult, IssueReportError> {
        let report = ValidatedReport::new(request)?;
        let context = ValidatedContext::new(context)?;
        let description = render_description(&report, &context);

        // Keep exact-title lookup and creation in one repository-scoped
        // critical section. MCP processes are short lived, so a process-local
        // mutex cannot prevent two independent turns from creating the same
        // issue. The advisory lock lives under the fixed canonical .beads
        // repository and its acquisition is bounded by the configured timeout.
        let _guard = self.acquire_repository_lock().await?;

        if let Some(existing) = self.find_exact_duplicate(&report.title).await? {
            return issue_result(existing, true);
        }

        let created = self.create_bug(&report, &description).await?;
        validate_created_issue(&created, &report)?;
        issue_result(created, false)
    }

    async fn acquire_repository_lock(&self) -> Result<RepositoryIssueLock, IssueReportError> {
        let path = issue_creation_lock_path(&self.project_root);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| IssueReportError::RepositoryLockIo {
                path: path.clone(),
                source,
            })?;
        let started = Instant::now();

        loop {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => return Ok(RepositoryIssueLock { file }),
                Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                    let elapsed = started.elapsed();
                    if elapsed >= self.process_timeout {
                        return Err(IssueReportError::RepositoryLockTimeout {
                            timeout_ms: self.process_timeout.as_millis(),
                        });
                    }
                    sleep(ISSUE_CREATION_LOCK_RETRY.min(self.process_timeout - elapsed)).await;
                }
                Err(source) => {
                    return Err(IssueReportError::RepositoryLockIo {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
    }

    async fn find_exact_duplicate(&self, title: &str) -> Result<Option<BdIssue>, IssueReportError> {
        let args = [
            OsString::from("--json"),
            OsString::from("--actor=factorio-buddy"),
            OsString::from("list"),
            OsString::from("--all"),
            OsString::from("--limit=0"),
        ];
        let stdout = self.run_bd("duplicate lookup", &args, None).await?;
        let issues: Vec<BdIssue> =
            serde_json::from_slice(&stdout).map_err(|source| IssueReportError::InvalidJson {
                operation: "duplicate lookup",
                source,
            })?;
        let normalized = normalize_title(title);
        Ok(issues
            .into_iter()
            .find(|issue| normalize_title(&issue.title) == normalized))
    }

    async fn create_bug(
        &self,
        report: &ValidatedReport,
        description: &str,
    ) -> Result<BdIssue, IssueReportError> {
        let mut args = vec![
            OsString::from("--json"),
            OsString::from("--actor=factorio-buddy"),
            OsString::from("create"),
            OsString::from(format!("--title={}", report.title)),
            OsString::from("--type=bug"),
            OsString::from(format!("--priority=P{}", report.priority)),
            OsString::from("--body-file=-"),
        ];
        if !report.labels.is_empty() {
            args.push(OsString::from(format!(
                "--labels={}",
                report.labels.join(",")
            )));
        }

        let stdout = self
            .run_bd("issue creation", &args, Some(description.as_bytes()))
            .await?;
        parse_created_issue(&stdout)
    }

    async fn run_bd(
        &self,
        operation: &'static str,
        args: &[OsString],
        stdin: Option<&[u8]>,
    ) -> Result<Vec<u8>, IssueReportError> {
        let mut spawn_attempt = 0_usize;
        let mut child = loop {
            let mut command = self.bd_command(args, stdin.is_some());
            match command.spawn() {
                Ok(child) => break child,
                Err(source)
                    if executable_file_busy(&source) && spawn_attempt < PROCESS_SPAWN_RETRIES =>
                {
                    // A concurrently replaced executable can briefly produce
                    // ETXTBSY on Linux. No process started, so retrying spawn
                    // is side-effect free and remains inside the bounded
                    // repository critical section.
                    spawn_attempt += 1;
                    sleep(ISSUE_CREATION_LOCK_RETRY).await;
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Err(IssueReportError::BdUnavailable { source });
                }
                Err(source) => {
                    return Err(IssueReportError::ProcessSpawn { operation, source });
                }
            }
        };

        let mut child_stdin = child.stdin.take();
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| IssueReportError::ProcessIo {
                operation,
                source: std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "bd stdout unavailable",
                ),
            })?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or_else(|| IssueReportError::ProcessIo {
                operation,
                source: std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "bd stderr unavailable",
                ),
            })?;

        let completed = timeout(self.process_timeout, async {
            let write_input = async {
                if let Some(input) = stdin {
                    let mut writer = child_stdin.take().ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::BrokenPipe, "bd stdin unavailable")
                    })?;
                    writer.write_all(input).await?;
                    writer.shutdown().await?;
                }
                Ok::<(), std::io::Error>(())
            };
            let read_stdout = read_bounded(child_stdout, MAX_BD_STDOUT_BYTES);
            let read_stderr = read_bounded(child_stderr, MAX_BD_STDERR_BYTES);
            let wait = child.wait();
            let (_, stdout, stderr, status) =
                tokio::try_join!(write_input, read_stdout, read_stderr, wait)?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        })
        .await
        .map_err(|_| IssueReportError::ProcessTimeout {
            operation,
            timeout_ms: self.process_timeout.as_millis(),
        })?
        .map_err(|source| IssueReportError::ProcessIo { operation, source })?;

        let (status, stdout, stderr) = completed;
        if stdout.truncated {
            return Err(IssueReportError::OutputTooLarge {
                operation,
                limit_bytes: MAX_BD_STDOUT_BYTES,
            });
        }
        if !status.success() {
            return Err(IssueReportError::ProcessFailed {
                operation,
                code: status.code(),
                stderr: bounded_output(&stderr.bytes),
            });
        }
        Ok(stdout.bytes)
    }

    fn bd_command(&self, args: &[OsString], pipe_stdin: bool) -> Command {
        let mut command = Command::new(&self.bd_executable);
        command
            .args(args)
            .current_dir(&self.project_root)
            .stdin(if pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        clear_beads_environment(&mut command);
        command
    }

    #[cfg(test)]
    fn with_test_bd(
        project_root: impl AsRef<Path>,
        bd_executable: impl Into<PathBuf>,
        process_timeout: Duration,
    ) -> Result<Self, IssueReportError> {
        Ok(Self {
            project_root: validate_project_root(project_root.as_ref())?,
            bd_executable: bd_executable.into(),
            process_timeout,
        })
    }
}

fn executable_file_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Holds the repository advisory lock across duplicate lookup and creation.
/// Closing the descriptor releases the lock even if the task is cancelled.
struct RepositoryIssueLock {
    file: File,
}

impl Drop for RepositoryIssueLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
struct ValidatedReport {
    title: String,
    observed_behavior: String,
    expected_behavior: String,
    evidence: Vec<String>,
    reproduction: Option<String>,
    labels: Vec<String>,
    priority: u8,
}

impl ValidatedReport {
    fn new(request: IssueReportRequest) -> Result<Self, IssueReportError> {
        let title = validate_model_text("title", request.title, MAX_TITLE_CHARS)?;
        let observed_behavior = validate_model_text(
            "observed_behavior",
            request.observed_behavior,
            MAX_BEHAVIOR_CHARS,
        )?;
        let expected_behavior = validate_model_text(
            "expected_behavior",
            request.expected_behavior,
            MAX_BEHAVIOR_CHARS,
        )?;

        if request.evidence.is_empty() || request.evidence.len() > MAX_EVIDENCE_ITEMS {
            return Err(invalid_field(
                "evidence",
                format!("must contain 1 to {MAX_EVIDENCE_ITEMS} entries"),
            ));
        }
        let evidence = request
            .evidence
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                validate_model_text("evidence", item, MAX_EVIDENCE_ITEM_CHARS).map_err(|error| {
                    match error {
                        IssueReportError::InvalidField { reason, .. } => {
                            invalid_field("evidence", format!("entry {} {reason}", index + 1))
                        }
                        other => other,
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let reproduction = request
            .reproduction
            .map(|value| validate_model_text("reproduction", value, MAX_REPRODUCTION_CHARS))
            .transpose()?;
        let labels = validate_labels(request.labels)?;
        if request.priority > MAX_PRIORITY {
            return Err(invalid_field(
                "priority",
                format!("must be between 0 and {MAX_PRIORITY}"),
            ));
        }

        Ok(Self {
            title,
            observed_behavior,
            expected_behavior,
            evidence,
            reproduction,
            labels,
            priority: request.priority,
        })
    }
}

#[derive(Debug, Serialize)]
struct ValidatedContext {
    agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commit_sha: Option<String>,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    factorio_version: Option<String>,
}

impl ValidatedContext {
    fn new(context: TrustedIssueContext) -> Result<Self, IssueReportError> {
        Ok(Self {
            agent_id: validate_context_value("agent_id", context.agent_id, 128)?,
            session_id: context
                .session_id
                .map(|value| validate_context_value("session_id", value, 128))
                .transpose()?,
            commit_sha: context
                .commit_sha
                .map(|value| validate_context_value("commit_sha", value, 128))
                .transpose()?,
            timestamp: validate_context_value("timestamp", context.timestamp, 64)?,
            factorio_version: context
                .factorio_version
                .map(|value| validate_context_value("factorio_version", value, 64))
                .transpose()?,
        })
    }
}

#[derive(Debug, Deserialize)]
struct BdIssue {
    id: String,
    title: String,
    status: String,
    issue_type: String,
    priority: u8,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BdCreateOutput {
    One(BdIssue),
    Many(Vec<BdIssue>),
}

fn validate_project_root(root: &Path) -> Result<PathBuf, IssueReportError> {
    let canonical =
        root.canonicalize()
            .map_err(|source| IssueReportError::ProjectRootUnavailable {
                root: root.to_path_buf(),
                source,
            })?;
    if !canonical.join(".beads").is_dir() {
        return Err(IssueReportError::BeadsRepositoryMissing { root: canonical });
    }
    Ok(canonical)
}

fn issue_creation_lock_path(project_root: &Path) -> PathBuf {
    project_root.join(".beads").join(ISSUE_CREATION_LOCK_FILE)
}

fn clear_beads_environment(command: &mut Command) {
    for name in BEADS_ROUTING_ENV {
        command.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("BEADS_") || name_text.starts_with("BD_") {
            command.env_remove(name);
        }
    }
}

fn validate_model_text(
    field: &'static str,
    value: String,
    max_chars: usize,
) -> Result<String, IssueReportError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(invalid_field(field, "must not be empty"));
    }
    let count = value.chars().count();
    if count > max_chars {
        return Err(invalid_field(
            field,
            format!("must not exceed {max_chars} characters (got {count})"),
        ));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
    {
        return Err(invalid_field(
            field,
            "contains a disallowed control character",
        ));
    }
    Ok(value)
}

fn validate_context_value(
    field: &'static str,
    value: String,
    max_chars: usize,
) -> Result<String, IssueReportError> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(invalid_field(field, "must not be empty"));
    }
    let count = value.chars().count();
    if count > max_chars {
        return Err(invalid_field(
            field,
            format!("must not exceed {max_chars} characters (got {count})"),
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid_field(field, "must be a single-line value"));
    }
    Ok(value)
}

fn validate_labels(labels: Vec<String>) -> Result<Vec<String>, IssueReportError> {
    if labels.len() > MAX_LABELS {
        return Err(invalid_field(
            "labels",
            format!("must contain at most {MAX_LABELS} entries"),
        ));
    }
    let mut validated = Vec::with_capacity(labels.len());
    for label in labels {
        if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            return Err(invalid_field(
                "labels",
                format!("each label must contain 1 to {MAX_LABEL_CHARS} characters"),
            ));
        }
        if !label.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        }) || !label
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(invalid_field(
                "labels",
                format!("label `{label}` is not a normalized label"),
            ));
        }
        if !ALLOWED_LABELS.contains(&label.as_str()) {
            return Err(invalid_field(
                "labels",
                format!("label `{label}` is not allowed"),
            ));
        }
        if validated.contains(&label) {
            return Err(invalid_field(
                "labels",
                format!("label `{label}` is duplicated"),
            ));
        }
        validated.push(label);
    }
    validated.sort();
    Ok(validated)
}

fn render_description(report: &ValidatedReport, context: &ValidatedContext) -> String {
    let mut output = String::from("## Model-supplied report\n\n### Observed behavior\n\n");
    append_blockquote(&mut output, &report.observed_behavior);
    output.push_str("\n### Expected behavior\n\n");
    append_blockquote(&mut output, &report.expected_behavior);
    output.push_str("\n### Evidence\n");
    for (index, evidence) in report.evidence.iter().enumerate() {
        output.push_str(&format!("\n{}.\n\n", index + 1));
        append_blockquote(&mut output, evidence);
    }
    if let Some(reproduction) = &report.reproduction {
        output.push_str("\n### Reproduction\n\n");
        append_blockquote(&mut output, reproduction);
    }

    output.push_str("\n## Trusted runtime context\n\n");
    let json = serde_json::to_string_pretty(context)
        .expect("validated trusted issue context must serialize");
    for line in json.lines() {
        output.push_str("    ");
        output.push_str(line);
        output.push('\n');
    }
    output
}

fn append_blockquote(output: &mut String, text: &str) {
    for line in text.lines() {
        output.push_str("> ");
        output.push_str(line);
        output.push('\n');
    }
}

fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn parse_created_issue(stdout: &[u8]) -> Result<BdIssue, IssueReportError> {
    let parsed: BdCreateOutput =
        serde_json::from_slice(stdout).map_err(|source| IssueReportError::InvalidJson {
            operation: "issue creation",
            source,
        })?;
    match parsed {
        BdCreateOutput::One(issue) => Ok(issue),
        BdCreateOutput::Many(mut issues) if issues.len() == 1 => Ok(issues.remove(0)),
        BdCreateOutput::Many(issues) => Err(IssueReportError::InvalidResponse {
            operation: "issue creation",
            reason: format!("expected one created issue, got {}", issues.len()),
        }),
    }
}

fn validate_created_issue(
    issue: &BdIssue,
    report: &ValidatedReport,
) -> Result<(), IssueReportError> {
    validate_issue_identity(issue, "issue creation")?;
    if normalize_title(&issue.title) != normalize_title(&report.title) {
        return Err(IssueReportError::InvalidResponse {
            operation: "issue creation",
            reason: "created title does not match the requested title".to_string(),
        });
    }
    if issue.status != "open" {
        return Err(IssueReportError::InvalidResponse {
            operation: "issue creation",
            reason: format!("created issue status is {:?}, expected open", issue.status),
        });
    }
    if issue.issue_type != "bug" {
        return Err(IssueReportError::InvalidResponse {
            operation: "issue creation",
            reason: format!("created issue type is {:?}, expected bug", issue.issue_type),
        });
    }
    if issue.priority != report.priority {
        return Err(IssueReportError::InvalidResponse {
            operation: "issue creation",
            reason: format!(
                "created issue priority is {}, expected {}",
                issue.priority, report.priority
            ),
        });
    }
    Ok(())
}

fn validate_issue_identity(
    issue: &BdIssue,
    operation: &'static str,
) -> Result<(), IssueReportError> {
    if issue.id.is_empty()
        || issue.id.len() > 128
        || !issue
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(IssueReportError::InvalidResponse {
            operation,
            reason: "issue id is missing or malformed".to_string(),
        });
    }
    if issue.title.trim().is_empty() {
        return Err(IssueReportError::InvalidResponse {
            operation,
            reason: "issue title is missing".to_string(),
        });
    }
    Ok(())
}

fn issue_result(issue: BdIssue, duplicate: bool) -> Result<IssueReportResult, IssueReportError> {
    let operation = if duplicate {
        "duplicate lookup"
    } else {
        "issue creation"
    };
    validate_issue_identity(&issue, operation)?;
    Ok(IssueReportResult {
        success: true,
        id: issue.id,
        title: issue.title,
        duplicate,
        status: issue.status,
        issue_type: issue.issue_type,
        priority: issue.priority,
    })
}

fn invalid_field(field: &'static str, reason: impl Into<String>) -> IssueReportError {
    IssueReportError::InvalidField {
        field,
        reason: reason.into(),
    }
}

struct BoundedRead {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(limit.min(16 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = read.min(remaining);
        bytes.extend_from_slice(&buffer[..retained]);
        if retained < read {
            truncated = true;
        }
    }
    Ok(BoundedRead { bytes, truncated })
}

fn bounded_output(bytes: &[u8]) -> String {
    const MAX_ERROR_CHARS: usize = 1_000;
    let output = String::from_utf8_lossy(bytes);
    let mut chars = output.chars();
    let bounded: String = chars.by_ref().take(MAX_ERROR_CHARS).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Child, Command as StdCommand};
    use std::thread;
    use std::time::Instant;

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    const LOCK_HELPER_PATH_ENV: &str = "FACTORIO_BUDDY_TEST_LOCK_PATH";
    const LOCK_HELPER_READY_ENV: &str = "FACTORIO_BUDDY_TEST_LOCK_READY";
    const LOCK_HELPER_RELEASE_ENV: &str = "FACTORIO_BUDDY_TEST_LOCK_RELEASE";

    struct LockHelperProcess {
        child: Child,
        release_path: PathBuf,
    }

    impl LockHelperProcess {
        fn spawn(project_root: &Path) -> Self {
            let lock_path = issue_creation_lock_path(project_root);
            let ready_path = project_root.join("lock-helper-ready");
            let release_path = project_root.join("lock-helper-release");
            let child = StdCommand::new(std::env::current_exe().unwrap())
                .arg("--ignored")
                .arg("--exact")
                .arg("issue_report::tests::repository_lock_child_process_helper")
                .env(LOCK_HELPER_PATH_ENV, lock_path)
                .env(LOCK_HELPER_READY_ENV, &ready_path)
                .env(LOCK_HELPER_RELEASE_ENV, &release_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut helper = Self {
                child,
                release_path,
            };
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready_path.exists() {
                if let Some(status) = helper.child.try_wait().unwrap() {
                    panic!("repository lock helper exited before acquiring lock: {status}");
                }
                assert!(
                    Instant::now() < deadline,
                    "repository lock helper did not acquire lock in time"
                );
                thread::sleep(Duration::from_millis(10));
            }
            helper
        }
    }

    impl Drop for LockHelperProcess {
        fn drop(&mut self) {
            let _ = fs::write(&self.release_path, b"release");
            let deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < deadline {
                match self.child.try_wait() {
                    Ok(Some(_)) => return,
                    Ok(None) => thread::sleep(Duration::from_millis(10)),
                    Err(_) => break,
                }
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    #[ignore = "subprocess helper for the repository advisory lock regression"]
    fn repository_lock_child_process_helper() {
        let Some(lock_path) = std::env::var_os(LOCK_HELPER_PATH_ENV) else {
            return;
        };
        let ready_path = PathBuf::from(std::env::var_os(LOCK_HELPER_READY_ENV).unwrap());
        let release_path = PathBuf::from(std::env::var_os(LOCK_HELPER_RELEASE_ENV).unwrap());
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        FileExt::lock_exclusive(&file).unwrap();
        fs::write(&ready_path, b"ready").unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        while !release_path.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        FileExt::unlock(&file).unwrap();
    }

    fn valid_request(title: impl Into<String>) -> IssueReportRequest {
        IssueReportRequest {
            title: title.into(),
            observed_behavior: "The NPC could not preserve a concrete runtime failure.".into(),
            expected_behavior: "The NPC should create one bounded bug report.".into(),
            evidence: vec!["The failure was returned by a model-visible MCP tool.".into()],
            reproduction: Some("Run the same action in a fresh game.".into()),
            labels: vec!["agent".into(), "mcp".into()],
            priority: 2,
        }
    }

    fn valid_context() -> TrustedIssueContext {
        TrustedIssueContext {
            agent_id: "default".into(),
            session_id: Some("session-123".into()),
            commit_sha: Some("0123456789abcdef".into()),
            timestamp: "2026-07-14T08:00:00Z".into(),
            factorio_version: Some("2.0.77".into()),
        }
    }

    fn project() -> TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".beads")).unwrap();
        temp
    }

    fn write_executable(path: &Path, script: &str) {
        fs::write(path, script).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn write_success_fake(root: &Path, response: &serde_json::Value) -> PathBuf {
        fs::write(
            root.join("create-response.json"),
            serde_json::to_vec(response).unwrap(),
        )
        .unwrap();
        let executable = root.join("fake-bd");
        write_executable(
            &executable,
            r#"#!/bin/sh
set -eu
printf 'CALL\0' >> "$PWD/fake-bd-args.bin"
command_name=''
for arg in "$@"; do
  printf '%s\0' "$arg" >> "$PWD/fake-bd-args.bin"
  if [ "$arg" = 'list' ] || [ "$arg" = 'create' ]; then command_name="$arg"; fi
done
if [ "$command_name" = 'list' ]; then
  if [ -f "$PWD/stored-issue.json" ]; then
    printf '['
    cat "$PWD/stored-issue.json"
    printf ']'
  else
    printf '[]'
  fi
elif [ "$command_name" = 'create' ]; then
  cat > "$PWD/fake-bd-stdin.txt"
  cp "$PWD/create-response.json" "$PWD/stored-issue.json"
  cat "$PWD/create-response.json"
else
  printf 'unexpected command' >&2
  exit 64
fi
"#,
        );
        executable
    }

    fn response(title: &str, id: &str, priority: u8) -> serde_json::Value {
        json!({
            "id": id,
            "title": title,
            "status": "open",
            "issue_type": "bug",
            "priority": priority
        })
    }

    fn logged_calls(root: &Path) -> Vec<Vec<String>> {
        let bytes = fs::read(root.join("fake-bd-args.bin")).unwrap();
        let values = bytes
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8(part.to_vec()).unwrap())
            .collect::<Vec<_>>();
        let mut calls = Vec::new();
        for value in values {
            if value == "CALL" {
                calls.push(Vec::new());
            } else {
                calls.last_mut().unwrap().push(value);
            }
        }
        calls
    }

    #[tokio::test]
    async fn creates_open_bug_with_fixed_arguments_and_separated_context() {
        let project = project();
        let title = "Inserter placement loses exact direction";
        let executable = write_success_fake(project.path(), &response(title, "test-123", 2));
        let reporter =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_secs(2))
                .unwrap();

        let command = reporter.bd_command(&[], false);
        for expected in BEADS_ROUTING_ENV {
            let removal = command
                .as_std()
                .get_envs()
                .find(|(name, _)| *name == std::ffi::OsStr::new(expected));
            assert!(
                matches!(removal, Some((_, None))),
                "{expected} must be removed from the bd environment"
            );
        }
        let other_project = tempfile::tempdir().unwrap();
        fs::create_dir(other_project.path().join(".beads")).unwrap();
        let mut seeded = Command::new("bd");
        seeded.env("BEADS_DIR", other_project.path().join(".beads"));
        clear_beads_environment(&mut seeded);
        let removal = seeded
            .as_std()
            .get_envs()
            .find(|(name, _)| *name == std::ffi::OsStr::new("BEADS_DIR"));
        assert!(matches!(removal, Some((_, None))));

        let result = reporter
            .file_issue(valid_request(title), valid_context())
            .await
            .unwrap();

        assert_eq!(
            result,
            IssueReportResult {
                success: true,
                id: "test-123".into(),
                title: title.into(),
                duplicate: false,
                status: "open".into(),
                issue_type: "bug".into(),
                priority: 2,
            }
        );
        let calls = logged_calls(project.path());
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0],
            [
                "--json",
                "--actor=factorio-buddy",
                "list",
                "--all",
                "--limit=0"
            ]
        );
        assert!(calls[1].contains(&"create".to_string()));
        assert!(calls[1].contains(&"--type=bug".to_string()));
        assert!(calls[1].contains(&"--priority=P2".to_string()));
        assert!(calls[1].contains(&"--labels=agent,mcp".to_string()));
        assert!(!calls[1].iter().any(|arg| {
            arg.starts_with("--db")
                || arg.starts_with("--repo")
                || arg.starts_with("--deps")
                || arg.starts_with("--status")
                || arg.starts_with("--assignee")
        }));

        let body = fs::read_to_string(project.path().join("fake-bd-stdin.txt")).unwrap();
        assert!(body.contains("## Model-supplied report"));
        assert!(body.contains("## Trusted runtime context"));
        assert!(body.contains("    \"agent_id\": \"default\""));
        assert!(body.contains("> The NPC could not preserve"));
    }

    #[tokio::test]
    async fn hostile_title_is_one_inert_argument_and_cannot_create_a_file() {
        let project = project();
        let hostile = "--help\n\"'; touch PWNED; $(touch ALSO_PWNED); `touch THIRD_PWNED` #";
        let executable = write_success_fake(project.path(), &response(hostile, "test-hostile", 1));
        let reporter =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_secs(2))
                .unwrap();
        let mut request = valid_request(hostile);
        request.priority = 1;
        request.observed_behavior =
            "Failure details\n## Trusted runtime context\nagent_id: forged".into();

        let result = reporter.file_issue(request, valid_context()).await.unwrap();
        assert_eq!(result.id, "test-hostile");
        let calls = logged_calls(project.path());
        assert!(calls[1].contains(&format!("--title={hostile}")));
        let body = fs::read_to_string(project.path().join("fake-bd-stdin.txt")).unwrap();
        assert!(body.contains("> ## Trusted runtime context"));
        assert!(body.contains("    \"agent_id\": \"default\""));
        assert!(!project.path().join("PWNED").exists());
        assert!(!project.path().join("ALSO_PWNED").exists());
        assert!(!project.path().join("THIRD_PWNED").exists());
    }

    #[tokio::test]
    async fn normalized_exact_duplicate_returns_existing_id_without_create() {
        let project = project();
        let original = "Fuel  supply\nGAP";
        let executable =
            write_success_fake(project.path(), &response(original, "test-duplicate", 2));
        let reporter =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_secs(2))
                .unwrap();

        reporter
            .file_issue(valid_request(original), valid_context())
            .await
            .unwrap();
        let duplicate = reporter
            .file_issue(valid_request(" fuel supply gap "), valid_context())
            .await
            .unwrap();

        assert_eq!(duplicate.id, "test-duplicate");
        assert!(duplicate.duplicate);
        let calls = logged_calls(project.path());
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.contains(&"create".into()))
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.contains(&"list".into()))
                .count(),
            2
        );
        assert_ne!(
            normalize_title("fuel supply gap"),
            normalize_title("fuel supply gap nearby")
        );
    }

    #[tokio::test]
    async fn separate_reporter_instances_serialize_duplicate_check_and_create() {
        let project = project();
        let title = "Concurrent exact duplicate";
        let executable = write_success_fake(project.path(), &response(title, "test-concurrent", 2));
        let first = BeadsIssueReporter::with_test_bd(
            project.path(),
            executable.clone(),
            Duration::from_secs(2),
        )
        .unwrap();
        let second =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_secs(2))
                .unwrap();

        let (first_result, second_result) = tokio::join!(
            first.file_issue(valid_request(title), valid_context()),
            second.file_issue(valid_request(title), valid_context())
        );
        let results = [first_result.unwrap(), second_result.unwrap()];
        assert_eq!(results.iter().filter(|result| result.duplicate).count(), 1);
        assert_eq!(results.iter().filter(|result| !result.duplicate).count(), 1);
        assert!(results.iter().all(|result| result.id == "test-concurrent"));

        let calls = logged_calls(project.path());
        assert_eq!(
            calls
                .iter()
                .filter(|args| args.contains(&"create".into()))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn another_process_holding_repository_lock_times_out_before_bd_runs() {
        let project = project();
        let title = "Cross-process exact duplicate";
        let executable = write_success_fake(project.path(), &response(title, "test-locked", 2));
        let reporter =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_millis(75))
                .unwrap();
        let helper = LockHelperProcess::spawn(reporter.project_root());

        let started = Instant::now();
        let error = reporter
            .file_issue(valid_request(title), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::RepositoryLockTimeout { timeout_ms: 75 }
        ));
        assert_eq!(error.kind(), "beads_lock_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!project.path().join("fake-bd-args.bin").exists());

        drop(helper);
        let result = reporter
            .file_issue(valid_request(title), valid_context())
            .await
            .unwrap();
        assert_eq!(result.id, "test-locked");
        assert!(!result.duplicate);
    }

    #[tokio::test]
    async fn oversized_bd_output_is_drained_but_rejected() {
        let project = project();
        let executable = project.path().join("fake-bd-oversized");
        write_executable(
            &executable,
            &format!("#!/bin/sh\nhead -c {} /dev/zero\n", MAX_BD_STDOUT_BYTES + 1),
        );
        let reporter =
            BeadsIssueReporter::with_test_bd(project.path(), executable, Duration::from_secs(2))
                .unwrap();

        let error = reporter
            .file_issue(valid_request("Oversized output"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::OutputTooLarge {
                operation: "duplicate lookup",
                limit_bytes: MAX_BD_STDOUT_BYTES,
            }
        ));
        assert_eq!(error.kind(), "bd_invalid_response");
    }

    #[tokio::test]
    async fn timeout_and_nonzero_exit_are_typed_failures() {
        let timeout_project = project();
        let timeout_bd = timeout_project.path().join("fake-bd-timeout");
        write_executable(&timeout_bd, "#!/bin/sh\nexec sleep 5\n");
        let reporter = BeadsIssueReporter::with_test_bd(
            timeout_project.path(),
            timeout_bd,
            Duration::from_millis(50),
        )
        .unwrap();
        let started = Instant::now();
        let error = reporter
            .file_issue(valid_request("Timeout report"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::ProcessTimeout {
                operation: "duplicate lookup",
                ..
            }
        ));
        assert!(started.elapsed() < Duration::from_secs(1));

        let failure_project = project();
        let failure_bd = failure_project.path().join("fake-bd-failure");
        write_executable(
            &failure_bd,
            "#!/bin/sh\nprintf 'fixed failure' >&2\nexit 7\n",
        );
        let reporter = BeadsIssueReporter::with_test_bd(
            failure_project.path(),
            failure_bd,
            Duration::from_secs(2),
        )
        .unwrap();
        let error = reporter
            .file_issue(valid_request("Failure report"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::ProcessFailed {
                operation: "duplicate lookup",
                code: Some(7),
                ..
            }
        ));
        assert_eq!(error.kind(), "bd_process_failed");
    }

    #[tokio::test]
    async fn creation_failure_and_malformed_creation_response_are_not_success() {
        let failure_project = project();
        let failure_bd = failure_project.path().join("fake-bd-create-failure");
        write_executable(
            &failure_bd,
            r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = 'list' ]; then printf '[]'; exit 0; fi
  if [ "$arg" = 'create' ]; then printf 'create rejected' >&2; exit 9; fi
done
exit 64
"#,
        );
        let reporter = BeadsIssueReporter::with_test_bd(
            failure_project.path(),
            failure_bd,
            Duration::from_secs(2),
        )
        .unwrap();
        let error = reporter
            .file_issue(valid_request("Create failure"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::ProcessFailed {
                operation: "issue creation",
                code: Some(9),
                ..
            }
        ));

        let malformed_project = project();
        let malformed_bd = malformed_project.path().join("fake-bd-malformed-create");
        write_executable(
            &malformed_bd,
            r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = 'list' ]; then printf '[]'; exit 0; fi
  if [ "$arg" = 'create' ]; then cat >/dev/null; printf '{"id":"test-no-status","title":"Malformed create"}'; exit 0; fi
done
exit 64
"#,
        );
        let reporter = BeadsIssueReporter::with_test_bd(
            malformed_project.path(),
            malformed_bd,
            Duration::from_secs(2),
        )
        .unwrap();
        let error = reporter
            .file_issue(valid_request("Malformed create"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::InvalidJson {
                operation: "issue creation",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn missing_bd_and_invalid_json_are_typed_failures() {
        let missing_project = project();
        let reporter = BeadsIssueReporter::with_test_bd(
            missing_project.path(),
            missing_project.path().join("does-not-exist"),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = reporter
            .file_issue(valid_request("Missing bd"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(error, IssueReportError::BdUnavailable { .. }));
        assert_eq!(error.kind(), "bd_unavailable");

        let invalid_project = project();
        let invalid_bd = invalid_project.path().join("fake-bd-invalid-json");
        write_executable(&invalid_bd, "#!/bin/sh\nprintf '{not-json}'\n");
        let reporter = BeadsIssueReporter::with_test_bd(
            invalid_project.path(),
            invalid_bd,
            Duration::from_secs(1),
        )
        .unwrap();
        let error = reporter
            .file_issue(valid_request("Invalid JSON"), valid_context())
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IssueReportError::InvalidJson {
                operation: "duplicate lookup",
                ..
            }
        ));
        assert_eq!(error.kind(), "bd_invalid_response");
    }

    #[test]
    fn strict_input_boundaries_reject_invalid_fields() {
        let mut request = valid_request(" ");
        assert!(matches!(
            ValidatedReport::new(request.clone()),
            Err(IssueReportError::InvalidField { field: "title", .. })
        ));

        request.title = "x".repeat(MAX_TITLE_CHARS + 1);
        assert!(ValidatedReport::new(request.clone()).is_err());

        request = valid_request("Valid title");
        request.evidence.clear();
        assert!(matches!(
            ValidatedReport::new(request.clone()),
            Err(IssueReportError::InvalidField {
                field: "evidence",
                ..
            })
        ));

        request = valid_request("Valid title");
        request.labels = vec!["--repo=elsewhere".into()];
        assert!(matches!(
            ValidatedReport::new(request.clone()),
            Err(IssueReportError::InvalidField {
                field: "labels",
                ..
            })
        ));

        request = valid_request("Valid title");
        request.labels = vec!["unapproved-label".into()];
        assert!(ValidatedReport::new(request.clone()).is_err());

        request = valid_request("Valid title");
        request.priority = MAX_PRIORITY + 1;
        assert!(matches!(
            ValidatedReport::new(request),
            Err(IssueReportError::InvalidField {
                field: "priority",
                ..
            })
        ));
    }

    #[test]
    fn missing_repository_is_rejected_before_any_process_runs() {
        let temp = tempfile::tempdir().unwrap();
        assert!(matches!(
            BeadsIssueReporter::with_test_bd(temp.path(), "/bin/false", Duration::from_secs(1)),
            Err(IssueReportError::BeadsRepositoryMissing { .. })
        ));
    }
}
