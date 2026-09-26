//! Standalone Factorio buddy runtime.
//!
//! This is intentionally a thin host around the Rust MCP server: it watches the
//! mod's chat inbox, gives Claude only the Factorio MCP tools, and sends the
//! final response back to the mod. Gameplay policy remains in the model and
//! gameplay implementation remains in Rust/Lua; there is no second planner or
//! memory system here.

#[path = "buddy/decision.rs"]
mod decision;
#[path = "buddy/mcp_client.rs"]
mod mcp_client;

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use decision::{DecisionMode, JevClient, Preview, Selection};

use anyhow::{bail, Context, Result};
use clap::Parser;
use factorioctl::client::{AgentId, FactorioClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout, Instant, MissedTickBehavior};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_SYSTEM_PROMPT: &str = "You are an autonomous AI teammate inside a Factorio game. Use the Factorio MCP tools to observe and play the game through your own character. Act on player requests immediately. When idle, inspect the real game state and make concrete progress toward a functioning automated factory. Prioritize self-sustaining automation: build production chains that continuously gather, transport, process, and deliver resources without your character manually moving items. Use hand-crafting and manual item transfers only for bounded bootstrap or recovery (bounded hand-fuelling of burner machines is part of the bootstrap until electricity and research run), then replace them with automated production; never treat hand-feeding as completion. Build belts as complete source-to-destination routes with route_belt or a higher-level automation controller; do not improvise disconnected one-tile belt fragments. Treat live resource patches as future extraction capacity, not forbidden terrain. Resource overlap is advisory, not a placement veto: prefer clear land for large permanent processing, storage, or power blocks when practical, but temporary bootstrap structures and compact transport, power, or fluid connections may cross or occupy resource tiles. Do not refuse useful automation or build a wasteful detour solely because an otherwise valid placement touches ore. Only extraction machinery is resource-category constrained; place mining drills or pumpjacks only where they are compatible. Use execute_edge_miner to derive a workable drill output, and accept a Factorio-buildable output tile even when it contains ore. Prefer dedicated item belts or deliberate lane separation; never assume a branch is pure because one sampled tile currently shows one item. Before tapping any belt that may carry multiple products, inspect its exact lanes; configure the receiving inserter's whitelist when one consumer must accept only specific items, but do not mistake a filtered inserter for a pure upstream belt. Treat planner output as an executable contract: when a plan returns exact mutation arguments, execute those exact arguments without substituting a search or approximate mutation. After a compound mutation, inspect the resulting state and correct or remove failed partial work before proceeding. Never claim an action succeeded unless a tool result confirms it. Keep final chat replies concise because they render in a small in-game panel. Your Factorio tools are exposed with the mcp__factorio__ prefix (for example mcp__factorio__situation_report); snapshots and tool results name them without the prefix, so always call the prefixed name.";

const AUTONOMY_DIRECTIVE: &str = "Autonomy tick: re-evaluate the factory from the authoritative snapshot below before acting; choose from current evidence, not from the previous turn's focus. snapshot.progression is the code-computed tech ladder (Factorio 2.0 trigger tree: 50 iron plates unlock steam power, 10 copper plates unlock electronics, crafting a lab unlocks red science). Unless a player request or a progression warning comes first, spend this turn closing progression.next_goal, using progression.how as the starting method. Reaching electricity and running research beats perfecting burner-era logistics: bounded hand-fuelling with bootstrap_burner_once (up to 50) and hand-crafting are correct until research runs, so do not build belt fuel feeds for burner machines before then. The long-term goal is launching a rocket; progression.rocket_path lists the remaining technologies and which science packs still lack assemblers. Prefer one-call controllers over long chains of place_entity calls: build_steam_power for power, and build_layout for any block you design yourself (smelter rows, science blocks, later oil and the silo). Stay near the existing smelters unless a needed resource is missing there. If a subsystem is healthy, leave it running; do not poll it or wait on it. Queue research only from progression.rocket_path.researchable_now unless a technology is needed for a concrete build now, and keep at least 3 technologies queued so the labs never idle while you are away, unless a progression warning says every path technology waits on an unmade pack: then make that pack instead of queueing other research. Turns are cut after 300 s and interrupted tools become outcome-unknown, so end the turn after about 25 tool calls or once next_goal is done; the next autonomy tick continues. Take concrete action and verify the result; do not merely describe a plan.";

// The managed server is local and returning players replace stale peers. Keep
// a temporarily starved background client connected instead of dropping it at
// Factorio's 20-second default.
const MANAGED_CLIENT_DROP_THRESHOLD_SECONDS: u64 = 86_400;
const PROVIDER_LIMIT_RETRY_SECONDS: u64 = 300;
const DEFAULT_MAP_SEED: u32 = 2_590_060_468;
const PROVIDER_LIMIT_MESSAGE: &str =
    "Claude is temporarily unavailable because the subscription usage limit was reached. Buddy will retry automatically; your Factorio game is still running.";

#[derive(Clone, Debug, Parser)]
#[command(about = "Run the autonomous Factorio buddy using the Rust MCP tool server")]
struct Args {
    #[arg(long, default_value = "default", env = "FACTORIO_AGENT_ID")]
    agent: String,

    #[arg(long)]
    label: Option<String>,

    #[arg(long, env = "MODEL", default_value = "claude-opus-5-5")]
    model: Option<String>,

    #[arg(
        long,
        default_value = "low",
        env = "BUDDY_EFFORT",
        value_parser = ["low", "medium", "high", "xhigh", "max"]
    )]
    effort: String,

    #[arg(long, default_value = "localhost", env = "FACTORIO_RCON_HOST")]
    rcon_host: String,

    #[arg(long, default_value_t = 27015, env = "FACTORIO_RCON_PORT")]
    rcon_port: u16,

    #[arg(long, default_value_t = 34197, env = "FACTORIO_GAME_PORT")]
    game_port: u16,

    #[arg(long, env = "FACTORIO_RCON_PASSWORD")]
    rcon_password: Option<String>,

    #[arg(long, env = "FACTORIO_SCRIPT_OUTPUT")]
    script_output: Option<PathBuf>,

    /// Start and own a local headless Factorio server before starting the NPC.
    #[arg(long)]
    start_server: bool,

    /// Recreate the local save before starting the server.
    #[arg(long, requires = "start_server")]
    fresh: bool,

    /// Map seed used when creating a new managed save. Ignored when resuming one.
    #[arg(long, default_value_t = DEFAULT_MAP_SEED, env = "FACTORIO_MAP_SEED")]
    map_seed: u32,

    #[arg(long, env = "FACTORIO_BIN")]
    factorio_bin: Option<PathBuf>,

    #[arg(long, default_value = ".factorio-buddy", env = "FACTORIO_WRITE_DATA")]
    write_data: PathBuf,

    #[arg(long)]
    save: Option<PathBuf>,

    #[arg(long, env = "FACTORIOCTL_MCP")]
    mcp_bin: Option<PathBuf>,

    /// Seconds between autonomous turns. Set to 0 for chat-only operation.
    #[arg(long, default_value_t = 30, env = "BUDDY_HEARTBEAT_SECONDS")]
    heartbeat_seconds: u64,

    /// Optional whole-turn timeout. Zero leaves a progressing turn uncapped;
    /// player input and shutdown can still cancel it immediately.
    #[arg(long, default_value_t = 0, env = "BUDDY_TURN_TIMEOUT_SECONDS")]
    turn_timeout_seconds: u64,

    #[arg(long, default_value = DEFAULT_SYSTEM_PROMPT)]
    system_prompt: String,

    /// Additional operator-defined temperament appended to the built-in gameplay rules.
    #[arg(long, env = "BUDDY_PERSONA")]
    persona: Option<String>,

    /// Project root whose Beads tracker receives `file_issue` calls from the
    /// MCP server. Trials point this at a disposable tracker.
    #[arg(long, env = "BUDDY_ISSUE_PROJECT_ROOT", default_value = env!("CARGO_MANIFEST_DIR"))]
    issue_project_root: PathBuf,

    /// Opt-in maintenance decision experiment before autonomous turns.
    #[arg(long, env = "BUDDY_DECISION_MODE", value_enum, default_value = "off")]
    decision_mode: DecisionMode,

    /// Stop starting autonomous/maintenance turns after this many autonomous
    /// turns (0 = unlimited). Human turns are unaffected.
    #[arg(long, default_value_t = 0, env = "BUDDY_MAX_AUTONOMOUS_TURNS")]
    max_autonomous_turns: u64,

    /// Stop autonomy this many seconds after Buddy comes online (0 = unlimited).
    /// An autonomous turn in flight at the deadline is cancelled.
    #[arg(long, default_value_t = 0, env = "BUDDY_AUTONOMY_DEADLINE_SECONDS")]
    autonomy_deadline_seconds: u64,

    /// Append-only JSONL evidence log of turns, tool outcomes and decisions.
    #[arg(long, env = "BUDDY_EVIDENCE_LOG")]
    evidence_log: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
struct InputMessage {
    #[serde(default)]
    id: Option<u64>,
    message: String,
    #[serde(default = "default_player_index")]
    player_index: u32,
    #[serde(default = "default_agent")]
    target_agent: String,
    response_to: Option<String>,
}

fn default_player_index() -> u32 {
    1
}
fn default_agent() -> String {
    "default".to_owned()
}

#[derive(Debug, Deserialize)]
struct ClaudeResult {
    #[serde(default)]
    result: String,
    session_id: Option<String>,
    #[serde(default)]
    is_error: bool,
    /// HTTP status of the provider error that ended the invocation, if any.
    #[serde(default)]
    api_error_status: Option<u16>,
}

impl ClaudeResult {
    /// A terminal provider refusal (subscription window, weekly cap, or an
    /// invocation that ended on HTTP 429). Retrying immediately only burns
    /// turns, so these back off like the documented usage limit.
    fn is_provider_limit(&self) -> bool {
        self.is_error
            && (self.api_error_status == Some(429) || is_provider_usage_limit(&self.result))
    }
}

struct ClaudeReply {
    text: String,
    already_delivered: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("Claude subscription usage limit reached")]
struct ClaudeUsageLimit;

struct Inbox {
    path: PathBuf,
    cursor_path: PathBuf,
    offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnKind {
    Human,
    Autonomy,
    /// Runtime maintenance controller (decision experiment); never a Claude turn.
    Maintenance,
}

impl TurnKind {
    fn evidence_name(self) -> &'static str {
        match self {
            TurnKind::Human => "human",
            TurnKind::Autonomy => "autonomy",
            TurnKind::Maintenance => "maintenance",
        }
    }
}

#[derive(Clone, Debug)]
struct TurnRequest {
    kind: TurnKind,
    prompt: Option<String>,
    player_index: u32,
    response_agent: String,
}

struct TurnCompletion {
    session_id: Option<String>,
    /// Model turns: whether Claude produced a result. Maintenance turns: the
    /// executed repair's outcome, `None` when no repair executed.
    succeeded: Option<bool>,
    provider_limited: bool,
    /// Maintenance returned control: start an Opus autonomy turn next.
    follow_with_opus: bool,
}

struct ActiveTurn {
    kind: TurnKind,
    started: Instant,
    handle: JoinHandle<TurnCompletion>,
}

const RECENT_OUTCOME_LIMIT: usize = 8;
const REPEATED_FAILURE_THRESHOLD: usize = 3;
const STDERR_TAIL_LINES: usize = 20;
const STDERR_TAIL_BYTES: usize = 4096;

/// Append-only JSONL evidence log. Absent path = disabled.
struct EvidenceLog(Option<StdMutex<File>>);

impl EvidenceLog {
    fn open(path: Option<&Path>) -> Result<Self> {
        let Some(path) = path else {
            return Ok(Self(None));
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("failed to open evidence log {}", path.display()))?;
        Ok(Self(Some(StdMutex::new(file))))
    }

