#!/usr/bin/env python3
"""
Thin pipe: Factorio in-game GUI <-> Claude agent SDK.

Watches for player messages from the mod, pipes each one through
Claude Code with factorioctl MCP tools, and sends the response back via RCON.

Single-agent:  python pipe.py --agent doug-nauvis
Multi-agent:   python pipe.py --group doug-squad
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import queue
import re
import signal
import shutil
import sys
import threading
import time
from datetime import datetime
from pathlib import Path
from typing import Any

from claude_agent_sdk import (
    query, ClaudeAgentOptions,
    AssistantMessage, UserMessage, ResultMessage, SystemMessage,
    TextBlock, ToolUseBlock, ToolResultBlock, ThinkingBlock,
)
from claude_agent_sdk.types import HookMatcher, McpStdioServerConfig
from loguru import logger

# Ensure sibling modules are importable
sys.path.insert(0, str(Path(__file__).resolve().parent))

# Load .env
_env_file = Path(__file__).parent / ".env"
if _env_file.exists():
    for _line in _env_file.read_text().splitlines():
        _line = _line.strip()
        if _line and not _line.startswith("#") and "=" in _line:
            _key, _, _val = _line.partition("=")
            _key, _val = _key.strip(), _val.strip()
            if _val and _key not in os.environ:
                os.environ[_key] = _val

logger.configure(extra={"agent": "system"})


def _shutdown_handler(signum, frame):
    """Handle SIGINT/SIGTERM and exit cleanly."""
    logger.info("Shutting down...")
    sys.exit(130 if signum == signal.SIGINT else 143)


# ── Run logging ───────────────────────────────────────────────

def setup_logging(log_dir: Path) -> Path | None:
    """Configure loguru console, human file, and structured JSONL sinks."""
    log_dir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().strftime("%Y-%m-%d_%H%M%S")
    log_path = log_dir / f"bridge-{stamp}.log"
    jsonl_path = log_dir / f"bridge-{stamp}.jsonl"
    console_format = (
        "<green>{time:HH:mm:ss}</green> | <level>{level: <8}</level> | "
        "<cyan>{extra[agent]}</cyan> | <level>{message}</level>"
    )
    file_format = (
        "{time:YYYY-MM-DD HH:mm:ss.SSS} | {level: <8} | "
        "{extra[agent]} | {message}"
    )
    logger.remove()
    logger.configure(extra={"agent": "system"})
    logger.add(
        sys.stderr,
        level="INFO",
        colorize=True,
        format=console_format,
        enqueue=True,
    )
    try:
        logger.add(log_path, level="DEBUG", format=file_format, enqueue=True)
        logger.add(jsonl_path, level="DEBUG", serialize=True, enqueue=True)
    except OSError as e:
        logger.warning("Could not open bridge log files in {}: {}", log_dir, e)
        return None
    return log_path


from ledger import (apply_ledger_update, load_ledger, parse_ledger_trailer,
                    render_ledger, strip_ledger_trailer)
from journal import (append_event, apply_reflection_update, count_events,
                     load_events, load_reflection, render_memory,
                     should_reflect, strip_reflection_trailer)
from planner import build_autonomy_prompt, choose_autonomy_mode
from skills import (apply_skill_update, load_library, render_skills,
                    strip_skill_trailer)
from rcon import RCONClient, ThreadSafeRCON, lua_long_string
from paths import find_script_output, find_factorioctl_mcp
from transport import (InputWatcher, send_response, send_tool_status, set_status,
                       check_mod_loaded, register_agent, unregister_agent,
                       pre_place_character, setup_surfaces, set_spectator_mode)
from paths import find_mod_source, find_mods_dir
from telemetry import SSEBroadcaster, start_sse_server, RelayPusher, Telemetry, emit_chat, emit_tool_call, emit_error, emit_status

_BRIDGE_DIR = Path(__file__).resolve().parent
_PLAYER_MESSAGES_MARKER = "\n\n--- Player Messages ---\n"
DEFAULT_MAX_TURNS = 200
SESSIONS_FILE = _BRIDGE_DIR / ".sessions.json"
_RCON_PRINT = "rcon." + "pr" + "int"
_MCP_TOOL_PREFIX = "mcp__factorioctl__"

# ── Agent profiles ───────────────────────────────────────────

def load_agent(agent_name: str) -> dict:
    """Load and validate agent profile from bridge/agents/{name}.json.
    If response_format is present, auto-generates and appends format instructions."""
    agent_file = _BRIDGE_DIR / "agents" / f"{agent_name}.json"
    if not agent_file.exists():
        raise FileNotFoundError(
            f"Agent profile not found: {agent_file}\n"
            f"Create it or use --agent default"
        )
    agent = json.loads(agent_file.read_text())
    # Validate required fields (per agent.schema.json)
    if not isinstance(agent.get("name"), str) or not agent["name"]:
        raise ValueError(f"Agent profile missing 'name': {agent_file}")
    if not isinstance(agent.get("system_prompt"), str) or not agent["system_prompt"]:
        raise ValueError(f"Agent profile missing 'system_prompt': {agent_file}")
    # Auto-generate formatting instructions from response_format
    fmt = agent.get("response_format")
    if fmt:
        instructions = build_format_instructions(fmt)
        agent["system_prompt"] = agent["system_prompt"] + "\n\n" + instructions
    return agent


# ── Response formatting ───────────────────────────────────────

def build_format_instructions(fmt: dict) -> str:
    """Generate system prompt formatting instructions from response_format config."""
    header_label = fmt.get("header_label", "STATUS")
    header_color = fmt.get("header_color", "1,0.8,0.2")
    action_label = fmt.get("action_label", "ACTIONS")
    action_color = fmt.get("action_color", "0.6,0.8,1")
    footer_label = fmt.get("footer_label")
    footer_color = fmt.get("footer_color", "0.4,0.6,0.4")
    sections = fmt.get("sections", [])

    lines = [
        "OUTPUT FORMAT — you MUST use these exact Factorio rich text tags in every response.",
        "These tags render as colored text in the game terminal. Output them literally.",
        "",
        "Structure:",
        f"  [color={header_color}]{header_label}:[/color] <short classification>",
        "",
        "  <body paragraphs — use [item=iron-plate] for items, [entity=stone-furnace] for buildings>",
    ]
    if True:  # always include actions
        lines.append("")
        lines.append(f"  [color={action_color}]{action_label}:[/color]")
        lines.append("  - action one")
        lines.append("  - action two")
    for sec in sections:
        color = sec.get("color", "0.5,0.7,0.5")
        lines.append("")
        lines.append(f"  [color={color}]{sec['label']}:[/color] <{sec.get('description', sec['label'].lower())}>")
    if footer_label:
        lines.append("")
        lines.append(f"  [color={footer_color}]{footer_label}:[/color] <closing status>")
    lines.append("")
    lines.append("Rules: No markdown (**, ##, ```). The [color=r,g,b]...[/color] tags are mandatory, not optional.")
    return "\n".join(lines)


# Matches [color=r,g,b]LABEL:[/color] section headers
_SECTION_RE = re.compile(
    r'\[color=([0-9.,]+)\]([A-Z][A-Z _]*?):\[/color\]\s*',
)


def parse_response(text: str) -> dict:
    """Parse a rich-text agent response into structured sections.
    Returns dict matching response.schema.json. Falls back to {"body": text}."""
    matches = list(_SECTION_RE.finditer(text))
    if not matches:
        return {"body": text}

    result = {}

    # Extract section contents by splitting between matches
    for i, m in enumerate(matches):
        color = m.group(1)
        label = m.group(2).strip()
        content_start = m.end()
        content_end = matches[i + 1].start() if i + 1 < len(matches) else len(text)
        content = text[content_start:content_end].strip()

        if i == 0:
            # First section is header. Split: first line = header text, rest = body.
            parts = content.split("\n\n", 1)
            result["header"] = {"label": label, "color": color, "text": parts[0].strip()}
            if len(parts) > 1 and parts[1].strip():
                result["body"] = parts[1].strip()
        elif "ACTION" in label.upper():
            actions = []
            for line in content.split("\n"):
                line = line.strip().lstrip("- ").strip()
                if line:
                    actions.append(line)
            if actions:
                result["actions"] = actions
        elif label.upper() in ("FILED", "CLASSIFIED", "END"):
            result["footer"] = {"label": label, "color": color, "text": content}
        else:
            if "data" not in result:
                result["data"] = {}
            result["data"][label] = {"color": color, "text": content}

    if "body" not in result:
        result["body"] = result.get("header", {}).get("text", text)

    return result


def sanitize_response(text: str) -> str:
    """Remove markdown artifacts while preserving Factorio rich text tags."""
    text = re.sub(r'\*\*(.+?)\*\*', r'\1', text)           # **bold** -> bold
    text = re.sub(r'^#{1,3}\s+', '', text, flags=re.MULTILINE)  # ## headers
    text = re.sub(r'```\w*\n?', '', text)                   # code fences
    return text.strip()


# ── Session persistence ──────────────────────────────────────

def _session_file(agent_name: str) -> Path:
    return _BRIDGE_DIR / f".session-{agent_name}.json"


def load_session(agent_name: str) -> str | None:
    """Load persisted session ID for an agent."""
    # Per-agent file (preferred)
    f = _session_file(agent_name)
    if f.exists():
        try:
            data = json.loads(f.read_text())
            return data.get("session_id")
        except (json.JSONDecodeError, OSError):
            return None
    # Backward compat: check old shared file
    if SESSIONS_FILE.exists():
        try:
            data = json.loads(SESSIONS_FILE.read_text())
            return data.get(agent_name)
        except (json.JSONDecodeError, OSError):
            return None
    return None


def save_session(agent_name: str, session_id: str):
    """Persist session ID for an agent (per-agent file, thread-safe)."""
    f = _session_file(agent_name)
    f.write_text(json.dumps({"session_id": session_id}) + "\n")


# ── MCP config ───────────────────────────────────────────────

McpServersConfig = dict[str, McpStdioServerConfig]


def build_mcp_servers(
    mcp_bin: str, rcon_host: str, rcon_port: int,
    rcon_password: str, agent_id: str = "default",
) -> McpServersConfig:
    """Build inline SDK MCP config for the factorioctl stdio server."""
    return {
        "factorioctl": {
            "type": "stdio",
            "command": mcp_bin,
            "args": [],
            "env": {
                "FACTORIO_RCON_HOST": rcon_host,
                "FACTORIO_RCON_PORT": str(rcon_port),
                "FACTORIO_RCON_PASSWORD": rcon_password,
                "FACTORIO_AGENT_ID": agent_id,
            },
        }
    }


# ── Claude SDK ───────────────────────────────────────────────


_BENIGN_STDERR = (
    "claude.ai connectors are disabled",
    "ANTHROPIC_API_KEY or another auth source is set",
)

# Matches an execution-tick progress note that says the plan/objective is done,
# so the next autonomy tick re-plans instead of spinning "plan complete".
_PLAN_DONE_RE = re.compile(
    r"\b(?:plan|objective)\b.{0,40}\b(?:complete|completed|finished|achieved|done)\b"
    r"|awaiting new|no changes|no further|nothing (?:to do|left|more)",
    re.IGNORECASE,
)


def _is_benign_stderr(stderr: str) -> bool:
    """True if every non-empty stderr line is known-benign CLI noise (the
    z.ai/claude.ai connector warning), so it is NOT recorded as a failure."""
    lines = [ln.strip() for ln in stderr.splitlines() if ln.strip()]
    if not lines:
        return True
    return all(any(p in ln for p in _BENIGN_STDERR) for ln in lines)


def _short_tool_name(name: str) -> str:
    if name.startswith("mcp__factorioctl__"):
        return name.removeprefix("mcp__factorioctl__")
    return name


def _json_for_log(value: Any) -> str:
    try:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        return str(value)


def _result_text(content: str | list[dict[str, Any]] | None) -> str:
    if content is None:
        return ""
    if isinstance(content, str):
        return content
    return _json_for_log(content)


def _split_player_messages(text: str) -> tuple[str, str]:
    if not isinstance(text, str):
        return str(text), ""
    if _PLAYER_MESSAGES_MARKER in text:
        tool_text, player_text = text.split(_PLAYER_MESSAGES_MARKER, 1)
        return tool_text.rstrip(), player_text.strip()
    return text, ""


def _strip_player_messages_from_value(value: Any) -> tuple[Any, list[str]]:
    if isinstance(value, dict):
        if value.get("type") == "text":
            stripped, player_text = _split_player_messages(str(value.get("text", "")))
            updated = dict(value)
            updated["text"] = stripped
            return updated, [player_text] if player_text else []
        updated = {}
        player_messages: list[str] = []
        for key, item in value.items():
            updated_item, item_messages = _strip_player_messages_from_value(item)
            updated[key] = updated_item
            player_messages.extend(item_messages)
        return updated, player_messages
    if isinstance(value, list):
        updated_items = []
        player_messages: list[str] = []
        for item in value:
            updated_item, item_messages = _strip_player_messages_from_value(item)
            updated_items.append(updated_item)
            player_messages.extend(item_messages)
        return updated_items, player_messages
    return value, []


def _result_text_and_player_messages(
    content: str | list[dict[str, Any]] | None,
) -> tuple[str, str]:
    if content is None:
        return "", ""

    if isinstance(content, str):
        tool_text, player_text = _split_player_messages(content)
        if player_text:
            return tool_text, player_text
        try:
            parsed = json.loads(content)
        except (TypeError, ValueError):
            return content, ""
        stripped, player_messages = _strip_player_messages_from_value(parsed)
        if player_messages:
            return _json_for_log(stripped), "\n".join(player_messages)
        return content, ""

    stripped, player_messages = _strip_player_messages_from_value(content)
    return _json_for_log(stripped), "\n".join(player_messages)


_MUTATING_FACTORIO_TOOLS = {
    "clear_area",
    "craft",
    "create_zone",
    "delete_zone",
    "extract_items",
    "insert_items",
    "mine_at",
    "place_entity",
    "remove_entity",
    "route_belt",
    "set_recipe",
    "start_research",
    "update_zone",
    "walk_to",
}
_PARALLEL_MUTATION_GUARD_PREFIX = (
    "Factorioctl bridge blocked parallel mutating tool call:"
)


def _short_factorio_tool_name(tool_name: str) -> str:
    if tool_name.startswith(_MCP_TOOL_PREFIX):
        return tool_name[len(_MCP_TOOL_PREFIX):]
    return tool_name


def _is_mutating_factorio_tool(tool_name: str) -> bool:
    return _short_factorio_tool_name(tool_name) in _MUTATING_FACTORIO_TOOLS


def _is_operator_only_tool_refusal(text: str) -> bool:
    stripped = str(text).strip()
    return (
        stripped.startswith("Error: execute_lua is disabled.")
        or stripped.startswith(_PARALLEL_MUTATION_GUARD_PREFIX)
    )


def _is_benign_tool_miss(text: str) -> bool:
    return str(text).strip().lower() in {
        "error: no items of that type in inventory",
        "no items of that type in inventory",
    }


class MutatingToolBatchGate:
    """Block same-message mutating MCP batches before they race inventory state."""

    def __init__(self, log, window_s: float | None = None):
        self.log = log
        self.window_s = float(
            window_s if window_s is not None else os.environ.get(
                "BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S", "1.0"
            )
        )
        self._lock = asyncio.Lock()
        self._last_at = 0.0
        self._last_tool_use_id: str | None = None
        self._last_tool_name: str | None = None

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        tool_name = _hook_value(hook_input, "tool_name")
        if not tool_name or not _is_mutating_factorio_tool(tool_name):
            return {}

        now = time.monotonic()
        short_name = _short_factorio_tool_name(tool_name)
        async with self._lock:
            if (
                self._last_tool_use_id
                and tool_use_id != self._last_tool_use_id
                and now - self._last_at < self.window_s
            ):
                previous = _short_factorio_tool_name(self._last_tool_name or "")
                message = (
                    f"{_PARALLEL_MUTATION_GUARD_PREFIX} {short_name}. "
                    "Wait for the previous mutating tool result before issuing "
                    "another world/inventory-changing command."
                )
                self.log.debug(
                    "blocked parallel mutating tool: {} after {} in {:.3f}s",
                    short_name,
                    previous,
                    now - self._last_at,
                )
                return {
                    "decision": "block",
                    "reason": message,
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": message,
                    },
                }

            self._last_at = now
            self._last_tool_use_id = tool_use_id
            self._last_tool_name = tool_name
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
            }
        }


def _hook_value(hook_input: Any, key: str) -> Any:
    if isinstance(hook_input, dict):
        return hook_input.get(key)
    return getattr(hook_input, key, None)


def _json_payload_has_error(value: Any) -> bool:
    if isinstance(value, dict):
        if value.get("success") is True:
            return False
        if (
            value.get("success") is False
            and value.get("can_place") is False
            and "entity" in value
            and "position" in value
            and "inventory_count" in value
        ):
            return False
        if (
            "allowed" in value
            and "policy_allowed" in value
            and "factorio_allowed" in value
            and "entity" in value
            and "position" in value
        ):
            return False
        for key, item in value.items():
            key_lower = str(key).lower()
            if key_lower == "error" and item:
                return True
            if key_lower == "success" and item is False:
                reason = (
                    value.get("error")
                    or value.get("message")
                    or value.get("reason")
                    or value.get("action_needed")
                )
                if reason:
                    return True
            if key_lower in {"status", "state", "result"}:
                item_text = str(item).strip().lower()
                if item_text in {"error", "failed", "failure", "fail"}:
                    return True
            if _json_payload_has_error(item):
                return True
    elif isinstance(value, list):
        return any(_json_payload_has_error(item) for item in value)
    return False


def _json_text_block_has_error(value: Any) -> bool:
    if isinstance(value, dict):
        if value.get("type") == "text":
            text = str(value.get("text", "")).strip()
            if _is_operator_only_tool_refusal(text):
                return False
            if _is_benign_tool_miss(text):
                return False
            lowered = text.lower()
            if lowered.startswith("error:") or lowered.startswith("cannot "):
                return True
            try:
                parsed = json.loads(text)
            except (TypeError, ValueError):
                return False
            return _json_payload_has_error(parsed)
        return any(_json_text_block_has_error(item) for item in value.values())
    if isinstance(value, list):
        return any(_json_text_block_has_error(item) for item in value)
    return False


def _looks_like_tool_error(text: str) -> bool:
    """Detect factorioctl game-logic failures that are returned as success-path
    strings instead of SDK/CLI tool errors."""
    stripped = text.strip()
    if not stripped:
        return False
    if _is_operator_only_tool_refusal(stripped):
        return False
    if _is_benign_tool_miss(stripped):
        return False
    try:
        parsed = json.loads(stripped)
    except (TypeError, ValueError):
        parsed = None
    if _json_payload_has_error(parsed):
        return True
    if parsed is not None:
        return _json_text_block_has_error(parsed)

    lowered = stripped.lower()
    patterns = (
        r"^error(?:\s|:)",
        r"\berror:",
        r"\bcannot\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)\b",
        r"\bcould not\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)\b",
        r"\bnot in inventory\b",
        r"\bno power\b",
        r"\bnot found\b",
        r"\bfailed\b",
        r"\binsufficient\b.{0,40}\b(?:items|resources|inventory|materials)\b",
        r"\bplacement\b.{0,40}\b(?:failed|blocked|invalid)\b",
        r"\bentity\b.{0,40}\b(?:not found|invalid|missing)\b",
    )
    return any(re.search(pattern, lowered) for pattern in patterns)


def _short_event_text(text: str, limit: int = 300) -> str:
    return " ".join(text.split())[:limit]


def _is_meaningful_anomaly(text: str) -> bool:
    normalized = re.sub(r"[^a-z0-9 ]+", "", text.strip().lower())
    normalized = re.sub(r"\s+", " ", normalized)
    if not normalized:
        return False
    nominal_values = {
        "none",
        "nominal",
        "na",
        "n a",
        "not applicable",
        "none detected",
        "none noted",
        "none observed",
        "no anomaly",
        "no anomalies",
        "no anomalies observed",
    }
    if normalized in nominal_values:
        return False
    return not normalized.startswith(("no anomaly", "no anomalies", "none ", "nominal"))


def _is_sdk_terminal_error_echo(text: str) -> bool:
    return "Claude Code returned an error result:" in str(text)


def _disallowed_tools_for_env(env: dict[str, str]) -> list[str]:
    raw_lua = str(env.get("FACTORIOCTL_ALLOW_RAW_LUA", "")).strip().lower()
    if raw_lua in {"1", "true", "yes", "on"}:
        return []
    return ["mcp__factorioctl__execute_lua"]


def _resolve_max_turns(value: Any = None) -> int:
    if value is None:
        value = os.environ.get("BRIDGE_MAX_TURNS")
    if value is None:
        return DEFAULT_MAX_TURNS
    try:
        turns = int(value)
    except (TypeError, ValueError):
        return DEFAULT_MAX_TURNS
    return turns if turns > 0 else DEFAULT_MAX_TURNS


def _record_anomaly(reply: str, agent_name: str) -> None:
    sections = parse_response(reply)
    data = sections.get("data") if isinstance(sections, dict) else None
    if not isinstance(data, dict):
        return
    anomaly = data.get("ANOMALY")
    if not isinstance(anomaly, dict):
        return
    text = str(anomaly.get("text", "")).strip()
    if _is_meaningful_anomaly(text):
        append_event(agent_name, "discovery", _short_event_text(text))


# Hard wall-clock cap on a single agent tick. The SDK's max_turns bounds tool
# turns, not a stalled TCP connection or a GLM response that never yields, so a
# tick is also wrapped in asyncio.wait_for. Override via BRIDGE_TICK_TIMEOUT_S.
_TICK_TIMEOUT_S = float(os.environ.get("BRIDGE_TICK_TIMEOUT_S", "2400"))


def _stderr_callback(log):
    def _handle(stderr: str) -> None:
        text = stderr.rstrip()
        if not text:
            return
        if _is_benign_stderr(text):
            log.debug("sdk stderr: {}", text)
        else:
            log.warning("sdk stderr: {}", text)
    return _handle


async def _run_agent(
    prompt: str,
    options: ClaudeAgentOptions,
    agent_name: str,
    telemetry: Telemetry | None,
    telemetry_name: str,
    rcon: RCONClient,
    player_index: int,
    log,
) -> tuple[list[str], str | None]:
    text_parts: list[str] = []
    new_session_id: str | None = None

    async for msg in query(prompt=prompt, options=options):
        if isinstance(msg, AssistantMessage):
            if msg.session_id:
                new_session_id = msg.session_id
            for block in msg.content:
                if isinstance(block, TextBlock):
                    text_parts.append(block.text)
                    log.info("text: {}", block.text.strip())
                elif isinstance(block, ToolUseBlock):
                    display = _short_tool_name(block.name)
                    log.debug("tool: {}({})", display, _json_for_log(block.input))
                    emit_tool_call(telemetry, display, block.input, agent=telemetry_name)
                    if display.endswith("broadcast_thought"):
                        thought = block.input.get("message", "")
                        if thought:
                            emit_chat(telemetry, "agent", thought, agent=telemetry_name)
                    if player_index > 0 and (
                        not block.name.startswith("mcp__")
                        or block.name.startswith("mcp__factorioctl__")
                    ):
                        try:
                            send_tool_status(rcon, player_index, agent_name, display)
                        except Exception as e:
                            log.debug("tool status update failed: {}", e)
                elif isinstance(block, ThinkingBlock):
                    log.debug("thinking: {}", block.thinking)
        elif isinstance(msg, UserMessage):
            # UserMessage.content is str OR list. The list form carries
            # ToolResultBlocks; the str form is a bare tool/result payload that
            # some Anthropic-compatible adapters (z.ai/GLM) emit instead. Inspect
            # BOTH so a string-wrapped failure can't vanish unlogged again.
            if isinstance(msg.content, str):
                text, player_messages = _result_text_and_player_messages(msg.content)
                if _looks_like_tool_error(text):
                    log.warning("tool_result ERROR: {}", text)
                    append_event(agent_name, "failure", _short_event_text(text))
                elif text.strip():
                    log.debug("tool_result: {}", text)
                if player_messages:
                    log.info("player_messages: {}", player_messages)
            else:
                for block in msg.content:
                    if isinstance(block, ToolResultBlock):
                        text, player_messages = _result_text_and_player_messages(block.content)
                        if (
                            block.is_error
                            and not _is_operator_only_tool_refusal(text)
                        ) or _looks_like_tool_error(text):
                            log.warning("tool_result ERROR: {}", text)
                            append_event(agent_name, "failure", _short_event_text(text))
                        else:
                            log.debug("tool_result: {}", text)
                        if player_messages:
                            log.info("player_messages: {}", player_messages)
        elif isinstance(msg, ResultMessage):
            new_session_id = msg.session_id or new_session_id
            if msg.result and msg.result not in text_parts:
                text_parts.append(msg.result)
            if msg.is_error:
                detail = msg.result or "; ".join(msg.errors or []) or "agent result marked as error"
                log.warning("result ERROR: {}", detail)
                append_event(agent_name, "failure", _short_event_text(detail))
            if msg.total_cost_usd is not None:
                log.info(
                    "done: ${:.4f} | {} turns | {:.1f}s",
                    msg.total_cost_usd,
                    msg.num_turns,
                    (msg.duration_ms or 0) / 1000,
                )
                if telemetry:
                    telemetry.emit({
                        "type": "compute_cost",
                        "data": {
                            "cost_usd": msg.total_cost_usd,
                            "turns": msg.num_turns,
                            "duration_ms": msg.duration_ms,
                        },
                        "agent": telemetry_name,
                    })
        elif isinstance(msg, SystemMessage):
            log.debug("system: {}", msg)
        else:
            log.debug("stream event: {}", msg)

    return text_parts, new_session_id


def _finalize_reply(reply: str, agent_name: str) -> str:
    """Persist any <ledger> trailer the agent emitted, strip it from the
    human-visible reply, and fall back to a placeholder if the reply was ONLY a
    ledger block (so the bridge never logs/sends a blank message). This is the
    tested seam for the ledger persist + empty-reply guard."""
    ledger_update = parse_ledger_trailer(reply)
    apply_ledger_update(agent_name, reply)
    apply_reflection_update(agent_name, reply)
    apply_skill_update(reply)
    if ledger_update and ledger_update.get("progress"):
        append_event(agent_name, "progress", ledger_update["progress"])
    _record_anomaly(reply, agent_name)
    reply = strip_ledger_trailer(reply)
    reply = strip_reflection_trailer(reply)
    reply = strip_skill_trailer(reply)
    if not reply.strip():
        return "(action complete)"
    return reply


def handle_message(
    prompt: str,
    mcp_config: McpServersConfig | str | Path,
    system_prompt: str,
    session_id: str | None,
    rcon: RCONClient,
    player_index: int,
    telemetry: Telemetry | None,
    agent_name: str = "default",
    telemetry_name: str | None = None,
    response_to: str | None = None,
    model: str | None = None,
    max_turns: int | None = None,
) -> str | None:
    """Pipe a message through the Claude SDK. Returns new session_id.
    agent_name: registered agent name (for RCON/mod).
    telemetry_name: display name for telemetry/logs (defaults to agent_name).
    response_to: if set, send response to this tab instead of agent_name (group chat)."""
    tname = telemetry_name or agent_name
    rcon_target = response_to or agent_name
    log = logger.bind(agent=tname)
    resume_tag = f" (resume {session_id[:8]}...)" if session_id else " (new session)"
    log.info("spawning claude sdk [model={}]{}", model or "default", resume_tag)

    env = os.environ.copy()
    env.pop("CLAUDECODE", None)
    mutating_tool_gate = MutatingToolBatchGate(log)
    options = ClaudeAgentOptions(
        system_prompt=system_prompt,
        model=model,
        max_turns=_resolve_max_turns(max_turns),
        mcp_servers=mcp_config,
        strict_mcp_config=True,
        tools=[],
        disallowed_tools=_disallowed_tools_for_env(env),
        permission_mode="bypassPermissions",
        resume=session_id,
        setting_sources=["local"],
        env=env,
        hooks={
            "PreToolUse": [HookMatcher(hooks=[mutating_tool_gate.hook])],
        },
        stderr=_stderr_callback(log),
    )
    try:
        text_parts, new_session_id = asyncio.run(
            asyncio.wait_for(
                _run_agent(
                    prompt,
                    options,
                    agent_name,
                    telemetry,
                    tname,
                    rcon,
                    player_index,
                    log,
                ),
                timeout=_TICK_TIMEOUT_S,
            )
        )
    except (asyncio.TimeoutError, TimeoutError):
        error_msg = f"Error: agent tick exceeded {_TICK_TIMEOUT_S:.0f}s and was aborted"
        log.error("agent tick timed out after {:.0f}s; aborting", _TICK_TIMEOUT_S)
        append_event(
            agent_name, "failure",
            _short_event_text(f"tick timeout after {_TICK_TIMEOUT_S:.0f}s"),
        )
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return session_id
    except FileNotFoundError:
        error_msg = "Error: claude CLI not installed"
        log.error("'claude' CLI not found. Install: npm install -g @anthropic-ai/claude-code")
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return session_id
    except Exception as e:
        error_msg = f"Error: {e}"
        if _is_sdk_terminal_error_echo(str(e)):
            log.warning("agent invocation ended after SDK terminal result: {}", e)
        else:
            log.exception("agent invocation failed")
            append_event(agent_name, "failure", _short_event_text(str(e)))
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return session_id

    # Send response — join all text parts so intermediate messages aren't lost
    reply = "\n\n".join(text_parts) if text_parts else "(action complete)"
    reply = sanitize_response(reply)
    reply = _finalize_reply(reply, agent_name)

    log.info("reply: {}", reply)
    sections = parse_response(reply)
    emit_chat(telemetry, "agent", reply, agent=tname, sections=sections)
    # For group chat, prefix reply with agent name so reader knows who said what
    if response_to:
        reply = f"[color=1,0.6,0.2]{tname}:[/color] {reply}"
    if player_index > 0:
        # A dropped RCON connection on this final send must not bubble out and
        # kill the agent thread (loguru no longer tees raw thread tracebacks).
        try:
            send_response(rcon, player_index, rcon_target, reply)
        except Exception as e:
            log.exception("failed to send reply to RCON")
            append_event(agent_name, "failure", _short_event_text(f"rcon send failed: {e}"))

    return new_session_id or session_id


# ── Telemetry ────────────────────────────────────────────────

def build_telemetry(args) -> Telemetry | None:
    """Wire up telemetry from CLI args."""
    sse_broadcaster = None
    relay_pusher = None

    if args.sse:
        try:
            sse_broadcaster = SSEBroadcaster()
            start_sse_server(sse_broadcaster, args.sse_port)
            logger.info("SSE server: http://localhost:{}/events", args.sse_port)
        except OSError as e:
            logger.warning("SSE server failed: {}", e)

    relay_url = args.relay or os.environ.get("RELAY_URL", "")
    if relay_url:
        token = args.relay_token or os.environ.get("RELAY_TOKEN", "")
        if not token:
            logger.warning("relay URL set but no RELAY_TOKEN")
        else:
            relay_pusher = RelayPusher(relay_url, token)
            logger.info("Relay: {}", relay_url)

    if sse_broadcaster or relay_pusher:
        return Telemetry(sse=sse_broadcaster, relay=relay_pusher)
    return None


# ── Multi-agent mode ─────────────────────────────────────────

# Planet order follows natural game progression
PLANET_ORDER = {
    "nauvis": 0,
    "vulcanus": 1,
    "fulgora": 2,
    "gleba": 3,
    "aquilo": 4,
}

def _agent_sort_key(agent: dict) -> tuple:
    """Sort agents by planet progression order, then name."""
    planet = agent.get("planet", "nauvis")
    return (PLANET_ORDER.get(planet, 99), agent.get("name", ""))

def discover_agents(group: str | None = None, names: list[str] | None = None) -> list[dict]:
    """Load agent profiles by group name or explicit name list."""
    if names:
        return [load_agent(n) for n in names]
    agents_dir = _BRIDGE_DIR / "agents"
    profiles = []
    for f in agents_dir.glob("*.json"):
        try:
            agent = json.loads(f.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        if agent.get("group") == group:
            profiles.append(load_agent(agent["name"]))
    if not profiles:
        raise ValueError(f"No agents found with group '{group}'")
    profiles.sort(key=_agent_sort_key)
    return profiles


class AgentThread:
    """Manages one agent's Claude SDK sessions in a dedicated thread."""

    def __init__(self, agent: dict,
                 mcp_config: McpServersConfig | str | Path | None, rcon,
                 telemetry: 'Telemetry | None', model: str | None,
                 heartbeat_interval: float = 0.0,
                 planner_interval: int = 5,
                 autonomy_requires_player: bool = True,
                 max_turns: int | None = None):
        self.agent = agent
        self.agent_name = agent["name"]
        self.system_prompt = agent["system_prompt"]
        # Tiered models: default to the fast "haiku" tier (.env -> glm-5-turbo)
        # for the frequent execution/reflection/chat ticks; planner ticks
        # override up to "sonnet" (.env -> glm-5.2) via _planner_model below.
        self.model = model or agent.get("model") or "haiku"
        self.max_turns = _resolve_max_turns(
            max_turns if max_turns is not None else agent.get("max_turns")
        )
        self.telemetry_name = agent.get("telemetry_name", self.agent_name)
        self.log = logger.bind(agent=self.telemetry_name)
        self.mcp_config = mcp_config
        self.rcon = rcon
        self.telemetry = telemetry
        # Autonomy: when no human message arrives within heartbeat_interval
        # seconds, the agent prompts itself to keep playing. <= 0 disables
        # autonomy (agent acts only in response to chat). A profile may
        # override via agent["heartbeat_interval"].
        self.heartbeat_interval = float(
            agent.get("heartbeat_interval", heartbeat_interval)
        )
        self._exec_ticks_since_plan = 0
        self._planner_interval = int(
            agent.get("planner_interval", planner_interval)
        )
        self._reflect_interval = int(agent.get("reflect_interval", 16))
        self._planner_model = agent.get("planner_model") or "sonnet"
        # When True, autonomy ticks only fire while a human is connected to the
        # server, so the agent waits to "do its own thing" until you join (and
        # goes back to idle if you leave). Chat is always processed regardless.
        self.autonomy_requires_player = bool(
            agent.get("autonomy_requires_player", autonomy_requires_player)
        )
        self._waiting_for_player_logged = False
        self.session_id = load_session(self.agent_name)
        self.inbox: queue.Queue = queue.Queue()
        self._thread = threading.Thread(
            target=self._run, name=f"agent-{self.agent_name}", daemon=True,
        )

    def start(self):
        self._thread.start()

    def enqueue(self, msg: dict):
        self.inbox.put(msg)

    def _human_connected(self) -> bool:
        """True if at least one human player is connected. AI agents are orphan
        character entities (not game.players), so connected_players counts only
        real human clients. On any RCON error, return False so we don't burn
        autonomy turns when we can't confirm a human is present."""
        try:
            out = self.rcon.execute(
                f"/silent-command {_RCON_PRINT}(#game.connected_players)"
            )
            return int(out.strip() or "0") > 0
        except Exception as e:
            self.log.debug("human-connected check failed: {}", e)
            return False

    def _live_state_line(self) -> str:
        """Best-effort one-line live state for autonomy ticks."""
        try:
            agent = lua_long_string(self.agent_name)
            lua = (
                f'local c = remote.call("claude_interface", "get_character", {agent}) '
                'if c and c.valid then '
                f'{_RCON_PRINT}("Live state: " .. c.surface.name .. " @ " .. '
                'string.format("%.1f,%.1f", c.position.x, c.position.y)) '
                'end'
            )
            return self.rcon.execute(f"/silent-command {lua}").strip()
        except Exception as e:
            self.log.debug("live-state lookup failed: {}", e)
            return ""

    def _compose_autonomy_prompt(self) -> str:
        """Assemble the autonomy-tick prompt for the current plan/execute mode."""
        tick = self._autonomy_tick()
        return tick["message"]

    def _autonomy_tick(self) -> dict:
        """Choose plan/execute mode, update cadence state, and build the message."""
        ledger = load_ledger(self.agent_name)
        events = load_events(self.agent_name, 5)
        memory = render_memory(events, load_reflection(self.agent_name))
        ledger_text = render_ledger(ledger)
        skill_text = render_skills(load_library())
        mode = choose_autonomy_mode(
            ledger, self._exec_ticks_since_plan, self._planner_interval,
        )
        # If the last tick reported the plan/objective finished, re-plan NOW
        # instead of spinning "plan complete" for planner_interval ticks.
        if mode == "execute" and events and _PLAN_DONE_RE.search(events[-1].get("text", "")):
            mode = "plan"
        if mode == "plan":
            self._exec_ticks_since_plan = 0
        else:
            self._exec_ticks_since_plan += 1
        # The available-skills list is injected every tick (compact); the full
        # save-a-new-skill format example is shown only on the deliberative
        # planner tick so cheap execution ticks stay lean.
        parts = [memory, ledger_text, skill_text]
        if mode == "plan":
            parts.append(
                "Prefer reusing an existing skill over re-deriving a build; when "
                "you perfect a new reusable build, save it as a <skill> block.\n"
                "<skill>\n"
                "name: lay_smelting_line\n"
                "params: ore_belt_pos, furnace_count\n"
                "steps:\n"
                "- place N stone furnaces in a column\n"
                "- route the ore belt past them and add input inserters\n"
                "- add output inserters to a plates belt\n"
                "outcome: iron/copper plates on the output belt\n"
                "</skill>"
            )
        continuity_parts = [part for part in parts if part]

        message = build_autonomy_prompt(
            mode, "\n\n".join(continuity_parts), self._live_state_line(),
        )
        if should_reflect(
            count_events(self.agent_name), getattr(self, "_reflect_interval", 16),
        ):
            message = "\n\n".join([
                message,
                "This is a reflection turn: emit a hidden <reflection> block "
                "summarizing durable lessons in exactly this format:\n"
                "<reflection>\n"
                "structures:\n"
                "- what is built where\n"
                "error_tips:\n"
                "- mistake to avoid next time\n"
                "</reflection>",
            ])

        tick = {
            "message": message,
            "player_index": 0,
            "player_name": "autonomy",
            "autonomy": True,
        }
        if mode == "plan" and self._planner_model:
            tick["model"] = self._planner_model
        return tick

    def _next_message(self) -> dict:
        """Block for the next human message, or synthesize an autonomy tick if
        the agent has been idle for heartbeat_interval seconds. When
        autonomy_requires_player is set, autonomy ticks are suppressed until a
        human is connected — chat is still delivered immediately regardless."""
        if self.heartbeat_interval <= 0:
            return self.inbox.get()
        while True:
            try:
                return self.inbox.get(timeout=self.heartbeat_interval)
            except queue.Empty:
                if self.autonomy_requires_player and not self._human_connected():
                    if not self._waiting_for_player_logged:
                        self.log.info(
                            "{} idle - waiting for a player to join before acting",
                            self.agent_name,
                        )
                        self._waiting_for_player_logged = True
                    continue
                self._waiting_for_player_logged = False
                return self._autonomy_tick()

    def _run(self):
        while True:
            try:
                self._run_once()
            except Exception:
                # A crashing tick must never take the whole agent thread down
                # silently — log it, journal it, and keep serving the inbox.
                self.log.exception("{} tick crashed; thread continuing", self.agent_name)
                try:
                    append_event(self.agent_name, "failure", "agent tick crashed (see log)")
                except Exception:
                    pass
                time.sleep(0.5)

    def _run_once(self):
        """Serve exactly one inbox message (or autonomy tick). Called in a
        guarded loop by _run so a single crash can't kill the thread."""
        msg = self._next_message()
        if msg.get("autonomy"):
            self.log.info("{} autonomy tick", self.agent_name)
        player_index = msg.get("player_index", 1)
        player_name = msg.get("player_name", "Player")
        message = msg["message"]
        response_to = msg.get("response_to")  # Group chat routing

        target_label = response_to or self.agent_name
        if response_to:
            self.log.info(
                "{} -> {}:{}: {}",
                player_name,
                target_label,
                self.agent_name,
                message,
            )
        else:
            self.log.info("{} -> {}: {}", player_name, self.agent_name, message)
        emit_chat(self.telemetry, "player", message, agent=self.telemetry_name)

        # player_index=0 means injected message (supervisor/API), skip GUI updates
        if player_index > 0:
            try:
                set_status(self.rcon, player_index, "[color=1,0.8,0.2]Thinking...[/color]")
            except Exception as e:
                self.log.debug("status update failed: {}", e)

        if not self.mcp_config:
            rcon_target = response_to or self.agent_name
            self.log.error("factorioctl MCP not found")
            if player_index > 0:
                send_response(self.rcon, player_index, rcon_target,
                              "Error: factorioctl MCP not found")
            return

        new_session = handle_message(
            message, self.mcp_config, self.system_prompt, self.session_id,
            self.rcon, player_index, self.telemetry,
            agent_name=self.agent_name, telemetry_name=self.telemetry_name,
            response_to=response_to, model=msg.get("model") or self.model,
            max_turns=self.max_turns,
        )
        if new_session:
            self.session_id = new_session
            save_session(self.agent_name, self.session_id)