    fn record(&self, event: &str, fields: Value) {
        let Some(file) = &self.0 else {
            return;
        };
        let mut object = match fields {
            Value::Object(object) => object,
            _ => serde_json::Map::new(),
        };
        object.insert("event".to_owned(), json!(event));
        let unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);
        object.insert("unix_ms".to_owned(), json!(unix_ms));
        let Ok(mut line) = serde_json::to_vec(&Value::Object(object)) else {
            return;
        };
        line.push(b'\n');
        if let Ok(mut file) = file.lock() {
            if let Err(error) = file.write_all(&line).and_then(|_| file.flush()) {
                warn!(%error, "failed to append evidence log");
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ToolOutcome {
    tool: String,
    arguments: Value,
    is_error: bool,
    error: Option<String>,
    tick: Option<u64>,
}

impl ToolOutcome {
    fn same_failure(&self, other: &ToolOutcome) -> bool {
        self.is_error
            && other.is_error
            && self.tool == other.tool
            && self.arguments == other.arguments
            && self.error == other.error
    }
}

/// Tools that move items by hand instead of by automation.
const MANUAL_TRANSFER_TOOLS: &[&str] = &[
    "bootstrap_burner_once",
    "bootstrap_smelting_once",
    "collect_from_chest",
    "refuel_burners",
    "feed_lab_from_inventory",
];
/// Window of recent calls over which the manual-transfer share is measured.
const MANUAL_TRANSFER_WINDOW: usize = 60;
/// Share of that window (in percent) at which the planner is told the factory
/// is running on hand logistics.
const MANUAL_TRANSFER_ALERT_PERCENT: usize = 30;

/// Bounded session memory of recent tool outcomes (diagnostic feedback only).
#[derive(Default)]
struct RecentOutcomes {
    entries: VecDeque<ToolOutcome>,
    /// Whether each of the last `MANUAL_TRANSFER_WINDOW` calls was a
    /// successful hand transfer.
    manual_window: VecDeque<bool>,
}

impl RecentOutcomes {
    /// Record an outcome; returns the identical-failure count when it reaches
    /// the repeated-failure threshold.
    fn push(&mut self, outcome: ToolOutcome) -> Option<usize> {
        self.manual_window
            .push_back(!outcome.is_error && MANUAL_TRANSFER_TOOLS.contains(&outcome.tool.as_str()));
        while self.manual_window.len() > MANUAL_TRANSFER_WINDOW {
            self.manual_window.pop_front();
        }
        self.entries.push_back(outcome);
        while self.entries.len() > RECENT_OUTCOME_LIMIT {
            self.entries.pop_front();
        }
        let streak = self.failure_streak();
        (streak >= REPEATED_FAILURE_THRESHOLD).then_some(streak)
    }

    /// Hand transfers among the last full window of calls, when they reach
    /// the alert share.
    fn manual_transfer_pressure(&self) -> Option<usize> {
        let manual = self.manual_window.iter().filter(|manual| **manual).count();
        (self.manual_window.len() == MANUAL_TRANSFER_WINDOW
            && manual * 100 >= MANUAL_TRANSFER_ALERT_PERCENT * MANUAL_TRANSFER_WINDOW)
            .then_some(manual)
    }

    /// Identical (tool, arguments, error) failures of the latest outcome in the
    /// bounded window since the last success of the same tool and arguments.
    /// Other calls in between (for example read-only inspections) do not reset
    /// the count: fail/inspect/fail/inspect/fail is still a repeated failure.
    fn failure_streak(&self) -> usize {
        self.entries
            .back()
            .filter(|last| last.is_error)
            .map_or(0, |last| self.failure_count(last))
    }

    fn failure_count(&self, target: &ToolOutcome) -> usize {
        self.entries
            .iter()
            .rev()
            .take_while(|entry| {
                entry.is_error || entry.tool != target.tool || entry.arguments != target.arguments
            })
            .filter(|entry| entry.same_failure(target))
            .count()
    }

    /// Every distinct failure that currently reaches the threshold.
    fn repeated_failures(&self) -> Vec<(&ToolOutcome, usize)> {
        let mut repeated: Vec<(&ToolOutcome, usize)> = Vec::new();
        for entry in self.entries.iter().rev().filter(|entry| entry.is_error) {
            if repeated.iter().any(|(seen, _)| seen.same_failure(entry)) {
                continue;
            }
            let count = self.failure_count(entry);
            if count >= REPEATED_FAILURE_THRESHOLD {
                repeated.push((entry, count));
            }
        }
        repeated
    }

    fn summary(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let mut text = String::from(
            "Recent tool outcomes (machine-generated session memory, oldest first; diagnostic feedback, not a ban):\n",
        );
        for entry in &self.entries {
            let result = match (&entry.is_error, &entry.error) {
                (false, _) => "ok".to_owned(),
                (true, Some(error)) => format!("FAILED: {error}"),
                (true, None) => "FAILED".to_owned(),
            };
            let tick = entry
                .tick
                .map(|tick| format!(" (tick {tick})"))
                .unwrap_or_default();
            text.push_str(&format!(
                "- {} {} -> {result}{tick}\n",
                entry.tool, entry.arguments
            ));
        }
        for (failure, count) in self.repeated_failures() {
            text.push_str(&format!(
                "REPEATED FAILURE: {} {} failed {count} times in the recent window with the same error and no success of the same call since. Do not repeat it unchanged. Inspect the current world state to find why it fails, then replan with a different action or arguments.\n",
                failure.tool, failure.arguments
            ));
        }
        if let Some(manual) = self.manual_transfer_pressure() {
            text.push_str(&format!(
                "MANUAL LOGISTICS: {manual} of the last {MANUAL_TRANSFER_WINDOW} tool calls moved fuel, plates or science packs by hand. The factory depends on you and stops when you do. Automate the most repeated transfer this turn (for example a coal lane feeding the furnaces and boiler, or plate belts into assemblers, with build_layout or route_belt).\n"
            ));
        }
        Some(text)
    }
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_json(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        other => other.clone(),
    }
}

fn short_tool_name(name: &str) -> &str {
    name.strip_prefix("mcp__factorio__").unwrap_or(name)
}

fn tool_result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn bounded(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Classify a tool result with the MCP server's semantics: the `isError` flag,
/// an `Error:` text reply, or a JSON reply with `success:false` or a nonempty
/// `error`. Returns (is_error, error, tick).
fn classify_tool_result(flagged_error: bool, text: &str) -> (bool, Option<String>, Option<u64>) {
    let trimmed = text.trim();
    let parsed = serde_json::from_str::<Value>(trimmed).ok();
    let tick = parsed
        .as_ref()
        .and_then(|value| value.get("tick"))
        .and_then(Value::as_u64);
    let mut error = None;
    if let Some(Value::Object(object)) = &parsed {
        let error_field = object.get("error").and_then(|value| match value {
            Value::Null => None,
            Value::String(text) if text.trim().is_empty() => None,
            Value::String(text) => Some(bounded(text, 200)),
            Value::Bool(false) => None,
            other => Some(bounded(&other.to_string(), 200)),
        });
        if error_field.is_some() {
            error = error_field;
        } else if object.get("success").and_then(Value::as_bool) == Some(false) {
            error = Some(
                object
                    .get("error_kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| "success=false".to_owned()),
            );
        }
    } else if let Some(rest) = trimmed.strip_prefix("Error:") {
        error = Some(bounded(rest.lines().next().unwrap_or(""), 200));
    }
    if flagged_error && error.is_none() {
        error = Some(bounded(trimmed, 200));
    }
    (error.is_some(), error, tick)
}

#[derive(Default)]
struct RuntimeState {
    /// tool_use id -> (tool, canonical arguments) without a result yet.
    in_flight: HashMap<String, (String, Value)>,
    outcomes: RecentOutcomes,
    /// Tools whose outcome is unknown because their turn was interrupted.
    unknown_outcomes: Vec<(String, Value)>,
    turn_tool_calls: u64,
    turn_tool_errors: u64,
    /// Process group of the currently owned Claude or MCP child tree.
    process_group: Option<i32>,
}

/// Runtime state shared between the control loop and turn tasks.
struct Shared {
    state: StdMutex<RuntimeState>,
    evidence: EvidenceLog,
}

impl Shared {
    fn new(evidence: EvidenceLog) -> Self {
        Self {
            state: StdMutex::new(RuntimeState::default()),
            evidence,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn tool_started(&self, id: &str, tool: &str, arguments: &Value) {
        let mut state = self.lock();
        state.turn_tool_calls += 1;
        state.in_flight.insert(
            id.to_owned(),
            (short_tool_name(tool).to_owned(), canonical_json(arguments)),
        );
    }

    /// Pair a tool result with its call, classify it, record it in the
    /// bounded outcome memory, and emit evidence.
    fn tool_finished(&self, id: &str, flagged_error: bool, text: &str) {
        let (is_error, error, tick) = classify_tool_result(flagged_error, text);
        let mut state = self.lock();
        let Some((tool, arguments)) = state.in_flight.remove(id) else {
            return;
        };
        if is_error {
            state.turn_tool_errors += 1;
        }
        let outcome = ToolOutcome {
            tool,
            arguments,
            is_error,
            error,
            tick,
        };
        let mut fields = json!({
            "tool": outcome.tool,
            "arguments": outcome.arguments,
            "is_error": outcome.is_error,
            "error": outcome.error,
        });
        if let Some(tick) = outcome.tick {
            fields["tick"] = json!(tick);
        }
        let hint = state.outcomes.push(outcome.clone());
        drop(state);
        self.evidence.record("tool_outcome", fields);
        if let Some(count) = hint {
            warn!(event = "repeated_failure_hint", tool = %outcome.tool, count, "identical tool failure repeated");
            self.evidence.record(
                "repeated_failure_hint",
                json!({
                    "tool": outcome.tool,
                    "arguments": outcome.arguments,
                    "error": outcome.error,
                    "count": count,
                }),
            );
        }
    }

    fn begin_turn(&self) {
        let mut state = self.lock();
        state.turn_tool_calls = 0;
        state.turn_tool_errors = 0;
    }

    fn turn_counts(&self) -> (u64, u64) {
        let state = self.lock();
        (state.turn_tool_calls, state.turn_tool_errors)
    }

    fn outcome_summary(&self) -> Option<String> {
        self.lock().outcomes.summary()
    }

    /// Build the outcome-unknown note without consuming it. The entries stay
    /// recorded until `acknowledge_unknown_outcomes` confirms delivery, so a
    /// turn cancelled before Claude starts leaves the note for the next turn.
    fn peek_unknown_outcome_note(&self) -> Option<(String, Vec<(String, Value)>)> {
        let unknown = self.lock().unknown_outcomes.clone();
        if unknown.is_empty() {
            return None;
        }
        let mut note = String::from(
            "OUTCOME UNKNOWN: the previous turn was interrupted while these tool calls were in flight:\n",
        );
        for (tool, arguments) in &unknown {
            note.push_str(&format!("- {tool} {arguments}\n"));
        }
        note.push_str("They may have fully, partially, or not executed; nothing was rolled back. Before acting, take a fresh snapshot and inspect the affected entities and your inventory. Do not blindly retry these calls.\n");
        Some((note, unknown))
    }

    /// Remove delivered outcome-unknown entries (one record per delivered
    /// entry); entries recorded after the note was built are kept.
    fn acknowledge_unknown_outcomes(&self, delivered: &[(String, Value)]) {
        let mut state = self.lock();
        for call in delivered {
            if let Some(index) = state
                .unknown_outcomes
                .iter()
                .position(|entry| entry == call)
            {
                state.unknown_outcomes.remove(index);
            }
        }
    }

    /// Terminate the owned child process tree (if any) and convert every tool
    /// call still lacking a result into an outcome-unknown record.
    ///
    /// The process group stays recorded until termination has finished, so a
    /// reap aborted mid-wait (turn cancellation) is redone by the next reap
    /// instead of being skipped.
    async fn reap_and_reconcile(&self) {
        let group = self.lock().process_group;
        if let Some(pgid) = group {
            terminate_process_group(pgid).await;
            let mut state = self.lock();
            if state.process_group == Some(pgid) {
                state.process_group = None;
            }
        }
        let interrupted: Vec<(String, Value)> = {
            let mut state = self.lock();
            let drained: Vec<_> = state.in_flight.drain().map(|(_, call)| call).collect();
            state.unknown_outcomes.extend(drained.iter().cloned());
            drained
        };
        for (tool, arguments) in interrupted {
            warn!(event = "interrupted_tool", tool = %tool, arguments = %arguments, "tool outcome unknown after interruption");
            self.evidence.record(
                "interrupted_tool",
                json!({"tool": tool, "arguments": arguments}),
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn process_group_alive(pgid: i32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            return false;
        };
        // Fields after the parenthesized command: state ppid pgrp ...
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            return false;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next();
        let _ppid = fields.next();
        let pgrp = fields.next().and_then(|value| value.parse::<i32>().ok());
        pgrp == Some(pgid) && state != Some("Z") && state != Some("X")
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_group_alive(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// `process_group_alive` scans /proc synchronously; run it on the blocking
/// pool so polling never stalls a runtime worker.
#[cfg(unix)]
async fn group_alive(pgid: i32) -> bool {
    tokio::task::spawn_blocking(move || process_group_alive(pgid))
        .await
        .unwrap_or(true)
}

/// SIGTERM an owned process group, escalate to SIGKILL, and wait until no
/// live member remains, so no stale Claude/MCP process can keep mutating the
/// world once a new turn starts.
///
/// PGID reuse: while any member of the group is alive Linux cannot hand the
/// group's id to a new process, so signalling a live group is safe. Callers
/// often reap the leader first (Claude's `child.wait()`, rmcp's shutdown), so
/// the group is checked before the first signal and never signalled once it
/// is observed empty. The remaining window between that scan and `kill` would
/// need a full PID wrap-around to hit a reused id; keeping the leader
/// unreaped would close it but is not feasible with rmcp's owned child.
async fn terminate_process_group(pgid: i32) {
    #[cfg(unix)]
    {
        if pgid <= 1 {
            return;
        }
        if !group_alive(pgid).await {
            info!(
                event = "process_group_reaped",
                pgid, "owned child process group already gone"
            );
            return;
        }
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(3);
        while group_alive(pgid).await && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if group_alive(pgid).await {
            warn!(pgid, "owned process group ignored SIGTERM; sending SIGKILL");
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            let deadline = Instant::now() + Duration::from_secs(5);
            while group_alive(pgid).await && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        if group_alive(pgid).await {
            warn!(
                pgid,
                "owned process group still has live members after SIGKILL"
            );
        } else {
            info!(
                event = "process_group_reaped",
                pgid, "owned child process group is gone"
            );
        }
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

struct LifecycleClient {
    host: String,
    port: u16,
    password: String,
    agent: String,
    client: Mutex<Option<FactorioClient>>,
}

struct ControllerLease {
    file: File,
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SaveIdentity {
    size: u64,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SaveOwner {
    version: u32,
    primary_save: String,
    primary_identity: SaveIdentity,
    run_id: String,
    run_directory: String,
    clean_shutdown: bool,
}

struct LocalServer {
    child: Child,
    // Factorio exits when stdin reaches EOF, so retain the pipe for the life of
    // the server even though the buddy never writes console commands to it.
    _stdin: tokio::process::ChildStdin,
    output_task: JoinHandle<()>,
    save_owner_path: PathBuf,
    save_owner: SaveOwner,
}

impl LocalServer {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("failed to inspect Factorio server process")
    }

    async fn stop(mut self) {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                warn!(%status, "Factorio server had already exited");
                let _ = self.output_task.await;
                return;
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "failed to inspect Factorio server before shutdown"),
        }

        info!("requesting Factorio shutdown and final save");
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The server runs in its own process group, so terminal Ctrl-C only
            // reaches Buddy. Deliver one SIGINT here and then give Factorio time
            // to finish its normal save-before-exit path.
            let result = unsafe { libc::kill(pid as i32, libc::SIGINT) };
            if result != 0 {
                warn!(error = %std::io::Error::last_os_error(), pid, "failed to interrupt Factorio server");
            }
        }
        #[cfg(not(unix))]
        let _ = self.child.start_kill();

        let clean_shutdown = match timeout(Duration::from_secs(60), self.child.wait()).await {
            Ok(Ok(status)) => {
                info!(%status, "Factorio server stopped after final save");
                status.success()
            }
            Ok(Err(error)) => {
                warn!(%error, "failed while waiting for Factorio server to stop");
                false
            }
            Err(_) => {
                warn!("Factorio did not stop within 60 seconds; forcing shutdown");
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
                false
            }
        };
        if clean_shutdown {
            self.save_owner.clean_shutdown = true;
            if let Err(error) = write_json_atomic(&self.save_owner_path, &self.save_owner, false) {
                warn!(%error, "failed to record clean Factorio shutdown");
            }
        }
        let _ = self.output_task.await;
    }
}

impl ControllerLease {
    fn acquire(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open controller lease {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                bail!(
                    "another Buddy controller already owns agent lease {}: {error}",
                    path.display()
                );
            }
        }

        #[cfg(not(unix))]
        if file.metadata()?.len() != 0 {
            bail!(
                "another Buddy controller may already own agent lease {}",
                path.display()
            );
        }

        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_all()?;
        Ok(Self { file, path })
    }
}

impl Drop for ControllerLease {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(not(unix))]
        {
            let _ = self.file.set_len(0);
        }
        let _ = &self.path;
    }
}

fn atomic_write(path: &Path, contents: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .context("atomic write destination has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let staged = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    if staged.exists() {
        std::fs::remove_file(&staged)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    let mut file = options.open(&staged)?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T, private: bool) -> Result<()> {
    let mut encoded = serde_json::to_vec_pretty(value)?;
    encoded.push(b'\n');
    atomic_write(path, &encoded, private)
}

fn password_path(write_data: &Path) -> PathBuf {
    write_data.join("rcon-password")
}

fn generate_password() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("failed to generate RCON password: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn configure_rcon_password(args: &mut Args) -> Result<()> {
    std::fs::create_dir_all(&args.write_data)?;
    let path = password_path(&args.write_data);

    if args.start_server {
        if !matches!(args.rcon_host.as_str(), "localhost" | "127.0.0.1" | "::1") {
            bail!(
                "an owned Factorio server must use loopback RCON, not {}",
                args.rcon_host
            );
        }
        args.rcon_host = "127.0.0.1".to_owned();
        let password = match args.rcon_password.take() {
            Some(password) if !password.trim().is_empty() => password,
            Some(_) => bail!("RCON password cannot be empty"),
            None => match std::fs::read_to_string(&path) {
                Ok(password) if !password.trim().is_empty() => password.trim().to_owned(),
                _ => generate_password()?,
            },
        };
        atomic_write(&path, format!("{password}\n").as_bytes(), true)?;
        args.rcon_password = Some(password);
        return Ok(());
    }

    if args
        .rcon_password
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        bail!("RCON password cannot be empty");
    }
    if args.rcon_password.is_none() {
        let password = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no RCON password supplied; set FACTORIO_RCON_PASSWORD or start the managed server once (missing {})",
                path.display()
            )
        })?;
        if password.trim().is_empty() {
            bail!("managed RCON password file is empty: {}", path.display());
        }
        args.rcon_password = Some(password.trim().to_owned());
    }
    Ok(())
}

fn rcon_password(args: &Args) -> Result<&str> {
    args.rcon_password
        .as_deref()
        .context("RCON password was not configured")
}

fn should_forward_factorio_output(line: &str) -> bool {
    !line.contains("New RCON connection")
}

async fn forward_factorio_output(reader: impl AsyncRead + Unpin) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) if should_forward_factorio_output(&line) => println!("{line}"),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(error) => {
                warn!(%error, "failed to read Factorio server output");
                break;
            }
        }
    }
}

impl Inbox {
    fn new(path: PathBuf, cursor_path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file_len = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let offset = match std::fs::read_to_string(&cursor_path) {
            Ok(cursor) => {
                let stored = cursor
                    .trim()
                    .parse::<u64>()
                    .with_context(|| format!("invalid inbox cursor {}", cursor_path.display()))?;
                if stored > file_len {
                    0
                } else {
                    stored
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        let inbox = Self {
            path,
            cursor_path,
            offset,
        };
        inbox.persist_cursor()?;
        Ok(inbox)
    }

    fn persist_cursor(&self) -> Result<()> {
        atomic_write(
            &self.cursor_path,
            format!("{}\n", self.offset).as_bytes(),
            false,
        )
    }

    fn poll(&mut self) -> Result<Vec<InputMessage>> {
        let Ok(metadata) = std::fs::metadata(&self.path) else {
            return Ok(Vec::new());
        };
        if metadata.len() < self.offset {
            self.offset = 0;
            self.persist_cursor()?;
        }
        if metadata.len() == self.offset {
            return Ok(Vec::new());
        }
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)?;
        let Some(last_newline) = chunk.iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let complete = String::from_utf8_lossy(&chunk[..=last_newline]);
        self.offset += (last_newline + 1) as u64;
        self.persist_cursor()?;
        Ok(parse_input(&complete))
    }
}

fn parse_input(chunk: &str) -> Vec<InputMessage> {
    chunk
        .lines()
        .filter_map(|line| {
            let message: InputMessage = match serde_json::from_str(line) {
                Ok(message) => message,
                Err(error) => {
                    warn!(%error, line, "ignored malformed chat inbox record");
                    return None;
                }
            };
            (!message.message.trim().is_empty()).then_some(message)
        })
        .collect()
}

fn find_mcp(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .with_context(|| format!("MCP binary not found: {}", path.display()));
    }
    let current = std::env::current_exe()?;
    let sibling = current.with_file_name(if cfg!(windows) { "mcp.exe" } else { "mcp" });
    if sibling.is_file() {
        return Ok(sibling);
    }
    which::which("factorioctl-mcp")
        .or_else(|_| which::which("mcp"))
        .context("factorioctl MCP binary not found; build with `cargo build --release`")
}

fn default_script_output() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".factorio-buddy/script-output")
}

fn find_factorio(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .with_context(|| format!("Factorio binary not found: {}", path.display()));
    }
    if let Ok(path) = which::which("factorio") {
        return Ok(path);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    [
        PathBuf::from("/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio"),
        home.join(".local/share/Steam/steamapps/common/Factorio/bin/x64/factorio"),
        home.join(".steam/steam/steamapps/common/Factorio/bin/x64/factorio"),
        PathBuf::from("/opt/factorio/bin/x64/factorio"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .context("Factorio binary not found; set FACTORIO_BIN=/path/to/factorio")
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn trees_equal(left: &Path, right: &Path) -> Result<bool> {
    if !left.is_dir() || !right.is_dir() {
        return Ok(false);
    }
    let mut left_entries = std::fs::read_dir(left)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut right_entries = std::fs::read_dir(right)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    left_entries.sort();
    right_entries.sort();
    if left_entries != right_entries {
        return Ok(false);
    }
    for name in left_entries {
        let left_path = left.join(&name);
        let right_path = right.join(&name);
        let left_type = std::fs::symlink_metadata(&left_path)?.file_type();
        let right_type = std::fs::symlink_metadata(&right_path)?.file_type();
        if left_type.is_dir() != right_type.is_dir() || left_type.is_file() != right_type.is_file()
        {
            return Ok(false);
        }
        if left_type.is_dir() {
            if !trees_equal(&left_path, &right_path)? {
                return Ok(false);
            }
        } else if left_type.is_file() && std::fs::read(&left_path)? != std::fs::read(&right_path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn install_mod_source(source: &Path, mods_dir: &Path) -> Result<bool> {
    let destination = mods_dir.join("claude-interface");
    if trees_equal(source, &destination)? {
        return Ok(false);
    }

    std::fs::create_dir_all(mods_dir)?;
    let staged = mods_dir.join(format!(
        ".claude-interface.installing-{}",
        std::process::id()
    ));
    let backup = mods_dir.join(format!(".claude-interface.backup-{}", std::process::id()));
    if staged.exists() {
        std::fs::remove_dir_all(&staged)?;
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    copy_tree(source, &staged)
        .with_context(|| format!("failed to stage Factorio mod from {}", source.display()))?;

    let had_destination = destination.exists();
    if had_destination {
        std::fs::rename(&destination, &backup).with_context(|| {
            format!("failed to preserve installed mod {}", destination.display())
        })?;
    }
    if let Err(error) = std::fs::rename(&staged, &destination) {
        if had_destination {
            let _ = std::fs::rename(&backup, &destination);
        }
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error)
            .with_context(|| format!("failed to activate Factorio mod {}", destination.display()));
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    Ok(true)
}

fn install_mod_into(mods_dir: &Path) -> Result<bool> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mod/claude-interface");
    install_mod_source(&source, mods_dir)
        .with_context(|| format!("failed to install Factorio mod from {}", source.display()))
}

#[cfg(target_os = "linux")]
fn factorio_client_running() -> bool {
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return false;
    };
    processes.filter_map(Result::ok).any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            return false;
        };
        let Ok(command_line) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            return false;
        };
        let args = command_line
            .split(|byte| *byte == 0)
            .filter_map(|arg| std::str::from_utf8(arg).ok())
            .collect::<Vec<_>>();
        let is_factorio = args
            .first()
            .is_some_and(|program| program.rsplit('/').next() == Some("factorio"));
        is_factorio
            && !args
                .iter()
                .any(|arg| matches!(*arg, "--start-server" | "--create"))
    })
}

#[cfg(not(target_os = "linux"))]
fn factorio_client_running() -> bool {
    false
}

fn install_mods(write_data: &Path) -> Result<()> {
    if install_mod_into(&write_data.join("mods"))? {
        info!(path = %write_data.join("mods").display(), "installed Factorio server mod");
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let client_mods = home.join(".factorio/mods");
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mod/claude-interface");
        let destination = client_mods.join("claude-interface");
        if !trees_equal(&source, &destination)? {
            if factorio_client_running() {
                bail!(
                    "the Factorio client is running with a different Buddy mod; close it and rerun the same command so the synchronized mod can be installed"
                );
            }
            install_mod_source(&source, &client_mods)
                .context("failed to install Buddy mod for the Factorio client")?;
            info!(path = %client_mods.display(), "installed Factorio client mod");
        }
    }
    Ok(())
}

fn newest_autosave(directory: &Path) -> Result<Option<(PathBuf, SystemTime)>> {
    let mut newest: Option<(PathBuf, SystemTime)> = None;
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(None);
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("_autosave")
            || path.extension().and_then(|ext| ext.to_str()) != Some("zip")
        {
            continue;
        }
        let modified = entry.metadata()?.modified()?;
        if newest
            .as_ref()
            .is_none_or(|(newest_path, newest_modified)| {
                modified > *newest_modified || (modified == *newest_modified && path > *newest_path)
            })
        {
            newest = Some((path, modified));
        }
    }
    Ok(newest)
}

fn normalized_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve {}", path.display()));
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().context("path has no parent directory")?;
    let parent = if parent.exists() {
        parent.canonicalize()?
    } else {
        parent.to_owned()
    };
    Ok(parent.join(absolute.file_name().context("path has no file name")?))
}

fn save_identity(path: &Path) -> Result<SaveIdentity> {
    // A stable content identity is sufficient here: it prevents an ownership
    // sidecar left beside one save from authorizing recovery over replacement
    // content at the same path. Run isolation below is the stronger ownership
    // boundary for autosaves themselves.
    const FNV_OFFSET_BASIS: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    let mut file = File::open(path)
        .with_context(|| format!("failed to open save identity source {}", path.display()))?;
    let mut digest = FNV_OFFSET_BASIS;
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .context("save is too large to identify")?;
        for byte in &buffer[..read] {
            digest ^= u128::from(*byte);
            digest = digest.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(SaveIdentity {
        size,
        digest: format!("fnv1a128:{digest:032x}"),
    })
}

fn save_owner_path(save: &Path) -> Result<PathBuf> {
    let parent = save.parent().context("save path has no parent directory")?;
    let mut name = save
        .file_name()
        .context("save path has no file name")?
        .to_os_string();
    name.push(".buddy-owner.json");
    Ok(parent.join(name))
}

fn valid_run_id(run_id: &str) -> bool {
    run_id.len() == 64 && run_id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn promote_owned_autosave(
    save: &Path,
    owner_path: &Path,
    managed_runs_root: &Path,
) -> Result<Option<PathBuf>> {
    let owner: SaveOwner = match std::fs::read(owner_path) {
        Ok(encoded) => serde_json::from_slice(&encoded)
            .with_context(|| format!("invalid save ownership file {}", owner_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if owner.version != 2 || owner.clean_shutdown {
        return Ok(None);
    }
    let requested = normalized_path(save)?;
    if Path::new(&owner.primary_save) != requested {
        warn!(
            requested = %requested.display(),
            owner = %owner.primary_save,
            "refusing to promote an autosave owned by a different primary save"
        );
        return Ok(None);
    }

    if save.exists() {
        let current_identity = save_identity(save)?;
        if current_identity.size != owner.primary_identity.size
            || current_identity.digest != owner.primary_identity.digest
        {
            warn!(
                save = %save.display(),
                "refusing to promote an autosave over primary save content that changed outside the owned run"
            );
            return Ok(None);
        }
    }

    if !valid_run_id(&owner.run_id) {
        warn!(run_id = %owner.run_id, "refusing invalid autosave run identity");
        return Ok(None);
    }
    let expected_run_directory = normalized_path(&managed_runs_root.join(&owner.run_id))?;
    if Path::new(&owner.run_directory) != expected_run_directory {
        warn!(
            expected = %expected_run_directory.display(),
            owner = %owner.run_directory,
            "refusing autosaves outside the owned run directory"
        );
        return Ok(None);
    }

    let Some((autosave, autosave_modified)) =
        newest_autosave(&expected_run_directory.join("saves"))?
    else {
        return Ok(None);
    };
    if save.exists() {
        let save_modified = std::fs::metadata(save)?.modified()?;
        if save_modified >= autosave_modified {
            return Ok(None);
        }
    }

    let directory = save.parent().context("save path has no parent directory")?;
    let stem = save
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("save");
    let backup = directory.join(format!("{stem}.previous.zip"));
    let staged = directory.join(format!(".{stem}.recovering.zip"));

    if save.exists() {
        std::fs::copy(save, &backup).with_context(|| {
            format!(
                "failed to preserve stale save {} as {}",
                save.display(),
                backup.display()
            )
        })?;
    }
    if staged.exists() {
        std::fs::remove_file(&staged)?;
    }
    std::fs::copy(&autosave, &staged).with_context(|| {
        format!(
            "failed to stage autosave {} as {}",
            autosave.display(),
            staged.display()
        )
    })?;
    std::fs::File::open(&staged)?.sync_all()?;
    std::fs::rename(&staged, save).with_context(|| {
        format!(
            "failed to promote autosave {} to {}",
            autosave.display(),
            save.display()
        )
    })?;

    info!(
        source = %autosave.display(),
        save = %save.display(),
        backup = %backup.display(),
        "promoted newer autosave before resume"
    );
    Ok(Some(autosave))
}

fn managed_factorio_config(data_root: &Path, run_directory: &Path) -> String {
    format!(
        "[path]\nread-data={}\nwrite-data={}\n\n[other]\ncheck-updates=false\ndrop-detection-threshold-time={}\n",
        data_root.join("data").display(),
        run_directory.display(),
        MANAGED_CLIENT_DROP_THRESHOLD_SECONDS,
    )
}

async fn start_local_server(args: &mut Args) -> Result<LocalServer> {
    if tokio::net::TcpStream::connect((&*args.rcon_host, args.rcon_port))
        .await
        .is_ok()
    {
        bail!(
            "RCON port {} is already in use; stop the existing Factorio server before `just play`",
            args.rcon_port
        );
    }
    let factorio = find_factorio(args.factorio_bin.clone())?;
    let write_data = std::fs::canonicalize(&args.write_data).or_else(|_| {
        std::fs::create_dir_all(&args.write_data)?;
        std::fs::canonicalize(&args.write_data)
    })?;
    install_mods(&write_data)?;
    std::fs::create_dir_all(write_data.join("saves"))?;

    let data_root = factorio
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .context("cannot derive Factorio data directory from binary path")?;
    let save = args
        .save
        .clone()
        .unwrap_or_else(|| write_data.join("saves/buddy.zip"));
    if let Some(parent) = save.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let save = normalized_path(&save)?;
    let save_owner_path = save_owner_path(&save)?;
    let managed_runs_root = write_data.join("managed-runs");
    std::fs::create_dir_all(&managed_runs_root)?;
    if args.fresh && save.exists() {
        std::fs::remove_file(&save)
            .with_context(|| format!("failed to remove old save: {}", save.display()))?;
    }
    if !args.fresh {
        promote_owned_autosave(&save, &save_owner_path, &managed_runs_root)?;
    }

    let run_id = generate_password()?;
    let run_directory = managed_runs_root.join(&run_id);
    std::fs::create_dir_all(run_directory.join("saves"))?;
    let run_directory = normalized_path(&run_directory)?;
    let config = run_directory.join("config.ini");
    atomic_write(
        &config,
        managed_factorio_config(data_root, &run_directory).as_bytes(),
        true,
    )?;
    if !save.exists() {
        info!(save = %save.display(), seed = args.map_seed, "creating Factorio save");
        let status = Command::new(&factorio)
            .arg("--config")
            .arg(&config)
            .arg("--mod-directory")
            .arg(write_data.join("mods"))
            .arg("--create")
            .arg(&save)
            .arg("--map-gen-settings")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/map-gen.json"))
            .arg("--map-gen-seed")
            .arg(args.map_seed.to_string())
            .status()
            .await?;
        if !status.success() {
            if !save.is_file() {
                bail!("Factorio failed to create save: {status}");
            }
            warn!(%status, "Factorio created the save but reported a non-zero create status");
        }
    }

    let save_owner = SaveOwner {
        version: 2,
        primary_save: normalized_path(&save)?.to_string_lossy().into_owned(),
        primary_identity: save_identity(&save)?,
        run_id,
        run_directory: run_directory.to_string_lossy().into_owned(),
        clean_shutdown: false,
    };
    write_json_atomic(&save_owner_path, &save_owner, false)?;
    args.script_output = Some(run_directory.join("script-output"));

    let mut server_command = Command::new(&factorio);
    server_command
        .arg("--config")
        .arg(&config)
        .arg("--mod-directory")
        .arg(write_data.join("mods"))
        .arg("--start-server")
        .arg(&save)
        .arg("--rcon-bind")
        .arg(format!("127.0.0.1:{}", args.rcon_port))
        .arg("--rcon-password")
        .arg(rcon_password(args)?)
        .arg("--port")
        .arg(args.game_port.to_string())
        .arg("--server-settings")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("configs/server.json"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    server_command.process_group(0);
    let mut child = server_command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .context("Factorio stdin pipe unavailable")?;
    let stdout = child
        .stdout
        .take()
        .context("Factorio stdout pipe unavailable")?;
    let output_task = tokio::spawn(forward_factorio_output(stdout));

    for _ in 0..60 {
        if let Some(status) = child.try_wait()? {
            bail!("Factorio server exited during startup: {status}");
        }
        if FactorioClient::connect(&args.rcon_host, args.rcon_port, rcon_password(args)?)
            .await
            .is_ok()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(status) = child.try_wait()? {
                bail!("Factorio server exited after opening RCON: {status}");
            }
            info!(save = %save.display(), "Factorio server ready");
            return Ok(LocalServer {
                child,
                _stdin: stdin,
                output_task,
                save_owner_path,
                save_owner,
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let _ = child.kill().await;
    bail!("Factorio server did not open RCON within 30 seconds")
}

/// Environment for the Factorio MCP server, shared by Claude's MCP config and
/// the runtime maintenance client so both reach the same agent/RCON/tracker.
fn mcp_env(args: &Args) -> Vec<(&'static str, String)> {
    vec![
        ("FACTORIO_RCON_HOST", args.rcon_host.clone()),
        ("FACTORIO_RCON_PORT", args.rcon_port.to_string()),
        (
            "FACTORIO_RCON_PASSWORD",
            rcon_password(args).expect("password configured").to_owned(),
        ),
        ("FACTORIO_AGENT_ID", args.agent.clone()),
        (
            "FACTORIO_BUDDY_PROJECT_ROOT",
            args.issue_project_root.to_string_lossy().into_owned(),
        ),
    ]
}

fn mcp_config(args: &Args, mcp: &Path) -> String {
    let env: serde_json::Map<String, Value> = mcp_env(args)
        .into_iter()
        .map(|(key, value)| (key.to_owned(), json!(value)))
        .collect();
    json!({"mcpServers": {"factorio": {
        "command": mcp,
        "env": env,
    }}})
    .to_string()
}

fn write_mcp_config(args: &Args, mcp: &Path) -> Result<PathBuf> {
    let path = args.write_data.join(format!("mcp-{}.json", args.agent));
    atomic_write(&path, mcp_config(args, mcp).as_bytes(), true)?;
    Ok(path)
}

fn validate_lifecycle_response(function: &str, response: &str) -> Result<()> {
    let trimmed = response.trim();
    if function == "ping" {
        if trimmed != "pong" {
            bail!("lifecycle ping returned {trimmed:?}, expected \"pong\"");
        }
        return Ok(());
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(());
    };
    if value.get("success").and_then(Value::as_bool) == Some(false) {
        let kind = value
            .get("error_kind")
            .and_then(Value::as_str)
            .unwrap_or("remote_error");
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("remote lifecycle call failed");
        bail!("{function} failed ({kind}): {message}");
    }
    if let Some(kind) = value.get("error_kind").and_then(Value::as_str) {
        bail!("{function} failed ({kind})");
    }
    if let Some(message) = value.get("error").and_then(Value::as_str) {
        if !message.is_empty() {
            bail!("{function} failed: {message}");
        }
    }
    if function == "pre_place_character_result" {
        match value.get("status").and_then(Value::as_str) {
            Some("created" | "already_placed") => {}
            Some(status) => bail!("pre_place_character_result failed with status {status}"),
            None => bail!("pre_place_character_result returned no status"),
        }
    }
    Ok(())
}

fn lifecycle_is_read_only(function: &str) -> bool {
    matches!(
        function,
        "ping" | "connected_player_count_result" | "autonomy_snapshot"
    )
}

impl LifecycleClient {
    fn new(args: &Args) -> Result<Self> {
        AgentId::new(Some(&args.agent))?;
        Ok(Self {
            host: args.rcon_host.clone(),
            port: args.rcon_port,
            password: rcon_password(args)?.to_owned(),
            agent: args.agent.clone(),
            client: Mutex::new(None),
        })
    }

    async fn call(&self, function: &str, values: &[Value]) -> Result<String> {
        let attempts = if lifecycle_is_read_only(function) {
            2
        } else {
            1
        };
        let mut last_error = None;
        for attempt in 0..attempts {
            let mut guard = self.client.lock().await;
            let mut client = match guard.take() {
                Some(client) => client,
                None => {
                    let agent_id = AgentId::new(Some(&self.agent))?;
                    match FactorioClient::connect(&self.host, self.port, &self.password).await {
                        Ok(client) => client.with_agent_id(agent_id),
                        Err(error) => {
                            last_error = Some(error);
                            if attempt + 1 < attempts {
                                warn!(function, "lifecycle RCON connection failed; retrying once");
                                continue;
                            }
                            break;
                        }
                    }
                }
            };
            // Keep the slot empty while an RCON operation is in flight. If the
            // task is cancelled, the local client is dropped instead of
            // returning a potentially half-read protocol stream to the pool.
            let response = client.call_remote(function, values).await;
            match response {
                Ok(response) => {
                    *guard = Some(client);
                    validate_lifecycle_response(function, &response)?;
                    if function == "receive_response" {
                        let field =
                            |index: usize| values.get(index).cloned().unwrap_or(Value::Null);
                        info!(
                            event = "response_delivered",
                            player_index = %field(0),
                            agent = %field(1),
                            text = %field(2),
                            "response delivered to Factorio"
                        );
                    }
                    return Ok(response);
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < attempts {
                        warn!(function, "lifecycle RCON disconnected; reconnecting once");
                    }
                }
            }
        }
        let error = last_error.context("lifecycle call failed without a transport error")?;
        Err(error).with_context(|| format!("lifecycle call {function} failed"))
    }
}

fn stream_event_session_id(event: &Value) -> Option<&str> {
    event.get("session_id").and_then(Value::as_str)
}

fn is_provider_usage_limit(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("limit")
        && message.contains("reset")
        && (message.contains("weekly/monthly limit exhausted")
            || message.contains("usage limit reached")
            || message.contains("hit your session limit")
            || message.contains("hit your usage limit"))
}

fn provider_limit_active(retry_at: Option<Instant>, now: Instant) -> bool {
    retry_at.is_some_and(|retry_at| now < retry_at)
}

fn autonomy_prompt(snapshot: &str) -> String {
    let formatted_snapshot = serde_json::from_str::<Value>(snapshot)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .unwrap_or_else(|_| snapshot.to_owned());
    format!("{AUTONOMY_DIRECTIVE}\n\nAuthoritative current factory snapshot:\n{formatted_snapshot}")
}

fn effective_system_prompt(args: &Args) -> String {
    let Some(persona) = args
        .persona
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return args.system_prompt.clone();
    };
    format!(
        "{}\n\nOperator-defined persona:\n{}",
        args.system_prompt.trim_end(),
        persona
    )
}

async fn collect_autonomy_prompt(args: &Args, lifecycle: &LifecycleClient) -> String {
    match lifecycle
        .call("autonomy_snapshot", &[json!(args.agent)])
        .await
    {
        Ok(snapshot) => autonomy_prompt(&snapshot),
        Err(error) => {
            warn!(%error, "failed to collect autonomy snapshot");
            format!("{AUTONOMY_DIRECTIVE}\n\nThe automatic snapshot failed. Inspect the whole factory with read-only tools before choosing what to work on.")
        }
    }
}

fn claude_arguments(
    args: &Args,
    config: &str,
    prompt: &str,
    session_id: Option<&str>,
) -> Vec<OsString> {
    let mut arguments = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        config.into(),
        "--permission-mode".into(),
        "bypassPermissions".into(),
        "--allowedTools".into(),
        "mcp__factorio__*".into(),
        "--disallowedTools".into(),
        "mcp__factorio__execute_lua".into(),
        "--tools".into(),
        "".into(),
        "--setting-sources".into(),
        "".into(),
        "--disable-slash-commands".into(),
        "--effort".into(),
        args.effort.clone().into(),
        "--system-prompt".into(),
        effective_system_prompt(args).into(),
    ];
    if let Some(model) = &args.model {
        arguments.push("--model".into());
        arguments.push(model.into());
    }
    if let Some(session_id) = session_id {
        arguments.push("--resume".into());
        arguments.push(session_id.into());
    }
    arguments.push("--".into());
    arguments.push(prompt.into());
    arguments
}

/// Everything a turn task needs; shared by Claude and maintenance turns.
struct Runtime {
    args: Arc<Args>,
    config: Arc<str>,
    lifecycle: Arc<LifecycleClient>,
    shared: Shared,
    mcp: PathBuf,
    jev: Option<Arc<JevClient>>,
    /// Consumers whose maintenance repair failed since the last completed
    /// Opus autonomy turn.
    failed_repairs: StdMutex<HashSet<u64>>,
    inbox_path: PathBuf,
    inbox_offset: AtomicU64,
}

impl Runtime {
    /// True when the chat inbox has bytes Buddy has not consumed yet.
    fn inbox_has_unread(&self) -> bool {
        std::fs::metadata(&self.inbox_path)
            .map(|metadata| metadata.len() > self.inbox_offset.load(Ordering::SeqCst))
            .unwrap_or(false)
    }
}

struct StderrTail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl StderrTail {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
        }
    }

    fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.lines.len() > STDERR_TAIL_LINES
            || (self.bytes > STDERR_TAIL_BYTES && self.lines.len() > 1)
        {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
            }
        }
    }

    fn render(&self) -> String {
        let joined = self.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        bounded(&joined, STDERR_TAIL_BYTES)
    }
}

/// `delivered_note` lists the outcome-unknown entries embedded in `prompt`;
/// they are acknowledged (and the list emptied) once Claude emits its first
/// stream event.
async fn invoke_claude(
    rt: &Runtime,
    prompt: &str,
    delivered_note: &mut Vec<(String, Value)>,
    player_index: u32,
    response_agent: &str,
    session_id: &mut Option<String>,
) -> Result<ClaudeReply> {
    let args = &*rt.args;
    let lifecycle = &*rt.lifecycle;
    let shared = &rt.shared;
    // No previously owned Claude/MCP tree may still be able to mutate the world.
    shared.reap_and_reconcile().await;
    let started = Instant::now();
    info!(event = "turn_start", session = session_id.as_deref().unwrap_or("new"), prompt = %prompt, "Claude turn started");
    let mut command = Command::new("claude");
    command
        .args(claude_arguments(
            args,
            &rt.config,
            prompt,
            session_id.as_deref(),
        ))
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    // Own the whole tree (Claude plus its MCP server) so cancellation can
    // terminate descendants, not just the direct child.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .context("failed to start `claude`; install/authenticate Claude Code")?;
    if let Some(pid) = child.id() {
        shared.lock().process_group = Some(pid as i32);
    }
    let stdout = child.stdout.take().context("claude stdout unavailable")?;
    let stderr = child.stderr.take().context("claude stderr unavailable")?;
    let observed_session_id = Arc::new(StdMutex::new(session_id.clone()));
    let streamed_session_id = Arc::clone(&observed_session_id);

    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail = StderrTail::new();
        while let Ok(Some(line)) = lines.next_line().await {
            warn!(event = "model_stderr", message = %line, "Claude stderr");
            tail.push(line);
        }
        tail.render()
    });

    let stream = async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut final_result = None;
        let mut delivered_text = Vec::new();
        let mut tool_names: HashMap<String, String> = HashMap::new();
        while let Some(line) = lines.next_line().await? {
            if !delivered_note.is_empty() {
                // Claude is running and has consumed the prompt; acknowledge
                // exactly once so later identical entries are kept.
                shared.acknowledge_unknown_outcomes(delivered_note);
                delivered_note.clear();
            }
            let event: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => {
                    warn!(event = "model_protocol_error", %error, raw = %line, "Invalid Claude event");
                    continue;
                }
            };
            if let Some(id) = stream_event_session_id(&event) {
                if let Ok(mut observed) = streamed_session_id.lock() {
                    *observed = Some(id.to_owned());
                }
            }
            match event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "assistant" => {
                    if let Some(blocks) =
                        event.pointer("/message/content").and_then(Value::as_array)
                    {
                        for block in blocks {
                            match block
                                .get("type")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                            {
                                "tool_use" => {
                                    let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                                    let tool = block
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown");
                                    let arguments =
                                        block.get("input").cloned().unwrap_or(Value::Null);
                                    tool_names.insert(id.to_owned(), tool.to_owned());
                                    shared.tool_started(id, tool, &arguments);
                                    info!(event = "tool_call", tool, tool_use_id = id, arguments = %arguments, "Tool call");
                                }
                                "text" => {
                                    let text = block
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .trim();
                                    info!(event = "model_text", text, "Claude text");
                                    if !text.is_empty() {
                                        match lifecycle
                                            .call(
                                                "receive_response",
                                                &[
                                                    json!(player_index),
                                                    json!(response_agent),
                                                    json!(text),
                                                ],
                                            )
                                            .await
                                        {
                                            Ok(_) => delivered_text.push(text.to_owned()),
                                            Err(error) => warn!(
                                                %error,
                                                "failed to stream Claude text to Factorio"
                                            ),
                                        }
                                    }
                                }
                                "thinking" => {
                                    info!(event = "model_thinking", thinking = %block.get("thinking").and_then(|value| value.as_str()).unwrap_or(""), "Claude thinking")
                                }
                                kind => {
                                    info!(event = "model_content", content_type = kind, payload = %block, "Claude content")
                                }
                            }
                        }
                    }
                }
                "user" => {
                    if let Some(blocks) =
                        event.pointer("/message/content").and_then(Value::as_array)
                    {
                        for block in blocks {
                            if block.get("type").and_then(Value::as_str) == Some("tool_result") {
                                let id = block
                                    .get("tool_use_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                let is_error = block
                                    .get("is_error")
                                    .and_then(|value| value.as_bool())
                                    .unwrap_or(false);
                                let result = block.get("content").cloned().unwrap_or(Value::Null);
                                shared.tool_finished(id, is_error, &tool_result_text(&result));
                                info!(event = "tool_result", tool = tool_names.get(id).map(String::as_str).unwrap_or("unknown"), tool_use_id = id, is_error, result = %result, "Tool result");
                            }
                        }
                    }
                }
                "result" => {
                    info!(event = "model_result", payload = %event, "Claude result");
                    final_result = serde_json::from_value::<ClaudeResult>(event).ok();
                }
                kind => {
                    info!(event = "model_event", event_type = kind, payload = %event, "Claude event")
                }
            }
        }
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((status, final_result, delivered_text))
    };