def main_multi(args, agent_profiles: list[dict]):
    """Multi-agent mode: one thread per agent, shared watcher."""
    log = logger.bind(agent="system")
    # Shared RCON (thread-safe)
    log.info("Connecting to Factorio RCON...")
    rcon_raw = RCONClient(args.rcon_host, args.rcon_port, args.rcon_password)
    rcon = ThreadSafeRCON(rcon_raw)
    log.info("RCON connected")

    mod_loaded = check_mod_loaded(rcon)
    if mod_loaded:
        log.info("claude-interface mod detected")
        # Register group chat + agents first, THEN remove default
        # (unregister must happen after registers so safety check passes)
        register_agent(rcon, "all", label="ALL")
        log.info("Registered tab: all (group chat)")
        for agent in agent_profiles:
            label = agent.get("planet", agent["name"]).capitalize()
            register_agent(rcon, agent["name"], label=label)
            log.info("Registered agent: {} [{}]", agent["name"], label)
        unregister_agent(rcon, "default")
    else:
        log.warning("claude-interface mod not detected")

    # Create planet surfaces if requested (for fresh worlds)
    if args.setup_surfaces:
        planets = list({a.get("planet", "nauvis") for a in agent_profiles} - {"nauvis"})
        if planets:
            log.info("Setting up planet surfaces")
            results = setup_surfaces(rcon, sorted(planets))
            for planet, status in results.items():
                log.info("{}: {}", planet, status)

    # Pre-place characters on correct planets (offset to avoid overlapping with player)
    log.info("Pre-placing characters")
    for i, agent in enumerate(agent_profiles):
        planet = agent.get("planet", "nauvis")
        result = pre_place_character(rcon, agent["name"], planet, spawn_offset=i)
        log.info("{} -> {}: {}", agent["name"], planet, result)

    # Spectator mode: players who connect will be set to spectator (no character body)
    if args.spectator:
        set_spectator_mode(rcon, enabled=True)
        log.info("Spectator mode enabled; players join as spectators")

    # Telemetry
    telemetry = build_telemetry(args)

    # MCP configs and agent threads
    mcp_bin = args.factorioctl_mcp or find_factorioctl_mcp()
    agents: dict[str, AgentThread] = {}
    for agent in agent_profiles:
        mcp_config = None
        if mcp_bin:
            mcp_config = build_mcp_servers(
                mcp_bin, args.rcon_host, args.rcon_port,
                args.rcon_password, agent_id=agent["name"],
            )
        at = AgentThread(agent, mcp_config, rcon, telemetry, args.model,
                         heartbeat_interval=args.heartbeat_interval,
                         planner_interval=args.planner_interval,
                         autonomy_requires_player=args.autonomy_requires_player,
                         max_turns=args.max_turns)
        agents[agent["name"]] = at

    # Resolve paths and start watcher
    script_output = Path(args.script_output) if args.script_output else find_script_output()
    input_file = script_output / "claude-chat" / "input.jsonl"
    input_file.parent.mkdir(parents=True, exist_ok=True)
    watcher = InputWatcher(input_file)

    # Banner
    agent_names = ", ".join(a["name"] for a in agent_profiles)
    log.info("Factorio companion - multi-agent")
    log.info("Agents: {}", agent_names)
    log.info("RCON: {}:{}", args.rcon_host, args.rcon_port)
    log.info("Input: {}", input_file)
    if mcp_bin:
        log.info("MCP server: {}", mcp_bin)

    # Start agent threads with staggered delays to avoid RCON flood
    stagger = args.stagger_delay
    log.info("Starting agents (stagger: {}s)", stagger)
    for i, at in enumerate(agents.values()):
        at.start()
        log.info("{} online", at.agent_name)
        if stagger > 0 and i < len(agents) - 1:
            time.sleep(stagger)

    log.info("Watching for messages... (Ctrl+C to stop)")

    try:
        while True:
            time.sleep(args.poll_interval)
            for msg in watcher.poll():
                target = msg.get("target_agent", "default")
                if target == "all":
                    # Fan out to all agents with staggered delivery
                    for i, at in enumerate(agents.values()):
                        at.enqueue({**msg, "response_to": "all"})
                        if i < len(agents) - 1:
                            time.sleep(1)  # stagger to avoid RCON flood
                elif target in agents:
                    agents[target].enqueue(msg)
                else:
                    log.warning("Message for unknown agent '{}', dropping", target)
    except (KeyboardInterrupt, SystemExit):
        log.info("Shutting down...")
    finally:
        rcon.close()
        log.info("Done")