    let outcome =
        if args.turn_timeout_seconds == 0 {
            stream.await
        } else {
            match timeout(Duration::from_secs(args.turn_timeout_seconds), stream).await {
                Ok(result) => result,
                Err(error) => Err(anyhow::Error::new(error)
                    .context("claude turn exceeded the wall-clock timeout")),
            }
        };
    // Claude has exited or been dropped; terminate any surviving descendants
    // and mark unanswered tool calls as outcome-unknown.
    shared.reap_and_reconcile().await;
    let stderr_tail = match timeout(Duration::from_secs(2), stderr_task).await {
        Ok(Ok(tail)) => tail,
        _ => String::new(),
    };
    if let Ok(observed) = observed_session_id.lock() {
        if let Some(id) = observed.as_deref() {
            if session_id.as_deref() != Some(id) {
                info!(event = "session", session = id, "Claude session observed");
                *session_id = Some(id.to_owned());
            }
        }
    }
    let (status, parsed, delivered_text) = outcome?;
    info!(event = "turn_exit", exit_status = %status, duration_ms = started.elapsed().as_millis(), "Claude process exited");

    if let Some(parsed) = parsed {
        if let Some(id) = parsed.session_id.as_deref() {
            info!(event = "session", session = %id, "Claude session active");
            *session_id = Some(id.to_owned());
        }
        if parsed.is_error {
            if parsed.is_provider_limit() {
                warn!(event = "provider_limit", message = %parsed.result, "Claude subscription usage limit reached");
                *session_id = None;
                return Err(ClaudeUsageLimit.into());
            }
            bail!("claude returned an error: {}", parsed.result);
        }
        if !parsed.result.trim().is_empty() {
            info!(event = "turn_complete", response = %parsed.result, "Claude turn completed");
            let already_delivered = delivered_text
                .iter()
                .any(|text| text.trim() == parsed.result.trim());
            return Ok(ClaudeReply {
                text: parsed.result,
                already_delivered,
            });
        }
    }
    let stderr_suffix = if stderr_tail.is_empty() {
        String::new()
    } else {
        format!("; stderr tail: {stderr_tail}")
    };
    if !status.success() {
        bail!("claude exited {status} without a valid result{stderr_suffix}");
    }
    bail!("claude returned no final result{stderr_suffix}")
}

async fn handle_turn(
    rt: Arc<Runtime>,
    request: TurnRequest,
    mut session_id: Option<String>,
) -> TurnCompletion {
    let lifecycle = &rt.lifecycle;
    rt.shared.reap_and_reconcile().await;
    let mut prompt = request
        .prompt
        .unwrap_or_else(|| String::from(AUTONOMY_DIRECTIVE));
    // Peek, not take: a turn cancelled before Claude starts keeps the note.
    let mut delivered_note = Vec::new();
    if let Some((note, entries)) = rt.shared.peek_unknown_outcome_note() {
        prompt = format!("{note}\n{prompt}");
        delivered_note = entries;
    }
    let _ = lifecycle
        .call(
            "set_status",
            &[
                json!(request.player_index),
                json!("[color=0.8,0.7,0.2]Thinking...[/color]"),
            ],
        )
        .await;

    let calls_before = rt.shared.turn_counts().0;
    let mut result = invoke_claude(
        &rt,
        &prompt,
        &mut delivered_note,
        request.player_index,
        &request.response_agent,
        &mut session_id,
    )
    .await;
    // Replaying the whole prompt is only safe when the failed attempt never
    // called a tool; otherwise its calls may have mutated the world and the
    // turn fails so the next turn receives the outcome-unknown note.
    let first_attempt_tool_calls = rt.shared.turn_counts().0 - calls_before;
    if result
        .as_ref()
        .err()
        .is_some_and(|error| session_id.is_some() && is_invalid_session_error(error))
    {
        if first_attempt_tool_calls == 0 {
            warn!(
                event = "session_reset",
                "Claude session was unavailable; retrying this turn without --resume"
            );
            session_id = None;
            result = invoke_claude(
                &rt,
                &prompt,
                &mut delivered_note,
                request.player_index,
                &request.response_agent,
                &mut session_id,
            )
            .await;
        } else {
            warn!(
                event = "session_reset_without_retry",
                tool_calls = first_attempt_tool_calls,
                "Claude session was unavailable after tool calls; not replaying the turn"
            );
            session_id = None;
        }
    }
    // The turn completed without cancellation: the note has been handed over.
    rt.shared.acknowledge_unknown_outcomes(&delivered_note);

    let provider_limited = result
        .as_ref()
        .err()
        .is_some_and(|error| error.downcast_ref::<ClaudeUsageLimit>().is_some());

    let succeeded = match result {
        Ok(reply) => {
            if !reply.already_delivered {
                if let Err(error) = lifecycle
                    .call(
                        "receive_response",
                        &[
                            json!(request.player_index),
                            json!(request.response_agent),
                            json!(reply.text),
                        ],
                    )
                    .await
                {
                    warn!(%error, "failed to send response to Factorio");
                }
            }
            Some(true)
        }
        Err(error) => {
            warn!(%error, "agent turn failed");
            let response = if provider_limited {
                PROVIDER_LIMIT_MESSAGE.to_owned()
            } else {
                // The panel is small: one bounded line. The full error, with
                // its stderr tail, stays in the log above.
                let first_line = format!("{error}");
                let first_line = first_line.lines().next().unwrap_or("");
                format!("Agent error: {}", bounded(first_line, 180))
            };
            let _ = lifecycle
                .call(
                    "receive_response",
                    &[
                        json!(request.player_index),
                        json!(request.response_agent),
                        json!(response),
                    ],
                )
                .await;
            Some(false)
        }
    };
    let _ = lifecycle
        .call(
            "set_status",
            &[
                json!(request.player_index),
                json!("[color=0.4,0.8,0.4]Ready[/color]"),
            ],
        )
        .await;
    TurnCompletion {
        session_id,
        succeeded,
        provider_limited,
        follow_with_opus: false,
    }
}