def _sync_mod():
    """Copy mod source to Factorio mods directory."""
    src = find_mod_source()
    mods_dir = find_mods_dir()
    dst = mods_dir / "claude-interface"
    dst.mkdir(parents=True, exist_ok=True)

    count = 0
    for f in src.rglob("*"):
        if f.is_file():
            rel = f.relative_to(src)
            target = dst / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f, target)
            count += 1

    # Read version from info.json
    info = json.loads((src / "info.json").read_text())
    ver = info.get("version", "?")
    logger.info("Synced claude-interface v{} ({} files)", ver, count)
    logger.info("{} -> {}", src, dst)


# ── Main ─────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Thin pipe: Factorio in-game GUI <-> Claude agent SDK",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument("--agent", default=None,
                        help="Single agent mode (loads bridge/agents/{name}.json)")
    parser.add_argument("--group", default=None,
                        help="Multi-agent mode: load all agents with this group name")
    parser.add_argument("--agents", default=None,
                        help="Multi-agent mode: comma-separated agent names")
    parser.add_argument("--scale", type=int, default=None,
                        help="Multi-agent mode: start first N agents from group (by planet order)")
    parser.add_argument("--rcon-host", default="localhost")
    parser.add_argument("--rcon-port", type=int, default=27015)
    parser.add_argument("--rcon-password", default="factorio")
    parser.add_argument("--script-output", default=None)
    parser.add_argument("--model", default=None, help="Claude model (e.g. sonnet, opus, haiku)")
    parser.add_argument(
        "--max-turns",
        type=int,
        default=None,
        help=f"Max tool-use turns per message (default: {DEFAULT_MAX_TURNS}; env BRIDGE_MAX_TURNS)",
    )
    parser.add_argument("--poll-interval", type=float, default=0.5)
    parser.add_argument("--heartbeat-interval", type=float, default=6.0,
                        help="Autonomy: seconds idle before the agent self-prompts "
                             "to keep playing. 0 disables autonomy (chat-only).")
    parser.add_argument("--planner-interval", type=int, default=5,
                        help="Autonomy: execution ticks between deliberative "
                             "planner ticks.")
    parser.add_argument("--autonomy-requires-player",
                        action=argparse.BooleanOptionalAction, default=True,
                        help="Only run autonomy ticks while a human is connected, "
                             "so the agent waits to act until you join (default). "
                             "Use --no-autonomy-requires-player to let it play "
                             "immediately on boot.")
    parser.add_argument("--factorioctl-mcp", default=None)
    parser.add_argument("--sse", action="store_true")
    parser.add_argument("--sse-port", type=int, default=8088)
    parser.add_argument("--relay", default=None)
    parser.add_argument("--relay-token", default=None)
    parser.add_argument("--setup-surfaces", action="store_true",
                        help="Create planet surfaces before placing agents (for fresh worlds)")
    parser.add_argument("--stagger-delay", type=float, default=3.0,
                        help="Seconds between agent startups to avoid RCON flood (0=instant)")
    parser.add_argument("--spectator", action="store_true",
                        help="Put the human player into spectator mode (no character body)")
    parser.add_argument("--log-dir", default=None,
                        help="Directory for bridge run logs (default: logs/)")
    parser.add_argument("--sync-mod", action="store_true",
                        help="Copy mod to Factorio mods dir and exit")
    args = parser.parse_args()

    # Sync mod and exit
    if args.sync_mod:
        _sync_mod()
        return

    # Set up run logging (console + human file + structured JSONL)
    log_dir = Path(args.log_dir) if args.log_dir else (_BRIDGE_DIR.parent / "logs")
    log_path = setup_logging(log_dir)
    if log_path:
        logger.info("Logging to {}", log_path)

    # Install signal handlers for clean Ctrl+C shutdown
    signal.signal(signal.SIGINT, _shutdown_handler)
    signal.signal(signal.SIGTERM, _shutdown_handler)

    # Multi-agent mode
    if args.group or args.agents or args.scale:
        names = args.agents.split(",") if args.agents else None
        group = args.group or "doug-squad"
        profiles = discover_agents(group=group, names=names)
        if args.scale:
            profiles = profiles[:args.scale]
        main_multi(args, profiles)
        return

    # Single-agent mode
    agent = load_agent(args.agent or "default")
    agent_name = agent["name"]
    system_prompt = agent["system_prompt"]

    # CLI flags override agent profile; default to the fast "haiku" tier
    # (.env -> glm-5-turbo) to match the multi-agent path so single-agent runs
    # never fall through to an unintended SDK default model.
    model = args.model or agent.get("model") or "haiku"
    max_turns = _resolve_max_turns(
        args.max_turns if args.max_turns is not None else agent.get("max_turns")
    )
    telemetry_name = agent.get("telemetry_name", agent_name)
    log = logger.bind(agent=telemetry_name)

    # Load persisted session
    session_id = load_session(agent_name)

    # Resolve paths
    script_output = Path(args.script_output) if args.script_output else find_script_output()
    mcp_bin = args.factorioctl_mcp or find_factorioctl_mcp()

    input_file = script_output / "claude-chat" / "input.jsonl"
    input_file.parent.mkdir(parents=True, exist_ok=True)

    # Banner
    log.info("Factorio companion - {}", agent_name)
    log.info("Agent: {}", agent_name)
    log.info("RCON: {}:{}", args.rcon_host, args.rcon_port)
    log.info("Input: {}", input_file)
    if session_id:
        log.info("Session: {}... (resumed)", session_id[:12])
    else:
        log.info("Session: (new)")
    if model:
        log.info("Model: {}", model)
    if mcp_bin:
        log.info("MCP server: {}", mcp_bin)
    else:
        log.warning("MCP server not found (chat-only)")

    # RCON
    log.info("Connecting to Factorio RCON...")
    rcon = RCONClient(args.rcon_host, args.rcon_port, args.rcon_password)
    log.info("RCON connected")
    if check_mod_loaded(rcon):
        log.info("claude-interface mod detected")
        register_agent(rcon, agent_name)
        log.info("Registered agent: {}", agent_name)
    else:
        log.warning("claude-interface mod not detected")

    # Pre-place character on correct planet
    planet = agent.get("planet", "nauvis")
    result = pre_place_character(rcon, agent_name, planet, spawn_offset=0)
    log.info("Character: {} -> {}: {}", agent_name, planet, result)

    # Telemetry
    telemetry = build_telemetry(args)

    # MCP config
    mcp_config = None
    if mcp_bin:
        mcp_config = build_mcp_servers(
            mcp_bin, args.rcon_host, args.rcon_port,
            args.rcon_password, agent_id=agent_name,
        )

    # Watcher
    watcher = InputWatcher(input_file)

    log.info("Watching for messages... (Ctrl+C to stop)")

    try:
        while True:
            time.sleep(args.poll_interval)

            for msg in watcher.poll():
                target = msg.get("target_agent", "default")
                if target != agent_name:
                    continue

                player_index = msg.get("player_index", 1)
                player_name = msg.get("player_name", "Player")
                message = msg["message"]

                log.info("{} -> {}: {}", player_name, agent_name, message)
                emit_chat(telemetry, "player", message, agent=telemetry_name)

                if player_index > 0:
                    try:
                        set_status(rcon, player_index, "[color=1,0.8,0.2]Thinking...[/color]")
                    except Exception as e:
                        log.debug("status update failed: {}", e)

                if not mcp_config:
                    log.error("factorioctl MCP not found")
                    if player_index > 0:
                        send_response(rcon, player_index, agent_name, "Error: factorioctl MCP not found")
                    continue

                new_session = handle_message(
                    message, mcp_config, system_prompt, session_id,
                    rcon, player_index, telemetry,
                    agent_name=agent_name, telemetry_name=telemetry_name,
                    model=model, max_turns=max_turns,
                )
                if new_session:
                    session_id = new_session
                    save_session(agent_name, session_id)

    except (KeyboardInterrupt, SystemExit):
        log.info("Shutting down...")
    finally:
        rcon.close()
        log.info("Done")


if __name__ == "__main__":
    main()