fn is_invalid_session_error(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    // Match Claude Code's resume diagnostics, not generic words that any
    // stderr line (MCP diagnostics, API bodies) may contain.
    message.contains("no conversation found") || message.contains("invalid session id")
}

const REPAIR_TOOL: &str = "repair_fuel_sustainability";

fn repair_arguments(dry_run: bool) -> Value {
    json!({"radius": 64, "limit": 20, "dry_run": dry_run})
}

/// Compact decision state: observed facts only, no chat, credentials or thinking.
fn decision_state(
    snapshot: &Value,
    preview: &Preview,
    preview_reply: &Value,
    previously_failed: bool,
    recent_outcomes: Option<String>,
) -> Value {
    json!({
        "tick": snapshot.get("tick"),
        "character": snapshot.get("character"),
        "research": snapshot.get("research"),
        "blockers": snapshot.pointer("/factory/blockers"),
        "fuel_repair_preview": {
            "consumer_unit_number": preview.consumer_unit_number,
            "transaction": preview.transaction,
            "diagnosis": preview_reply.get("diagnosis"),
        },
        "consumer_repair_failed_since_last_plan": previously_failed,
        "recent_tool_outcomes": recent_outcomes,
    })
}

fn record_decision(
    rt: &Runtime,
    eligible: bool,
    selection: Option<Selection>,
    jev: Option<&decision::JevOutcome>,
    unavailable_reason: Option<String>,
    preview: Option<&Preview>,
) {
    let mode = rt.args.decision_mode;
    let answer = jev.and_then(|outcome| outcome.answer.as_ref());
    let choice = match (answer, mode) {
        (Some(answer), _) => Some(answer.choice.clone()),
        (None, DecisionMode::Deterministic) => selection.map(|selection| match selection {
            Selection::Repair => decision::CHOICE_REPAIR.to_owned(),
            Selection::ReturnToOpus => decision::CHOICE_REPLAN.to_owned(),
        }),
        (None, _) => None,
    };
    let unavailable_reason =
        unavailable_reason.or_else(|| jev.and_then(|outcome| outcome.unavailable_reason.clone()));
    info!(event = "decision", mode = mode.as_str(), eligible, choice = ?choice, selection = ?selection, unavailable = ?unavailable_reason, "maintenance decision");
    rt.shared.evidence.record(
        "decision",
        json!({
            "mode": mode.as_str(),
            "eligible": eligible,
            "choice": choice,
            "selection": selection.map(|selection| match selection {
                Selection::Repair => "repair",
                Selection::ReturnToOpus => "return_to_opus",
            }),
            "confidence": answer.map(|answer| answer.confidence),
            "probabilities": answer.map(|answer| Value::Object(answer.probabilities.clone())),
            "model": jev.and_then(|outcome| outcome.model.clone()),
            // null when no Jev request was made, so it never skews latency stats.
            "latency_ms": jev.map(|outcome| outcome.latency_ms as u64),
            "input_tokens": jev.and_then(|outcome| outcome.input_tokens),
            "unavailable_reason": unavailable_reason,
            "preview": preview.map(Preview::evidence),
            "jev_spent_usd_estimate": rt.jev.as_ref().map(|jev| jev.spent_usd()),
        }),
    );
}

/// Result of one maintenance opportunity.
struct MaintenanceOutcome {
    /// Control must go back to Opus right away (no repair executed, the
    /// repair failed, or it changed a different transaction than approved).
    follow_with_opus: bool,
    /// Executed repair outcome; `None` when no repair executed.
    repair_succeeded: Option<bool>,
}

impl MaintenanceOutcome {
    const RETURN_TO_OPUS: Self = Self {
        follow_with_opus: true,
        repair_succeeded: None,
    };
}

/// One bounded maintenance opportunity.
async fn maintenance_opportunity(rt: &Runtime) -> MaintenanceOutcome {
    let snapshot = match rt
        .lifecycle
        .call("autonomy_snapshot", &[json!(rt.args.agent)])
        .await
    {
        Ok(snapshot) => serde_json::from_str::<Value>(&snapshot).unwrap_or(Value::Null),
        Err(error) => {
            record_decision(
                rt,
                false,
                None,
                None,
                Some(format!("snapshot failed: {error}")),
                None,
            );
            return MaintenanceOutcome::RETURN_TO_OPUS;
        }
    };
    let client = match mcp_client::McpClient::spawn(&rt.mcp, &mcp_env(&rt.args)).await {
        Ok(client) => client,
        Err(error) => {
            record_decision(
                rt,
                false,
                None,
                None,
                Some(format!("MCP client unavailable: {error:#}")),
                None,
            );
            return MaintenanceOutcome::RETURN_TO_OPUS;
        }
    };
    rt.shared.lock().process_group = client.process_group();
    let outcome = maintenance_with_client(rt, &client, &snapshot).await;
    client.close().await;
    outcome
}

async fn dry_run_preview(
    client: &mcp_client::McpClient,
) -> std::result::Result<(Preview, Value), String> {
    let reply = client
        .call_tool(REPAIR_TOOL, repair_arguments(true))
        .await
        .map_err(|error| format!("dry-run call failed: {error:#}"))?;
    decision::preview_from_reply(reply.is_error, &reply.text)
}

async fn maintenance_with_client(
    rt: &Runtime,
    client: &mcp_client::McpClient,
    snapshot: &Value,
) -> MaintenanceOutcome {
    let mode = rt.args.decision_mode;
    let (preview, preview_reply) = match dry_run_preview(client).await {
        Ok(preview) => preview,
        Err(reason) => {
            record_decision(rt, false, None, None, Some(reason), None);
            return MaintenanceOutcome::RETURN_TO_OPUS;
        }
    };
    // Plan rule: skip a consumer whose maintenance repair failed since the
    // last completed Opus turn (the set is cleared when one completes).
    let previously_failed = rt
        .failed_repairs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .contains(&preview.consumer_unit_number);
    let jev_outcome = match (mode.uses_jev(), rt.jev.as_ref()) {
        (true, Some(jev)) => {
            let state = decision_state(
                snapshot,
                &preview,
                &preview_reply,
                previously_failed,
                rt.shared.outcome_summary(),
            );
            Some(jev.decide(&state).await)
        }
        _ => None,
    };
    let selection = decision::select(
        mode,
        previously_failed,
        jev_outcome
            .as_ref()
            .and_then(|outcome| outcome.answer.as_ref()),
    );
    record_decision(
        rt,
        true,
        Some(selection),
        jev_outcome.as_ref(),
        None,
        Some(&preview),
    );
    if selection != Selection::Repair {
        return MaintenanceOutcome::RETURN_TO_OPUS;
    }

    let not_executed = |reason: &str| {
        warn!(
            event = "maintenance_skipped",
            reason, "maintenance repair not executed"
        );
        rt.shared.evidence.record(
            "maintenance_result",
            json!({
                "consumer_unit_number": preview.consumer_unit_number,
                "success": false,
                "dry_run": false,
                "executed": false,
                "result_summary": reason,
            }),
        );
    };
    if rt.inbox_has_unread() {
        not_executed("a human message arrived; yielding to the human request");
        return MaintenanceOutcome {
            follow_with_opus: false,
            repair_succeeded: None,
        };
    }
    match dry_run_preview(client).await {
        Ok((refreshed, _)) if refreshed == preview => {}
        Ok(_) => {
            not_executed("refreshed preview selected a different consumer or transaction");
            return MaintenanceOutcome::RETURN_TO_OPUS;
        }
        Err(reason) => {
            not_executed(&format!("refreshed preview unavailable: {reason}"));
            return MaintenanceOutcome::RETURN_TO_OPUS;
        }
    }

    let arguments = repair_arguments(false);
    let call_id = format!("maintenance-{}", preview.consumer_unit_number);
    rt.shared.tool_started(&call_id, REPAIR_TOOL, &arguments);
    // The controller recomputes its preflight on execution, so the reply's
    // selected transaction (not the approved preview) names what was touched.
    let (success, actual, mut summary) = match client.call_tool(REPAIR_TOOL, arguments).await {
        Ok(reply) => {
            rt.shared
                .tool_finished(&call_id, reply.is_error, &reply.text);
            let parsed = serde_json::from_str::<Value>(reply.text.trim()).ok();
            let success = !reply.is_error
                && parsed
                    .as_ref()
                    .and_then(|value| value.get("success"))
                    .and_then(Value::as_bool)
                    == Some(true);
            let (_, error, _) = classify_tool_result(reply.is_error, &reply.text);
            (
                success,
                decision::executed_transaction(&reply.text),
                json!({"executed": true, "error": error}),
            )
        }
        // The call stays in flight and becomes an outcome-unknown record.
        Err(error) => (
            false,
            None,
            json!({"executed": "unknown", "error": format!("{error:#}")}),
        ),
    };
    let executed_known = summary["executed"] == json!(true);
    // Only a reply that names a transaction proves which consumer was touched.
    let transaction_changed = executed_known.then(|| actual.as_ref() != Some(&preview));
    let actual_consumer = actual.as_ref().map(|actual| actual.consumer_unit_number);
    {
        let mut failed = rt
            .failed_repairs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // Attribute the outcome to the consumer actually selected; fall back
        // to the approved one only when the reply does not name one.
        let consumer = actual_consumer.unwrap_or(preview.consumer_unit_number);
        if success {
            failed.remove(&consumer);
        } else {
            failed.insert(consumer);
        }
    }
    let post_tick = rt
        .lifecycle
        .call("autonomy_snapshot", &[json!(rt.args.agent)])
        .await
        .ok()
        .and_then(|snapshot| serde_json::from_str::<Value>(&snapshot).ok())
        .and_then(|snapshot| snapshot.get("tick").and_then(Value::as_u64));
    summary["post_snapshot_tick"] = json!(post_tick);
    info!(
        event = "maintenance_result",
        consumer = preview.consumer_unit_number,
        actual_consumer = ?actual_consumer,
        transaction_changed = ?transaction_changed,
        success,
        "maintenance repair finished"
    );
    let mut result = json!({
        "consumer_unit_number": preview.consumer_unit_number,
        "success": success,
        "dry_run": false,
        "transaction_changed": transaction_changed,
        "result_summary": summary,
    });
    if let Some(actual) = &actual {
        result["actual_consumer_unit_number"] = json!(actual.consumer_unit_number);
        if transaction_changed == Some(true) {
            result["actual_transaction"] = actual.transaction.clone();
        }
    }
    rt.shared.evidence.record("maintenance_result", result);
    MaintenanceOutcome {
        // A changed transaction was not the approved decision: Opus inspects.
        follow_with_opus: !success || transaction_changed == Some(true),
        repair_succeeded: Some(success),
    }
}

async fn run_maintenance(rt: Arc<Runtime>, session_id: Option<String>) -> TurnCompletion {
    rt.shared.reap_and_reconcile().await;
    let outcome = maintenance_opportunity(&rt).await;
    rt.shared.reap_and_reconcile().await;
    TurnCompletion {
        session_id,
        succeeded: outcome.repair_succeeded,
        provider_limited: false,
        follow_with_opus: outcome.follow_with_opus,
    }
}

fn start_turn(
    rt: &Arc<Runtime>,
    mut request: TurnRequest,
    session_id: Option<String>,
) -> ActiveTurn {
    let kind = request.kind;
    rt.shared.begin_turn();
    let rt = Arc::clone(rt);
    let handle = tokio::spawn(async move {
        match kind {
            TurnKind::Maintenance => run_maintenance(rt, session_id).await,
            TurnKind::Autonomy => {
                let mut prompt = collect_autonomy_prompt(&rt.args, &rt.lifecycle).await;
                if let Some(summary) = rt.shared.outcome_summary() {
                    prompt = format!("{prompt}\n\n{summary}");
                }
                request.prompt = Some(prompt);
                handle_turn(rt, request, session_id).await
            }
            TurnKind::Human => handle_turn(rt, request, session_id).await,
        }
    });
    ActiveTurn {
        kind,
        started: Instant::now(),
        handle,
    }
}

fn record_turn_finished(rt: &Runtime, turn: &ActiveTurn, completion: Option<&TurnCompletion>) {
    let (tool_calls, tool_errors) = rt.shared.turn_counts();
    rt.shared.evidence.record(
        "turn_finished",
        json!({
            "kind": turn.kind.evidence_name(),
            // null: a maintenance turn that executed no repair.
            "succeeded": match completion {
                Some(completion) => json!(completion.succeeded),
                None => json!(false),
            },
            "provider_limited": completion.is_some_and(|completion| completion.provider_limited),
            "cancelled": completion.is_none(),
            "duration_ms": turn.started.elapsed().as_millis() as u64,
            "tool_calls": tool_calls,
            "tool_errors": tool_errors,
        }),
    );
}

fn autonomy_deadline_reached(args: &Args, online_at: Instant) -> bool {
    args.autonomy_deadline_seconds > 0
        && online_at.elapsed() >= Duration::from_secs(args.autonomy_deadline_seconds)
}

fn record_autonomy_exhausted(rt: &Runtime, reason: &str, autonomous_turns: u64) {
    info!(
        event = "autonomy_budget_exhausted",
        reason, autonomous_turns, "autonomy budget exhausted; staying idle until shutdown"
    );
    rt.shared.evidence.record(
        "autonomy_budget_exhausted",
        json!({"reason": reason, "autonomous_turns": autonomous_turns}),
    );
}

/// Cancel the active turn, then terminate its whole process tree and wait for
/// it to disappear before any new turn can start. Nothing is replayed.
async fn cancel_active_turn(active: &mut Option<ActiveTurn>, reason: &str, rt: &Runtime) {
    let Some(mut turn) = active.take() else {
        return;
    };
    info!(kind = ?turn.kind, reason, "cancelling active Claude turn");
    turn.handle.abort();
    let _ = (&mut turn.handle).await;
    rt.shared.reap_and_reconcile().await;
    record_turn_finished(rt, &turn, None);
    info!(event = "turn_cancelled", kind = ?turn.kind, reason, "Claude turn cancelled and its process tree reaped");
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn run_buddy(
    args: Arc<Args>,
    jev: Option<Arc<JevClient>>,
    local_server: &mut Option<LocalServer>,
) -> Result<()> {
    let mcp = find_mcp(args.mcp_bin.clone())?;
    let config_path = write_mcp_config(&args, &mcp)?;
    let config: Arc<str> = config_path.to_string_lossy().into_owned().into();
    let input = args
        .script_output
        .clone()
        .unwrap_or_else(default_script_output)
        .join("claude-chat/input.jsonl");
    let cursor = args.write_data.join(format!("inbox-{}.cursor", args.agent));
    let mut inbox = Inbox::new(input.clone(), cursor)?;
    let label = args.label.clone().unwrap_or_else(|| args.agent.clone());
    let lifecycle = Arc::new(LifecycleClient::new(&args)?);
    let evidence = EvidenceLog::open(args.evidence_log.as_deref())?;

    lifecycle
        .call("ping", &[])
        .await
        .context("Factorio Buddy mod is not reachable")?;
    lifecycle
        .call("register_agent", &[json!(args.agent), json!(label)])
        .await?;
    lifecycle
        .call(
            "pre_place_character_result",
            &[json!(args.agent), json!("nauvis"), json!(0)],
        )
        .await?;

    let rt = Arc::new(Runtime {
        args: Arc::clone(&args),
        config,
        lifecycle: Arc::clone(&lifecycle),
        shared: Shared::new(evidence),
        mcp: mcp.clone(),
        jev,
        failed_repairs: StdMutex::new(HashSet::new()),
        inbox_path: input.clone(),
        inbox_offset: AtomicU64::new(inbox.offset),
    });

    info!(
        agent = %args.agent,
        input = %input.display(),
        mcp = %mcp.display(),
        model = args.model.as_deref().unwrap_or("default"),
        effort = %args.effort,
        heartbeat_seconds = args.heartbeat_seconds,
        turn_timeout_seconds = args.turn_timeout_seconds,
        decision_mode = args.decision_mode.as_str(),
        max_autonomous_turns = args.max_autonomous_turns,
        autonomy_deadline_seconds = args.autonomy_deadline_seconds,
        "Factorio buddy online"
    );
    let online_at = Instant::now();
    let mut timer = interval(Duration::from_millis(500));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    timer.tick().await;
    let heartbeat = Duration::from_secs(args.heartbeat_seconds.max(1));
    let mut next_autonomy = Instant::now() + heartbeat;
    let mut session_id = None;
    let mut provider_retry_at = None;
    let mut pending: VecDeque<TurnRequest> = VecDeque::new();
    let mut active: Option<ActiveTurn> = None;
    let mut autonomous_turns: u64 = 0;
    let mut autonomy_exhausted = false;
    // Opus always plans first; at most one maintenance action between Opus turns.
    let mut maintenance_allowed = false;
    let autonomy_request = |kind: TurnKind| TurnRequest {
        kind,
        prompt: None,
        player_index: 0,
        response_agent: args.agent.clone(),
    };
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        while active.is_none() {
            let Some(request) = pending.pop_front() else {
                break;
            };
            if request.kind != TurnKind::Human {
                // The deadline is also checked here, not only on the timer
                // tick, so a follow-up turn queued by maintenance cannot start
                // after it has passed.
                if !autonomy_exhausted && autonomy_deadline_reached(&args, online_at) {
                    autonomy_exhausted = true;
                    pending.retain(|request| request.kind == TurnKind::Human);
                    record_autonomy_exhausted(&rt, "autonomy_deadline", autonomous_turns);
                }
                let budget_left = !autonomy_exhausted
                    && (args.max_autonomous_turns == 0
                        || autonomous_turns < args.max_autonomous_turns);
                if !budget_left {
                    continue;
                }
                if request.kind == TurnKind::Autonomy {
                    autonomous_turns += 1;
                }
            }
            info!(kind = ?request.kind, "starting queued Claude turn");
            active = Some(start_turn(&rt, request, session_id.clone()));
        }

        tokio::select! {
            _ = &mut shutdown => {
                cancel_active_turn(&mut active, "shutdown", &rt).await;
                break;
            }
            completion = async {
                let handle = &mut active
                    .as_mut()
                    .expect("active turn guarded by select condition")
                    .handle;
                handle.await
            }, if active.is_some() => {
                let turn = active.take().expect("completed active turn");
                let kind = turn.kind;
                match completion {
                    Ok(completion) => {
                        record_turn_finished(&rt, &turn, Some(&completion));
                        session_id = completion.session_id.clone();
                        if completion.provider_limited {
                            session_id = None;
                            provider_retry_at = Some(
                                Instant::now()
                                    + Duration::from_secs(PROVIDER_LIMIT_RETRY_SECONDS),
                            );
                            // Every queued human request gets a terminal answer
                            // before the queue is dropped.
                            for request in pending.drain(..) {
                                if request.kind != TurnKind::Human {
                                    continue;
                                }
                                match lifecycle
                                    .call(
                                        "receive_response",
                                        &[
                                            json!(request.player_index),
                                            json!(request.response_agent),
                                            json!(PROVIDER_LIMIT_MESSAGE),
                                        ],
                                    )
                                    .await
                                {
                                    Ok(_) => info!(
                                        event = "provider_limit_queued_response",
                                        player_index = request.player_index,
                                        "answered a queued player request with the provider-limit message"
                                    ),
                                    Err(error) => warn!(
                                        %error,
                                        "failed to send provider-limit response for a queued request"
                                    ),
                                }
                            }
                            warn!(
                                retry_seconds = PROVIDER_LIMIT_RETRY_SECONDS,
                                "Claude provider usage limit active; pausing autonomous turns"
                            );
                        } else if completion.succeeded == Some(true) {
                            provider_retry_at = None;
                        }
                        if kind == TurnKind::Autonomy {
                            maintenance_allowed = true;
                            // Opus has replanned: failed consumers are eligible again.
                            rt.failed_repairs
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .clear();
                        }
                        if kind == TurnKind::Maintenance
                            && completion.follow_with_opus
                            && pending.is_empty()
                        {
                            pending.push_back(autonomy_request(TurnKind::Autonomy));
                        }
                        let succeeded = completion
                            .succeeded
                            .map_or_else(|| "not_executed".to_owned(), |ok| ok.to_string());
                        info!(?kind, succeeded = %succeeded, "Claude turn finished");
                    }
                    Err(error) if error.is_cancelled() => {
                        record_turn_finished(&rt, &turn, None);
                        info!(?kind, "Claude turn cancelled");
                    }
                    Err(error) => {
                        record_turn_finished(&rt, &turn, None);
                        warn!(?kind, %error, "Claude turn task failed");
                    }
                }
                next_autonomy = Instant::now() + heartbeat;
            }
            _ = timer.tick() => {
                if let Some(server) = local_server.as_mut() {
                    if let Some(status) = server.try_wait()? {
                        cancel_active_turn(&mut active, "Factorio server exited", &rt).await;
                        bail!("owned Factorio server exited unexpectedly: {status}");
                    }
                }

                let messages = inbox.poll().unwrap_or_else(|error| {
                    warn!(%error, "failed to read Factorio chat inbox");
                    Vec::new()
                });
                rt.inbox_offset.store(inbox.offset, Ordering::SeqCst);
                let now = Instant::now();
                let provider_unavailable = provider_limit_active(provider_retry_at, now);
                if provider_retry_at.is_some() && !provider_unavailable {
                    provider_retry_at = None;
                    info!("Claude provider retry interval elapsed; the next turn will start a fresh session");
                }
                let mut received_human_message = false;
                for message in messages {
                    if message.target_agent != args.agent && message.target_agent != "all" {
                        continue;
                    }
                    let target = message
                        .response_to
                        .as_deref()
                        .unwrap_or(&args.agent)
                        .to_owned();
                    info!(
                        message_id = message.id,
                        player_index = message.player_index,
                        target_agent = %message.target_agent,
                        "received player message"
                    );
                    if provider_unavailable {
                        match lifecycle
                            .call(
                                "receive_response",
                                &[
                                    json!(message.player_index),
                                    json!(target),
                                    json!(PROVIDER_LIMIT_MESSAGE),
                                ],
                            )
                            .await
                        {
                            Ok(_) => info!(
                                event = "provider_unavailable_response",
                                player_index = message.player_index,
                                "responded to player without starting Claude while its usage limit is active"
                            ),
                            Err(error) => warn!(
                                %error,
                                "failed to send provider-unavailable response to Factorio"
                            ),
                        }
                        continue;
                    }
                    received_human_message = true;
                    pending.push_back(TurnRequest {
                        kind: TurnKind::Human,
                        prompt: Some(message.message),
                        player_index: message.player_index,
                        response_agent: target,
                    });
                }
                if received_human_message {
                    // Humans are FIFO and never preempt each other; only
                    // autonomous and maintenance work yields to new chat.
                    pending.retain(|request| request.kind == TurnKind::Human);
                    if active.as_ref().is_some_and(|turn| turn.kind != TurnKind::Human) {
                        cancel_active_turn(&mut active, "player message", &rt).await;
                    }
                    next_autonomy = Instant::now() + heartbeat;
                    continue;
                }

                if !autonomy_exhausted {
                    let deadline_reached = autonomy_deadline_reached(&args, online_at);
                    let autonomous_active =
                        active.as_ref().is_some_and(|turn| turn.kind != TurnKind::Human);
                    let turns_reached = args.max_autonomous_turns > 0
                        && autonomous_turns >= args.max_autonomous_turns
                        && !autonomous_active;
                    if deadline_reached || turns_reached {
                        autonomy_exhausted = true;
                        let reason = if deadline_reached {
                            "autonomy_deadline"
                        } else {
                            "max_autonomous_turns"
                        };
                        pending.retain(|request| request.kind == TurnKind::Human);
                        if deadline_reached && autonomous_active {
                            cancel_active_turn(&mut active, reason, &rt).await;
                        }
                        record_autonomy_exhausted(&rt, reason, autonomous_turns);
                    }
                }

                if autonomy_exhausted
                    || active.is_some()
                    || !pending.is_empty()
                    || args.heartbeat_seconds == 0
                    || provider_unavailable
                    || Instant::now() < next_autonomy
                {
                    continue;
                }
                let kind = if args.decision_mode != DecisionMode::Off && maintenance_allowed {
                    maintenance_allowed = false;
                    TurnKind::Maintenance
                } else {
                    TurnKind::Autonomy
                };
                pending.push_back(autonomy_request(kind));
                next_autonomy = Instant::now() + heartbeat;
            }
        }
    }
    Ok(())
}

/// Build the Jev client for Jev decision modes. Missing credentials fail
/// startup; `off` and `deterministic` never need them.
fn jev_client(mode: DecisionMode) -> Result<Option<Arc<JevClient>>> {
    if !mode.uses_jev() {
        return Ok(None);
    }
    let api_key = std::env::var("TYPESAFE_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .with_context(|| {
            format!(
                "--decision-mode {} requires the TYPESAFE_API_KEY environment variable",
                mode.as_str()
            )
        })?;
    let url = std::env::var("TYPESAFE_API_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| decision::DEFAULT_JEV_URL.to_owned());
    let budget = match std::env::var("BUDDY_JEV_BUDGET_USD") {
        Ok(value) => value
            .trim()
            .parse::<f64>()
            .context("BUDDY_JEV_BUDGET_USD must be a number")?,
        Err(_) => 5.0,
    };
    Ok(Some(Arc::new(JevClient::new(url, api_key, budget)?)))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .init();
    let mut args = Args::parse();
    AgentId::new(Some(&args.agent)).context("invalid --agent")?;
    let jev = jev_client(args.decision_mode)?;
    if args.start_server && args.script_output.is_none() {
        args.script_output = Some(args.write_data.join("script-output"));
    }
    let _lease =
        ControllerLease::acquire(args.write_data.join(format!("buddy-{}.lock", args.agent)))?;
    let _server_lease = if args.start_server {
        Some(ControllerLease::acquire(
            args.write_data.join("buddy-server.lock"),
        )?)
    } else {
        None
    };
    configure_rcon_password(&mut args)?;
    let mut local_server = if args.start_server {
        Some(start_local_server(&mut args).await?)
    } else {
        None
    };
    let result = run_buddy(Arc::new(args), jev, &mut local_server).await;
    if let Some(server) = local_server.take() {
        info!("stopping Factorio server");
        server.stop().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const RUN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn write_owner(
        path: &Path,
        save: &Path,
        managed_runs_root: &Path,
        run_id: &str,
        clean_shutdown: bool,
    ) -> PathBuf {
        let run_directory = managed_runs_root.join(run_id);
        std::fs::create_dir_all(run_directory.join("saves")).unwrap();
        let owner = SaveOwner {
            version: 2,
            primary_save: normalized_path(save)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            primary_identity: save_identity(save).unwrap(),
            run_id: run_id.to_owned(),
            run_directory: normalized_path(&run_directory)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            clean_shutdown,
        };
        write_json_atomic(path, &owner, false).unwrap();
        run_directory
    }

    fn append(path: &Path, contents: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(contents).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn parses_valid_jsonl_and_ignores_noise() {
        let messages = parse_input(
            "noise\n{\"message\":\"mine coal\",\"player_index\":4}\n{\"message\":\"\"}\n",
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "mine coal");
        assert_eq!(messages[0].player_index, 4);
    }

    #[test]
    fn reads_session_id_from_stream_events() {
        let event = json!({"type": "system", "subtype": "init", "session_id": "abc-123"});
        assert_eq!(stream_event_session_id(&event), Some("abc-123"));
        assert_eq!(stream_event_session_id(&json!({"type": "assistant"})), None);
    }

    #[test]
    fn provider_limit_classification_requires_a_terminal_limit_message() {
        for terminal in [
            "API Error: Request rejected (429) - Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-07-20 01:09:22",
            "Usage limit reached for 5 hour. Your limit will reset at 2026-07-18 13:00:00",
            // Observed from Claude Code 2.1.281 during the model trials.
            "You've hit your session limit · resets 4am (America/New_York)",
        ] {
            assert!(is_provider_usage_limit(terminal), "{terminal}");
        }

        for non_terminal in [
            r#"{"type":"system","subtype":"api_retry","error":"unknown","error_status":null}"#,
            r#"{"type":"system","subtype":"status","status":"compacting"}"#,
            "context window limit reached",
            "rate limit retry from SDK",
        ] {
            assert!(!is_provider_usage_limit(non_terminal), "{non_terminal}");
        }
    }

    #[test]
    fn invocation_ending_on_http_429_is_a_provider_limit() {
        let parse = |value: Value| serde_json::from_value::<ClaudeResult>(value).unwrap();
        let observed = parse(json!({
            "type": "result", "subtype": "success", "is_error": true,
            "api_error_status": 429, "result": "rejected", "session_id": "s"
        }));
        assert!(observed.is_provider_limit());
        let tool_failure = parse(json!({
            "type": "result", "subtype": "success", "is_error": true,
            "result": "rate limit retry from SDK", "session_id": "s"
        }));
        assert!(!tool_failure.is_provider_limit());
        let success = parse(json!({
            "type": "result", "subtype": "success", "is_error": false,
            "api_error_status": 429, "result": "done", "session_id": "s"
        }));
        assert!(!success.is_provider_limit());
    }

    #[test]
    fn provider_limit_backoff_expires_without_persisted_state() {
        let now = Instant::now();
        let retry_at = now + Duration::from_secs(PROVIDER_LIMIT_RETRY_SECONDS);
        assert!(provider_limit_active(Some(retry_at), now));
        assert!(!provider_limit_active(Some(retry_at), retry_at));
        assert!(!provider_limit_active(None, now));
    }

    #[test]
    fn hides_only_routine_rcon_connection_lines() {
        assert!(!should_forward_factorio_output(
            "Info RemoteCommandProcessor.cpp:245: New RCON connection from 127.0.0.1"
        ));
        assert!(should_forward_factorio_output(
            "Error RemoteCommandProcessor.cpp: RCON authentication failed"
        ));
        assert!(should_forward_factorio_output("Joining game"));
    }

    #[test]
    fn normal_factorio_chat_wakes_buddy_and_responses_do_not_require_the_gui() {
        let control = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/mod/claude-interface/control.lua"
        ));
        let chat_start = control
            .find("script.on_event(defines.events.on_console_chat")
            .expect("console chat handler");
        let chat_end = control[chat_start..]
            .find("-- Hotkey toggle")
            .map(|offset| chat_start + offset)
            .expect("end of console chat handler");
        assert!(control[chat_start..chat_end].contains("write_bridge_message("));

        let display_start = control
            .find("local function add_chat_message")
            .expect("chat display helper");
        let display_end = control[display_start..]
            .find("local function set_status")
            .map(|offset| display_start + offset)
            .expect("end of chat display helper");
        let display = &control[display_start..display_end];
        let print = display.find("player.print(").expect("console delivery");
        let gui_guard = display
            .find("player.gui.screen[GUI_FRAME]")
            .expect("optional GUI delivery");
        assert!(
            print < gui_guard,
            "console delivery must precede the GUI guard"
        );
    }

    #[test]
    fn default_prompt_requires_complete_belt_routes() {
        assert!(DEFAULT_SYSTEM_PROMPT
            .contains("Build belts as complete source-to-destination routes with route_belt"));
        assert!(
            DEFAULT_SYSTEM_PROMPT.contains("do not improvise disconnected one-tile belt fragments")
        );
    }

    #[test]
    fn default_prompt_treats_resource_overlap_as_advisory_and_keeps_belt_contents_explicit() {
        for required in [
            "future extraction capacity, not forbidden terrain",
            "Resource overlap is advisory, not a placement veto",
            "temporary bootstrap structures and compact transport",
            "Do not refuse useful automation or build a wasteful detour",
            "Only extraction machinery is resource-category constrained",
            "accept a Factorio-buildable output tile even when it contains ore",
            "Prefer dedicated item belts or deliberate lane separation",
            "never assume a branch is pure",
            "configure the receiving inserter's whitelist",
            "do not mistake a filtered inserter for a pure upstream belt",
        ] {
            assert!(
                DEFAULT_SYSTEM_PROMPT.contains(required),
                "default gameplay prompt should include {required:?}"
            );
        }
    }

    #[test]
    fn default_new_game_map_uses_peaceful_mode_and_published_seed() {
        let settings: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/configs/map-gen.json"
        )))
        .expect("default map generation settings should be valid JSON");
        assert_eq!(
            settings
                .get("peaceful_mode")
                .and_then(|value| value.as_bool()),
            Some(true),
            "just play must create peaceful default maps"
        );
        assert_eq!(
            settings.get("seed").and_then(|value| value.as_u64()),
            Some(u64::from(DEFAULT_MAP_SEED)),
            "the checked-in map settings should document the default new-game seed"
        );

        let overridden = Args::try_parse_from(["buddy", "--map-seed", "42"]).unwrap();
        assert_eq!(overridden.map_seed, 42);
    }

    #[test]
    fn managed_server_tolerates_background_client_stalls() {
        let config = managed_factorio_config(Path::new("/factorio"), Path::new("/buddy-run"));
        assert!(config.contains("read-data=/factorio/data"));
        assert!(config.contains("write-data=/buddy-run"));
        assert!(config.contains(&format!(
            "drop-detection-threshold-time={MANAGED_CLIENT_DROP_THRESHOLD_SECONDS}"
        )));
    }

    #[test]
    fn autonomy_prompt_includes_snapshot() {
        let prompt = autonomy_prompt(r#"{"research":{"research_progress":0.5}}"#);
        assert!(prompt.contains("\"research_progress\": 0.5"));
    }

    #[test]
    fn missing_inbox_cursor_replays_queued_complete_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        std::fs::write(
            &path,
            b"{\"id\":1,\"message\":\"first\"}\n{\"id\":2,\"message\":\"second\"}\n",
        )
        .unwrap();

        let mut inbox = Inbox::new(path, cursor).unwrap();
        let messages = inbox.poll().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, Some(1));
        assert_eq!(messages[1].message, "second");
    }

    #[test]
    fn inbox_does_not_consume_partial_jsonl_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        let mut inbox = Inbox::new(path.clone(), cursor).unwrap();

        append(&path, b"{\"id\":7,\"message\":\"still writing\"");
        assert!(inbox.poll().unwrap().is_empty());
        assert_eq!(inbox.offset, 0);
        append(&path, b"}\n");
        let messages = inbox.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, Some(7));
        assert_eq!(messages[0].message, "still writing");
    }

    #[test]
    fn inbox_resumes_from_durable_cursor_after_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        append(&path, b"{\"message\":\"first\"}\n");
        let mut first = Inbox::new(path.clone(), cursor.clone()).unwrap();
        assert_eq!(first.poll().unwrap().len(), 1);
        append(&path, b"{\"message\":\"second\"}\n");

        let mut resumed = Inbox::new(path, cursor).unwrap();
        let messages = resumed.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "second");
    }

    #[test]
    fn inbox_replays_a_recreated_shorter_file_instead_of_clamping_to_its_end() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.jsonl");
        let cursor = directory.path().join("cursor");
        append(
            &path,
            b"{\"message\":\"a deliberately long message in the original file\"}\n",
        );
        let mut first = Inbox::new(path.clone(), cursor.clone()).unwrap();
        assert_eq!(first.poll().unwrap().len(), 1);
        std::fs::write(&path, b"{\"message\":\"new\"}\n").unwrap();

        let mut recreated = Inbox::new(path, cursor).unwrap();
        let messages = recreated.poll().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "new");
    }

    #[test]
    fn claude_prompt_is_separated_from_options_and_settings_are_isolated() {
        let args = Args::try_parse_from(["buddy"]).unwrap();
        let arguments = claude_arguments(&args, "{}", "--help", Some("session-id"))
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&arguments[arguments.len() - 2..], ["--", "--help"]);
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--setting-sources", ""]));
        assert!(arguments
            .windows(2)
            .any(|pair| pair == ["--disallowedTools", "mcp__factorio__execute_lua"]));
    }

    #[test]
    fn operator_persona_is_appended_without_replacing_gameplay_rules() {
        let args = Args::try_parse_from([
            "buddy",
            "--persona",
            "  Build boldly, repair root causes, and expand throughput.  ",
        ])
        .unwrap();
        let prompt = effective_system_prompt(&args);
        assert!(prompt.starts_with(DEFAULT_SYSTEM_PROMPT));
        assert!(prompt.contains("\n\nOperator-defined persona:\n"));
        assert!(prompt.ends_with("Build boldly, repair root causes, and expand throughput."));

        let arguments = claude_arguments(&args, "{}", "play", None)
            .into_iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let system_prompt = arguments
            .windows(2)
            .find(|pair| pair[0] == "--system-prompt")
            .map(|pair| pair[1].as_str())
            .expect("Claude arguments should include the effective system prompt");
        assert_eq!(system_prompt, prompt);
    }

    #[test]
    fn blank_operator_persona_does_not_change_the_system_prompt() {
        let args = Args::try_parse_from(["buddy", "--persona", "   "]).unwrap();
        assert_eq!(effective_system_prompt(&args), DEFAULT_SYSTEM_PROMPT);
    }

    #[test]
    fn companion_autonomy_has_no_player_presence_switch() {
        let args = Args::try_parse_from(["buddy"]).unwrap();
        assert_eq!(args.heartbeat_seconds, 30);
        assert_eq!(args.turn_timeout_seconds, 0);
        assert!(Args::try_parse_from(["buddy", "--autonomy-requires-player"]).is_err());
    }

    #[test]
    fn lifecycle_rejects_structured_failure_and_bad_character_status() {
        assert!(validate_lifecycle_response("ping", "not-pong").is_err());
        assert!(validate_lifecycle_response(
            "register_agent",
            r#"{"success":false,"error_kind":"unknown_function","error":"old mod"}"#
        )
        .is_err());
        assert!(validate_lifecycle_response(
            "pre_place_character_result",
            r#"{"status":"creation_failed"}"#
        )
        .is_err());
        assert!(validate_lifecycle_response(
            "pre_place_character_result",
            r#"{"status":"teleported"}"#
        )
        .is_err());
        validate_lifecycle_response("pre_place_character_result", r#"{"status":"created"}"#)
            .unwrap();
        validate_lifecycle_response("receive_response", "").unwrap();
    }

    #[test]
    fn controller_lease_excludes_a_second_controller() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("buddy.lock");
        let first = ControllerLease::acquire(path.clone()).unwrap();
        assert!(ControllerLease::acquire(path.clone()).is_err());
        drop(first);
        ControllerLease::acquire(path).unwrap();
    }

    fn test_runtime(evidence: Option<&Path>) -> Arc<Runtime> {
        let mut args = Args::try_parse_from(["buddy"]).unwrap();
        args.rcon_password = Some("test".to_owned());
        let args = Arc::new(args);
        Arc::new(Runtime {
            lifecycle: Arc::new(LifecycleClient::new(&args).unwrap()),
            args,
            config: Arc::from("{}"),
            shared: Shared::new(EvidenceLog::open(evidence).unwrap()),
            mcp: PathBuf::from("/nonexistent"),
            jev: None,
            failed_repairs: StdMutex::new(HashSet::new()),
            inbox_path: PathBuf::from("/nonexistent"),
            inbox_offset: AtomicU64::new(0),
        })
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancellation_reaps_the_whole_process_tree_and_marks_in_flight_tools_unknown() {
        let directory = tempfile::tempdir().unwrap();
        let evidence = directory.path().join("evidence.jsonl");
        let rt = test_runtime(Some(&evidence));
        // A child that spawns a grandchild, like Claude spawning its MCP server.
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 300 & sleep 300"])
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let pgid = child.id().unwrap() as i32;
        rt.shared.lock().process_group = Some(pgid);
        rt.shared.tool_started(
            "toolu_1",
            "mcp__factorio__place_entity",
            &json!({"b": 1, "a": 2}),
        );
        let handle = tokio::spawn(async move {
            let _child = child;
            std::future::pending::<TurnCompletion>().await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(process_group_alive(pgid));
        let mut active = Some(ActiveTurn {
            kind: TurnKind::Autonomy,
            started: Instant::now(),
            handle,
        });
        cancel_active_turn(&mut active, "test player message", &rt).await;
        assert!(active.is_none());
        assert!(
            !process_group_alive(pgid),
            "grandchild survived cancellation"
        );
        let (note, delivered) = rt.shared.peek_unknown_outcome_note().unwrap();
        assert!(note.contains(r#"place_entity {"a":2,"b":1}"#), "{note}");
        // Peeking does not consume: a cancelled delivery keeps the note.
        assert!(rt.shared.peek_unknown_outcome_note().is_some());
        rt.shared.acknowledge_unknown_outcomes(&delivered);
        assert!(rt.shared.peek_unknown_outcome_note().is_none());
        let events: Vec<Value> = std::fs::read_to_string(&evidence)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events
            .iter()
            .any(|event| event["event"] == "interrupted_tool" && event["tool"] == "place_entity"));
        assert!(events.iter().any(|event| event["event"] == "turn_finished"
            && event["cancelled"] == true
            && event["tool_calls"] == 1));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn aborted_reap_keeps_the_group_for_the_next_reap() {
        let rt = test_runtime(None);
        // The whole group ignores SIGTERM, so termination needs SIGKILL.
        let mut command = Command::new("sh");
        command
            .args(["-c", "trap '' TERM; sleep 300 & wait"])
            .process_group(0)
            .kill_on_drop(true);
        let _child = command.spawn().unwrap();
        let pgid = _child.id().unwrap() as i32;
        rt.shared.lock().process_group = Some(pgid);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let reaper = {
            let rt = Arc::clone(&rt);
            tokio::spawn(async move { rt.shared.reap_and_reconcile().await })
        };
        tokio::time::sleep(Duration::from_millis(300)).await;
        reaper.abort();
        let _ = reaper.await;
        assert_eq!(rt.shared.lock().process_group, Some(pgid));
        assert!(process_group_alive(pgid), "SIGTERM alone must not kill it");
        rt.shared.reap_and_reconcile().await;
        assert!(!process_group_alive(pgid));
        assert_eq!(rt.shared.lock().process_group, None);
    }

    fn failure(tool: &str, error: &str) -> ToolOutcome {
        ToolOutcome {
            tool: tool.to_owned(),
            arguments: json!({"x": 1}),
            is_error: true,
            error: Some(error.to_owned()),
            tick: None,
        }
    }

    #[test]
    fn three_identical_failures_without_success_trigger_a_replan_hint() {
        let mut outcomes = RecentOutcomes::default();
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), None);
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), None);
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), Some(3));
        assert!(outcomes.summary().unwrap().contains("REPEATED FAILURE"));

        // An intervening success or a different error breaks the streak.
        let mut outcomes = RecentOutcomes::default();
        outcomes.push(failure("mine_at", "no resource"));
        outcomes.push(failure("mine_at", "no resource"));
        let mut success = failure("mine_at", "");
        success.is_error = false;
        success.error = None;
        outcomes.push(success);
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), None);
        assert_eq!(outcomes.push(failure("mine_at", "inventory full")), None);
        assert!(!outcomes.summary().unwrap().contains("REPEATED FAILURE"));
    }

    #[test]
    fn manual_logistics_alert_needs_a_full_window_at_the_threshold_share() {
        let call = |tool: &str| {
            let mut outcome = failure(tool, "");
            outcome.is_error = false;
            outcome
        };
        let mut recent = RecentOutcomes::default();
        // 17 hand transfers in a full window of 60 stay below 30 %.
        for index in 0..MANUAL_TRANSFER_WINDOW {
            recent.push(call(if index >= MANUAL_TRANSFER_WINDOW - 17 {
                "bootstrap_burner_once"
            } else {
                "build_layout"
            }));
        }
        assert_eq!(recent.manual_transfer_pressure(), None);
        assert!(!recent.summary().unwrap().contains("MANUAL LOGISTICS"));
        // One more transfer displaces a build call: 18 of 60 is exactly 30 %.
        recent.push(call("collect_from_chest"));
        assert_eq!(recent.manual_transfer_pressure(), Some(18));
        assert!(recent
            .summary()
            .unwrap()
            .contains("MANUAL LOGISTICS: 18 of the last 60"));
        // Failed transfers moved nothing and do not count.
        let mut failed = RecentOutcomes::default();
        for _ in 0..MANUAL_TRANSFER_WINDOW {
            failed.push(failure("collect_from_chest", "item_not_found"));
        }
        assert_eq!(failed.manual_transfer_pressure(), None);
        // A short session never alerts, however manual it is.
        let mut short = RecentOutcomes::default();
        for _ in 0..MANUAL_TRANSFER_WINDOW - 1 {
            short.push(call("bootstrap_burner_once"));
        }
        assert_eq!(short.manual_transfer_pressure(), None);
    }

    #[test]
    fn fail_inspect_loop_is_a_repeated_failure() {
        let inspect = || {
            let mut outcome = failure("get_entity_inventory", "");
            outcome.is_error = false;
            outcome.error = None;
            outcome
        };
        let mut outcomes = RecentOutcomes::default();
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), None);
        outcomes.push(inspect());
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), None);
        outcomes.push(inspect());
        assert_eq!(outcomes.push(failure("mine_at", "no resource")), Some(3));
        // The hint survives a trailing inspection in the next summary.
        outcomes.push(inspect());
        let summary = outcomes.summary().unwrap();
        assert!(summary.contains("REPEATED FAILURE: mine_at"), "{summary}");
    }

    #[test]
    fn session_reset_classifier_ignores_generic_stderr_words() {
        assert!(!is_invalid_session_error(&anyhow::anyhow!(
            "claude exited 1; stderr tail: MCP session unavailable: invalid token"
        )));
    }

    #[test]
    fn recent_outcomes_are_bounded_to_the_last_eight() {
        let mut outcomes = RecentOutcomes::default();
        for index in 0..12 {
            outcomes.push(failure(&format!("tool{index}"), "e"));
        }
        assert_eq!(outcomes.entries.len(), RECENT_OUTCOME_LIMIT);
        assert_eq!(outcomes.entries.front().unwrap().tool, "tool4");
    }

    #[test]
    fn tool_results_are_classified_with_mcp_semantics() {
        assert_eq!(
            classify_tool_result(false, r#"{"success":true,"tick":42}"#),
            (false, None, Some(42))
        );
        assert_eq!(
            classify_tool_result(
                false,
                r#"{"success":false,"error_kind":"no_ready_fuel_transaction"}"#
            )
            .1,
            Some("no_ready_fuel_transaction".to_owned())
        );
        assert_eq!(
            classify_tool_result(false, r#"{"error":"blocked"}"#).1,
            Some("blocked".to_owned())
        );
        assert!(!classify_tool_result(false, r#"{"error":null,"ok":1}"#).0);
        assert!(!classify_tool_result(false, r#"{"error":""}"#).0);
        assert_eq!(
            classify_tool_result(false, "Error: not connected\nmore").1,
            Some("not connected".to_owned())
        );
        assert!(classify_tool_result(true, "anything").0);
        assert!(!classify_tool_result(false, "Mined 5 coal").0);
    }

    #[test]
    fn stderr_only_missing_conversation_is_an_invalid_session() {
        let error = anyhow::anyhow!(
            "claude exited exit status: 1 without a valid result; stderr tail: No conversation found with session ID: abc"
        );
        assert!(is_invalid_session_error(&error));
        assert!(!is_invalid_session_error(&anyhow::anyhow!(
            "claude returned an error: tool failed"
        )));
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let mut tail = StderrTail::new();
        for index in 0..100 {
            tail.push(format!("line {index} {}", "x".repeat(100)));
        }
        let rendered = tail.render();
        assert!(rendered.len() <= STDERR_TAIL_BYTES + 4);
        assert!(rendered.contains("line 99"));
        assert!(!rendered.contains("line 0 "));
    }

    #[test]
    fn owned_server_password_is_generated_persisted_and_private() {
        let directory = tempfile::tempdir().unwrap();
        let mut args = Args::try_parse_from([
            "buddy",
            "--start-server",
            "--write-data",
            directory.path().to_str().unwrap(),
        ])
        .unwrap();
        configure_rcon_password(&mut args).unwrap();
        let password = args.rcon_password.as_deref().unwrap();
        let mut resumed = args.clone();
        resumed.rcon_password = None;
        configure_rcon_password(&mut resumed).unwrap();
        assert_eq!(resumed.rcon_password.as_deref(), Some(password));
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            std::fs::read_to_string(password_path(directory.path()))
                .unwrap()
                .trim(),
            password
        );
        assert_eq!(args.rcon_host, "127.0.0.1");
        let config_path = write_mcp_config(&args, Path::new("/tmp/factorio-mcp")).unwrap();
        let arguments = claude_arguments(&args, &config_path.to_string_lossy(), "play", None);
        assert!(arguments
            .iter()
            .all(|argument| !argument.to_string_lossy().contains(password)));
        assert!(std::fs::read_to_string(&config_path)
            .unwrap()
            .contains(password));
        assert!(std::fs::read_to_string(&config_path)
            .unwrap()
            .contains(env!("CARGO_MANIFEST_DIR")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(password_path(directory.path()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(config_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn mod_install_replaces_complete_tree_and_skips_identical_content() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let mods = directory.path().join("mods");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("info.json"), b"v1").unwrap();
        std::fs::write(source.join("nested/control.lua"), b"return 1").unwrap();

        assert!(install_mod_source(&source, &mods).unwrap());
        assert!(trees_equal(&source, &mods.join("claude-interface")).unwrap());
        assert!(!install_mod_source(&source, &mods).unwrap());
        std::fs::write(source.join("info.json"), b"v2").unwrap();
        assert!(install_mod_source(&source, &mods).unwrap());
        assert_eq!(
            std::fs::read(mods.join("claude-interface/info.json")).unwrap(),
            b"v2"
        );
        assert!(std::fs::read_dir(&mods).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')));
    }

    #[test]
    fn resume_promotes_only_owned_newer_autosave_and_preserves_stale_primary() {
        let directory = tempfile::tempdir().expect("tempdir");
        let save = directory.path().join("buddy.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"stale").expect("write primary");
        let run = write_owner(&owner, &save, &managed_runs, RUN_A, false);
        let autosave = run.join("saves/_autosave2.zip");
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(&autosave, b"newer").expect("write autosave");

        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            Some(autosave.clone())
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"newer");
        assert_eq!(
            std::fs::read(directory.path().join("buddy.previous.zip")).unwrap(),
            b"stale"
        );
    }

    #[test]
    fn resume_refuses_clean_or_different_save_autosaves() {
        let directory = tempfile::tempdir().expect("tempdir");
        let save = directory.path().join("buddy.zip");
        let other = directory.path().join("other.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"current").unwrap();
        std::fs::write(&other, b"other").unwrap();

        let run = write_owner(&owner, &save, &managed_runs, RUN_A, true);
        let autosave = run.join("saves/_autosave1.zip");
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(&autosave, b"new autosave").unwrap();

        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            None
        );
        write_owner(&owner, &other, &managed_runs, RUN_B, false);
        assert_eq!(
            promote_owned_autosave(&save, &owner, &managed_runs).unwrap(),
            None
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"current");
        assert!(!directory.path().join("buddy.previous.zip").exists());
    }

    #[test]
    fn resume_refuses_recovery_over_replaced_primary_content() {
        let directory = tempfile::tempdir().unwrap();
        let save = directory.path().join("buddy.zip");
        let managed_runs = directory.path().join("managed-runs");
        let owner_path = save_owner_path(&save).unwrap();
        std::fs::write(&save, b"current").unwrap();
        let run = write_owner(&owner_path, &save, &managed_runs, RUN_A, false);
        std::fs::write(&save, b"replacement save").unwrap();
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(run.join("saves/_autosave1.zip"), b"owned autosave").unwrap();

        assert_eq!(
            promote_owned_autosave(&save, &owner_path, &managed_runs).unwrap(),
            None
        );
        assert_eq!(std::fs::read(&save).unwrap(), b"replacement save");
    }

    #[test]
    fn per_primary_sidecars_and_run_namespaces_prevent_competing_save_contamination() {
        let directory = tempfile::tempdir().unwrap();
        let managed_runs = directory.path().join("managed-runs");
        let save_a = directory.path().join("alpha.zip");
        let save_b = directory.path().join("beta.zip");
        std::fs::write(&save_a, b"alpha primary").unwrap();
        std::fs::write(&save_b, b"beta primary").unwrap();
        let owner_a = save_owner_path(&save_a).unwrap();
        let owner_b = save_owner_path(&save_b).unwrap();
        assert_ne!(owner_a, owner_b);

        let run_a = write_owner(&owner_a, &save_a, &managed_runs, RUN_A, false);
        let run_b = write_owner(&owner_b, &save_b, &managed_runs, RUN_B, false);
        std::thread::sleep(Duration::from_millis(10));
        let autosave_a = run_a.join("saves/_autosave1.zip");
        std::fs::write(&autosave_a, b"alpha autosave").unwrap();
        std::thread::sleep(Duration::from_millis(10));
        std::fs::write(run_b.join("saves/_autosave3.zip"), b"newer beta autosave").unwrap();
        std::fs::write(
            directory.path().join("_autosave9.zip"),
            b"shared contamination",
        )
        .unwrap();

        assert_eq!(
            promote_owned_autosave(&save_a, &owner_a, &managed_runs).unwrap(),
            Some(autosave_a)
        );
        assert_eq!(std::fs::read(&save_a).unwrap(), b"alpha autosave");
        assert_eq!(std::fs::read(&save_b).unwrap(), b"beta primary");
    }
}
