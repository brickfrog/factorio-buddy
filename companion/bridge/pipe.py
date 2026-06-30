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
from datetime import datetime, timedelta, timezone
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
from learning import (apply_learning_update, learning_proposal_prompt,
                      load_accepted_learning, render_accepted_learning,
                      strip_learning_trailers)
from models import (
    AgentProfile,
    BridgeValidationError,
    TOOL_PARAM_BOOLEAN,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_STRING,
    ToolCallRequest,
)
from planner import (
    build_autonomy_prompt,
    choose_autonomy_mode,
    objective_satisfied_by_live_state,
)
from skills import strip_skill_trailer
from rcon import RCONClient, ThreadSafeRCON, lua_long_string
from paths import find_script_output, find_factorioctl_mcp
from transport import (InputWatcher, send_response, send_tool_status, set_status,
                       check_mod_loaded, register_agent, unregister_agent,
                       pre_place_character, setup_surfaces, set_spectator_mode)
from paths import find_mod_source, find_mods_dir
from telemetry import SSEBroadcaster, start_sse_server, RelayPusher, Telemetry, emit_chat, emit_tool_call, emit_error, emit_status

_BRIDGE_DIR = Path(__file__).resolve().parent
_PROJECT_ROOT = _BRIDGE_DIR.parent.parent
_PLAYER_MESSAGES_MARKER = "\n\n--- Player Messages ---\n"
DEFAULT_MAX_TURNS = 200
DEFAULT_SDK_SKILLS = "factorio-control"
SESSIONS_FILE = _BRIDGE_DIR / ".sessions.json"
_RCON_PRINT = "rcon." + "pr" + "int"
_MCP_TOOL_PREFIX = "mcp__factorioctl__"
_USAGE_LIMIT_RESET_RE = re.compile(
    r"Usage limit reached.*?reset at "
    r"(?P<reset>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})",
    re.IGNORECASE,
)
_PROVIDER_TIMESTAMP_RE = re.compile(r"\[(?P<stamp>\d{14})[^\]]*\]")
_USAGE_LIMIT_COOLDOWNS: dict[str, datetime] = {}
_USAGE_LIMIT_COOLDOWNS_LOCK = threading.Lock()
_CONTEXT_WINDOW_COOLDOWNS: dict[str, datetime] = {}
_CONTEXT_WINDOW_COOLDOWNS_LOCK = threading.Lock()

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
    try:
        raw_agent = json.loads(agent_file.read_text())
    except json.JSONDecodeError as exc:
        raise ValueError(f"{agent_file}: invalid JSON: {exc}") from exc
    profile = AgentProfile.from_mapping(raw_agent)
    # Auto-generate formatting instructions from response_format
    fmt = profile.response_format
    if fmt:
        instructions = build_format_instructions(fmt)
        profile = profile.with_system_prompt(profile.system_prompt + "\n\n" + instructions)
    return profile.to_dict()


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

SESSION_RESET = "__factorioctl_session_reset__"

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


def clear_session(agent_name: str) -> None:
    """Forget a stale SDK session ID for an agent."""
    try:
        _session_file(agent_name).unlink()
    except FileNotFoundError:
        pass
    except OSError:
        pass

    # Backward compat: older bridge versions stored all agents in one file.
    if not SESSIONS_FILE.exists():
        return
    try:
        data = json.loads(SESSIONS_FILE.read_text())
    except (json.JSONDecodeError, OSError):
        return
    if not isinstance(data, dict) or agent_name not in data:
        return
    data.pop(agent_name, None)
    try:
        SESSIONS_FILE.write_text(json.dumps(data) + "\n")
    except OSError:
        pass


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
    r"|awaiting new|no further|nothing (?:to do|left|more)",
    re.IGNORECASE,
)
_PLAN_DONE_EVENT_KINDS = {"progress", "discovery", "milestone"}


def _events_indicate_plan_done(events: list[dict]) -> bool:
    """True when recent useful journal events say the current plan is done."""
    if not isinstance(events, list):
        return False
    for event in reversed(events):
        if not isinstance(event, dict):
            continue
        if event.get("kind") not in _PLAN_DONE_EVENT_KINDS:
            continue
        if _PLAN_DONE_RE.search(str(event.get("text", ""))):
            return True
    return False


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
    "feed_lab_from_inventory",
    "insert_items",
    "mine_at",
    "place_entity",
    "remove_entity",
    "route_belt",
    "rotate_entity",
    "set_recipe",
    "start_research",
    "update_zone",
    "walk_to",
}
_READ_ONLY_FACTORIO_TOOLS = {
    "analyze_belt_gaps",
    "analyze_belt_networks",
    "analyze_belt_reach",
    "analyze_inserters",
    "build_direct_smelter",
    "build_edge_miner",
    "check_placement",
    "detect_sushi_belts",
    "diagnose_steam_power",
    "extend_power_to",
    "find_build_area",
    "find_entity_placements",
    "find_nearest_resource",
    "get_alerts",
    "get_available_research",
    "get_belt_lane_contents",
    "get_blank_slate",
    "get_character",
    "get_entities",
    "get_inventory",
    "get_machine_belt_positions",
    "get_power_coverage",
    "get_power_networks",
    "get_power_status",
    "get_protected_resources",
    "get_recipe",
    "get_recipes_by_category",
    "get_recipes_for_item",
    "get_research_status",
    "get_resources",
    "get_tick",
    "get_zone",
    "list_zones",
    "plan_steam_power",
    "repair_steam_power",
    "render_map",
    "scan_resources",
    "situation_report",
    "trace_belt_sources",
    "verify_production",
}
_PARALLEL_MUTATION_GUARD_PREFIX = (
    "Factorioctl bridge blocked parallel mutating tool call:"
)
_READ_ONLY_TURN_GUARD_PREFIX = (
    "Factorioctl bridge blocked non-read-only tool during planner/reflection turn:"
)
_SKILL_REQUIRED_GUARD_PREFIX = (
    "Factorioctl bridge blocked Factorio tool before control skill:"
)
_PARAM_SCHEMA_GUARD_PREFIX = (
    "Factorioctl bridge blocked invalid Factorio tool parameters:"
)
_FACTORIO_TOOL_PARAM_SCHEMAS: dict[str, dict[str, dict[str, str]]] = {
    "walk_to": {
        "required": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
    },
    "place_entity": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {"direction": TOOL_PARAM_STRING},
    },
    "check_placement": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {"direction": TOOL_PARAM_STRING},
    },
    "find_entity_placements": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "mine_at": {
        "required": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
        "optional": {"count": TOOL_PARAM_INTEGER},
    },
    "craft": {
        "required": {"recipe": TOOL_PARAM_STRING},
        "optional": {"count": TOOL_PARAM_INTEGER},
    },
    "insert_items": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "item": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"inventory_type": TOOL_PARAM_STRING},
    },
    "extract_items": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "item": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"inventory_type": TOOL_PARAM_STRING},
    },
    "feed_lab_from_inventory": {
        "required": {
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "science_pack": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"dry_run": TOOL_PARAM_BOOLEAN},
    },
    "route_belt": {
        "required": {
            "from_x": TOOL_PARAM_INTEGER,
            "from_y": TOOL_PARAM_INTEGER,
            "to_x": TOOL_PARAM_INTEGER,
            "to_y": TOOL_PARAM_INTEGER,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
        },
    },
    "get_entities": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "name": TOOL_PARAM_STRING,
            "entity_type": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "get_resources": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "resource_type": TOOL_PARAM_STRING,
        },
    },
    "find_nearest_resource": {
        "required": {"resource_type": TOOL_PARAM_STRING},
        "optional": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
    },
    "get_recipe": {
        "required": {"name": TOOL_PARAM_STRING},
    },
    "get_recipes_for_item": {
        "required": {"item": TOOL_PARAM_STRING},
    },
    "get_recipes_by_category": {
        "required": {"category": TOOL_PARAM_STRING},
    },
    "set_recipe": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
        },
    },
    "remove_entity": {
        "required": {"unit_number": TOOL_PARAM_INTEGER},
    },
    "rotate_entity": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "direction": TOOL_PARAM_STRING,
        },
    },
    "get_machine_belt_positions": {
        "required": {"unit_number": TOOL_PARAM_INTEGER},
    },
    "build_edge_miner": {
        "required": {
            "resource_type": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "drill_name": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "build_direct_smelter": {
        "optional": {
            "drill_unit_number": TOOL_PARAM_INTEGER,
            "output_x": TOOL_PARAM_NUMBER,
            "output_y": TOOL_PARAM_NUMBER,
            "output_direction": TOOL_PARAM_STRING,
            "furnace_name": TOOL_PARAM_STRING,
            "inserter_name": TOOL_PARAM_STRING,
            "belt_name": TOOL_PARAM_STRING,
            "radius": TOOL_PARAM_INTEGER,
        },
    },
    "plan_steam_power": {
        "required": {
            "water_x1": TOOL_PARAM_NUMBER,
            "water_y1": TOOL_PARAM_NUMBER,
            "water_x2": TOOL_PARAM_NUMBER,
            "water_y2": TOOL_PARAM_NUMBER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
    },
    "repair_steam_power": {
        "required": {
            "x": TOOL_PARAM_INTEGER,
            "y": TOOL_PARAM_INTEGER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "extend_power_to": {
        "required": {
            "x": TOOL_PARAM_INTEGER,
            "y": TOOL_PARAM_INTEGER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "diagnose_steam_power": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_status": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_networks": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "find_power_issues": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_coverage": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_alerts": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "start_research": {
        "required": {"technology": TOOL_PARAM_STRING},
    },
}


def _short_factorio_tool_name(tool_name: str) -> str:
    if tool_name.startswith(_MCP_TOOL_PREFIX):
        return tool_name[len(_MCP_TOOL_PREFIX):]
    return tool_name


def _is_mutating_factorio_tool(tool_name: str) -> bool:
    return _short_factorio_tool_name(tool_name) in _MUTATING_FACTORIO_TOOLS


def _is_read_only_factorio_tool(tool_name: str) -> bool:
    return _short_factorio_tool_name(tool_name) in _READ_ONLY_FACTORIO_TOOLS


def _is_read_only_dry_run_tool_call(tool_name: str, hook_input: Any) -> bool:
    if _short_factorio_tool_name(tool_name) != "feed_lab_from_inventory":
        return False
    try:
        request = ToolCallRequest.from_hook_input(hook_input)
    except BridgeValidationError:
        return False
    return request.tool_input.get("dry_run", True) is not False


def _is_factorio_mcp_tool(tool_name: str) -> bool:
    return str(tool_name).startswith(_MCP_TOOL_PREFIX)


def _is_operator_only_tool_refusal(text: str) -> bool:
    stripped = str(text).strip()
    return (
        stripped.startswith("Error: execute_lua is disabled.")
        or stripped.startswith(_PARALLEL_MUTATION_GUARD_PREFIX)
        or stripped.startswith(_READ_ONLY_TURN_GUARD_PREFIX)
        or stripped.startswith(_SKILL_REQUIRED_GUARD_PREFIX)
        or stripped.startswith(_PARAM_SCHEMA_GUARD_PREFIX)
    )


def _is_benign_tool_miss(text: str) -> bool:
    return str(text).strip().lower() in {
        "error: no items of that type in inventory",
        "no items of that type in inventory",
        "error: no electric poles found in area",
        "no electric poles found in area",
        "error: no minable entity at position",
        "no minable entity at position",
    }


TOOL_RESULT_OK = "ok"
TOOL_RESULT_EXPECTED_MISS = "expected_miss"
TOOL_RESULT_INVALID_REQUEST = "invalid_request"
TOOL_RESULT_GAME_REJECTED = "game_rejected"
TOOL_RESULT_SDK_FAILURE = "sdk_failure"
TOOL_RESULT_INFRASTRUCTURE_FAILURE = "infrastructure_failure"
TOOL_RESULT_FAILURE_CLASSES = {
    TOOL_RESULT_INVALID_REQUEST,
    TOOL_RESULT_GAME_REJECTED,
    TOOL_RESULT_SDK_FAILURE,
    TOOL_RESULT_INFRASTRUCTURE_FAILURE,
}


def _classify_text_failure(text: str) -> str | None:
    lowered = str(text).strip().lower()
    if not lowered:
        return None
    if _is_operator_only_tool_refusal(text):
        return TOOL_RESULT_OK
    if _is_benign_tool_miss(text):
        return TOOL_RESULT_EXPECTED_MISS
    if re.search(
        r"invalid type|invalid json|failed to deserialize|expected .*sequence|"
        r"missing required|missing field|value for required field\b.{0,80}\bmissing|"
        r"unknown field|bad request|packet too large",
        lowered,
    ):
        return TOOL_RESULT_INVALID_REQUEST
    if re.search(
        r"expected value at line \d+ column \d+|exceeds maximum allowed tokens|"
        r"rcon|connection|timed out|timeout|unavailable|sync_or_restart_mod|"
        r"mod does not expose|claude-interface mod",
        lowered,
    ):
        return TOOL_RESULT_INFRASTRUCTURE_FAILURE
    if re.search(
        r"cannot\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)|"
        r"could not\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)|"
        r"not in inventory|no power|not found|no labs found|no .*resource entity found|"
        r"failed|insufficient\b.{0,40}\b(?:items|resources|inventory|materials)|"
        r"placement\b.{0,40}\b(?:failed|blocked|invalid)|"
        r"entity\b.{0,40}\b(?:not found|invalid|missing)|"
        r"route failed|factorio cannot place|\bblocked\b",
        lowered,
    ):
        return TOOL_RESULT_GAME_REJECTED
    if lowered.startswith("error:") or lowered.startswith("error "):
        return TOOL_RESULT_SDK_FAILURE
    return None


def _classify_json_payload(value: Any) -> str | None:
    if isinstance(value, dict):
        success_false = value.get("success") is False
        if success_false and value.get("expected_miss") is True:
            return TOOL_RESULT_EXPECTED_MISS

        if value.get("type") == "text":
            text = str(value.get("text", ""))
            try:
                parsed_text = json.loads(text)
            except (TypeError, ValueError):
                parsed_text = None
            if parsed_text is not None:
                parsed_class = _classify_json_payload(parsed_text)
                if parsed_class:
                    return parsed_class
                return None
            return _classify_text_failure(text)

        if value.get("success") is True:
            return TOOL_RESULT_OK

        if (
            value.get("success") is False
            and value.get("mined_count") == 0
            and not value.get("error")
        ):
            return TOOL_RESULT_EXPECTED_MISS

        if (
            value.get("success") is False
            and value.get("can_place") is False
            and "entity" in value
            and "position" in value
        ):
            return TOOL_RESULT_GAME_REJECTED

        if (
            "allowed" in value
            and "policy_allowed" in value
            and "factorio_allowed" in value
            and "entity" in value
            and "position" in value
        ):
            return TOOL_RESULT_OK

        for key, item in value.items():
            key_lower = str(key).lower()
            if key_lower == "error" and item:
                text_class = _classify_text_failure(str(item))
                if text_class:
                    return text_class
                if success_false:
                    return TOOL_RESULT_GAME_REJECTED
                return TOOL_RESULT_SDK_FAILURE
            if key_lower in {"message", "reason", "action_needed"} and item and success_false:
                text_class = _classify_text_failure(str(item))
                if text_class:
                    return text_class
            if key_lower in {"status", "state", "result"}:
                item_text = str(item).strip().lower()
                if item_text in {"error", "failed", "failure", "fail"}:
                    return TOOL_RESULT_SDK_FAILURE
            child_class = _classify_json_payload(item)
            if child_class and child_class != TOOL_RESULT_OK:
                return child_class
    elif isinstance(value, list):
        saw_explicit_ok = False
        for item in value:
            item_class = _classify_json_payload(item)
            if item_class and item_class != TOOL_RESULT_OK:
                return item_class
            if item_class == TOOL_RESULT_OK:
                saw_explicit_ok = True
        return TOOL_RESULT_OK if saw_explicit_ok else None
    return None


def _classify_tool_result(text: str, sdk_is_error: bool = False) -> str:
    stripped = str(text).strip()
    if not stripped:
        return TOOL_RESULT_OK
    if _is_operator_only_tool_refusal(stripped):
        return TOOL_RESULT_OK
    if _is_benign_tool_miss(stripped):
        return TOOL_RESULT_EXPECTED_MISS

    try:
        parsed = json.loads(stripped)
    except (TypeError, ValueError):
        parsed = None

    if parsed is not None:
        parsed_class = _classify_json_payload(parsed)
        if parsed_class:
            if parsed_class == TOOL_RESULT_OK and sdk_is_error:
                return TOOL_RESULT_OK
            return parsed_class
        if sdk_is_error:
            return TOOL_RESULT_SDK_FAILURE
        return TOOL_RESULT_OK

    text_class = _classify_text_failure(stripped)
    if text_class:
        return text_class
    if sdk_is_error:
        return TOOL_RESULT_SDK_FAILURE
    return TOOL_RESULT_OK


def _journal_classified_tool_failure(
    agent_name: str,
    classification: str,
    text: str,
) -> None:
    append_event(
        agent_name,
        "failure",
        f"{classification}: {_short_event_text(text)}",
    )


def _log_tool_result(agent_name: str, log, text: str, sdk_is_error: bool = False) -> str:
    classification = _classify_tool_result(text, sdk_is_error=sdk_is_error)
    if classification == TOOL_RESULT_OK:
        if text.strip():
            log.debug("tool_result: {}", text)
        return classification
    if classification == TOOL_RESULT_EXPECTED_MISS:
        log.debug("tool_result expected_miss: {}", text)
        return classification
    if classification == TOOL_RESULT_GAME_REJECTED:
        log.info("tool_result game_rejected: {}", text)
        _journal_classified_tool_failure(agent_name, classification, text)
        return classification
    if classification in TOOL_RESULT_FAILURE_CLASSES:
        log.warning("tool_result {}: {}", classification, text)
        _journal_classified_tool_failure(agent_name, classification, text)
        return classification
    log.debug("tool_result {}: {}", classification, text)
    return classification


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


class PlannerReadOnlyToolGate:
    """Block Factorio MCP mutations while the bridge is running a planning turn."""

    def __init__(self, log, enabled: bool = False):
        self.log = log
        self.enabled = enabled

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        if not self.enabled:
            return {}

        tool_name = str(_hook_value(hook_input, "tool_name") or "")
        if not tool_name or not _is_factorio_mcp_tool(tool_name):
            return {}
        if _is_read_only_factorio_tool(tool_name):
            return {}
        if _is_read_only_dry_run_tool_call(tool_name, hook_input):
            return {}

        short_name = _short_factorio_tool_name(tool_name)
        message = (
            f"{_READ_ONLY_TURN_GUARD_PREFIX} {short_name}. "
            "This turn may only use read-only diagnostics; emit a ledger-only "
            "plan or reflection and stop."
        )
        self.log.debug(
            "blocked non-read-only tool during planner/reflection turn: {}",
            short_name,
        )
        return _deny_pre_tool_use(message)


class FactorioToolSchemaGate:
    """Reject clearly malformed Factorio MCP parameters before Rust deserialization."""

    def __init__(self, log):
        self.log = log

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        try:
            request = ToolCallRequest.from_hook_input(hook_input)
        except BridgeValidationError as exc:
            message = f"{_PARAM_SCHEMA_GUARD_PREFIX} {exc}"
            self.log.debug("blocked malformed tool call hook input: {}", exc)
            return _deny_pre_tool_use(message)

        if not _is_factorio_mcp_tool(request.tool_name):
            return {}

        short_name = _short_factorio_tool_name(request.tool_name)
        schema = _FACTORIO_TOOL_PARAM_SCHEMAS.get(short_name)
        if not schema:
            return {}

        try:
            request.validate_params(
                required=schema.get("required", {}),
                optional=schema.get("optional", {}),
            )
        except BridgeValidationError as exc:
            message = f"{_PARAM_SCHEMA_GUARD_PREFIX} {short_name}: {exc}"
            self.log.debug("blocked invalid {} params: {}", short_name, exc)
            return _deny_pre_tool_use(message)

        return {}


class FactorioSkillGate:
    """Require the SDK control skill before exposing Factorio MCP actions."""

    def __init__(self, log, required: bool = True):
        self.log = log
        self.required = required
        self._lock = asyncio.Lock()
        self._saw_skill = False

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        tool_name = str(_hook_value(hook_input, "tool_name") or "")
        if not self.required or not tool_name:
            return {}

        async with self._lock:
            if tool_name == "Skill":
                self._saw_skill = True
                return {
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "allow",
                    }
                }

            if _is_factorio_mcp_tool(tool_name) and not self._saw_skill:
                short_name = _short_factorio_tool_name(tool_name)
                message = (
                    f"{_SKILL_REQUIRED_GUARD_PREFIX} {short_name}. "
                    "Call Skill(factorio-control) before using Factorio MCP tools."
                )
                self.log.debug(
                    "blocked Factorio MCP tool before skill: {}",
                    short_name,
                )
                return _deny_pre_tool_use(message)

        return {}


class AgentTickWatchdog:
    """Abort a single SDK tick that is looping without useful game progress."""

    def __init__(
        self,
        *,
        same_failure_limit: int | None = None,
        no_progress_timeout_s: float | None = None,
        clock=time.monotonic,
    ):
        self.same_failure_limit = (
            _WATCHDOG_SAME_FAILURE_LIMIT
            if same_failure_limit is None
            else int(same_failure_limit)
        )
        self.no_progress_timeout_s = (
            _WATCHDOG_NO_PROGRESS_TIMEOUT_S
            if no_progress_timeout_s is None
            else float(no_progress_timeout_s)
        )
        self.clock = clock
        self.started_at = self.clock()
        self.last_progress_at = self.started_at
        self._tool_names: dict[str, str] = {}
        self._last_failure_signature: str | None = None
        self._same_failure_count = 0

    def record_tool_use(self, tool_use_id: str | None, tool_name: str) -> None:
        if tool_use_id:
            self._tool_names[str(tool_use_id)] = str(tool_name)
        self.check_no_progress()

    def observe_text(self) -> None:
        self.check_no_progress()

    def observe_tool_result(
        self,
        tool_use_id: str | None,
        classification: str,
        text: str,
    ) -> None:
        tool_name = self._tool_names.get(str(tool_use_id), "") if tool_use_id else ""
        if classification == TOOL_RESULT_OK:
            if (
                _is_mutating_factorio_tool(tool_name)
                and _tool_result_indicates_progress(text)
            ):
                self.mark_progress()
            self._last_failure_signature = None
            self._same_failure_count = 0
            self.check_no_progress()
            return

        if classification == TOOL_RESULT_EXPECTED_MISS:
            self.check_no_progress()
            return

        if classification == TOOL_RESULT_GAME_REJECTED:
            signature = self._failure_signature(tool_name, classification, text)
            if signature == self._last_failure_signature:
                self._same_failure_count += 1
            else:
                self._last_failure_signature = signature
                self._same_failure_count = 1
            if (
                self.same_failure_limit > 0
                and self._same_failure_count >= self.same_failure_limit
            ):
                short_tool = _short_factorio_tool_name(tool_name) or "tool"
                raise AgentTickWatchdogAbort(
                    "repeated same game rejection "
                    f"({self._same_failure_count}x) from {short_tool}: "
                    f"{_short_event_text(text)}"
                )
            self.check_no_progress()
            return

        self.check_no_progress()

    def mark_progress(self) -> None:
        self.last_progress_at = self.clock()

    def check_no_progress(self) -> None:
        if self.no_progress_timeout_s <= 0:
            return
        elapsed = self.clock() - self.last_progress_at
        if elapsed >= self.no_progress_timeout_s:
            raise AgentTickWatchdogAbort(
                "no successful mutating progress for "
                f"{elapsed:.0f}s during active tick"
            )

    def _failure_signature(self, tool_name: str, classification: str, text: str) -> str:
        return "|".join([
            _short_factorio_tool_name(tool_name),
            classification,
            _short_event_text(text),
        ])


def _tool_result_indicates_progress(text: str) -> bool:
    stripped = str(text).strip()
    if not stripped:
        return False
    try:
        parsed = json.loads(stripped)
    except (TypeError, ValueError):
        return True
    return _json_payload_indicates_progress(parsed)


def _json_payload_indicates_progress(value: Any) -> bool:
    if isinstance(value, dict):
        if value.get("success") is False:
            return False
        if value.get("error"):
            return False
        if value.get("type") == "text":
            text = str(value.get("text", "")).strip()
            if not text:
                return False
            try:
                nested = json.loads(text)
            except (TypeError, ValueError):
                return not _looks_like_tool_error(text)
            return _json_payload_indicates_progress(nested)
        return True
    if isinstance(value, list):
        return any(_json_payload_indicates_progress(item) for item in value)
    return True


def _deny_pre_tool_use(message: str) -> dict[str, Any]:
    return {
        "decision": "block",
        "reason": message,
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": message,
        },
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
                if _is_benign_tool_miss(str(item)):
                    return False
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
    return _classify_tool_result(text) in TOOL_RESULT_FAILURE_CLASSES


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


def _is_context_window_limit(text: str) -> bool:
    lowered = str(text).lower()
    return (
        "context window limit" in lowered
        or "context length" in lowered
        or "maximum context" in lowered
    )


def _handle_context_window_limit(
    *,
    agent_name: str,
    session_id: str | None,
    log,
    telemetry: Telemetry | None,
    telemetry_name: str,
    rcon: RCONClient,
    player_index: int,
    rcon_target: str,
) -> str:
    clear_session(agent_name)
    if session_id:
        error_msg = (
            "Error: SDK context window limit reached; cleared saved session. "
            "The next tick will start a fresh SDK session."
        )
        log.warning("sdk context window limit; cleared session for {}", agent_name)
    else:
        cooldown_until = _set_context_window_cooldown(agent_name, log)
        error_msg = _context_window_message(cooldown_until)
        log.warning(
            "sdk context window limit persisted after session reset for {}; backing off",
            agent_name,
        )
    emit_error(telemetry, error_msg, agent=telemetry_name)
    if player_index > 0:
        send_response(rcon, player_index, rcon_target, error_msg)
        set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
    return SESSION_RESET


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


def _resolve_sdk_skills(value: Any = None) -> list[str] | str:
    if value is None:
        value = os.environ.get("BRIDGE_SDK_SKILLS", DEFAULT_SDK_SKILLS)
    if isinstance(value, list):
        return [str(item).strip() for item in value if str(item).strip()]
    raw = str(value).strip()
    if not raw:
        return []
    lowered = raw.lower()
    if lowered in {"0", "false", "no", "none", "off", "disabled"}:
        return []
    if lowered == "all":
        return "all"
    return [item.strip() for item in raw.split(",") if item.strip()]


def _claude_tools_for_sdk_skills(skills: list[str] | str) -> list[str]:
    # The SDK documents `skills=` as auto-configuring the Skill tool, but the
    # Claude Code init stream used by this bridge still reports `skill_tool=no`
    # without this explicit entry. Keep the explicit tool until the live init
    # payload proves the native path works here.
    return ["Skill"] if skills else []


def _setting_sources_for_sdk_skills(skills: list[str] | str) -> list[str] | None:
    return ["project", "local"] if skills else ["local"]


def _bounded_list_for_log(value: Any, limit: int = 12) -> list[str]:
    if not isinstance(value, list):
        return []
    items = [str(item) for item in value[:limit]]
    if len(value) > limit:
        items.append(f"...+{len(value) - limit}")
    return items


def _log_sdk_init(msg: SystemMessage, options: ClaudeAgentOptions, log) -> bool:
    if getattr(msg, "subtype", None) != "init":
        return False
    data = getattr(msg, "data", None)
    if not isinstance(data, dict):
        return False
    tools = data.get("tools", [])
    visible_skills = data.get("skills", [])
    skill_tool = isinstance(tools, list) and "Skill" in tools
    log.info(
        "sdk init: cwd={} skill_tool={} configured_skills={} visible_skills={}",
        data.get("cwd"),
        "yes" if skill_tool else "no",
        options.skills if options.skills is not None else "default",
        _bounded_list_for_log(visible_skills),
    )
    return True


def _should_log_system_message(msg: SystemMessage) -> bool:
    return getattr(msg, "subtype", None) not in {"thinking_tokens"}


def _is_skill_tool(block: ToolUseBlock) -> bool:
    return block.name == "Skill" or block.name.endswith("__Skill")


def _parse_utc_offset(value: str | None) -> timezone:
    raw = str(value or "+08:00").strip()
    match = re.fullmatch(r"([+-])(\d{1,2})(?::?(\d{2}))?", raw)
    if not match:
        return timezone(timedelta(hours=8))
    sign, hours_raw, minutes_raw = match.groups()
    hours = int(hours_raw)
    minutes = int(minutes_raw or "0")
    if hours > 23 or minutes > 59:
        return timezone(timedelta(hours=8))
    delta = timedelta(hours=hours, minutes=minutes)
    if sign == "-":
        delta = -delta
    return timezone(delta)


def _infer_provider_timezone(text: str, now: datetime | None = None) -> timezone | None:
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    now_utc_naive = now.astimezone(timezone.utc).replace(tzinfo=None)
    for stamp in reversed(_PROVIDER_TIMESTAMP_RE.findall(str(text))):
        try:
            provider_naive = datetime.strptime(stamp, "%Y%m%d%H%M%S")
        except ValueError:
            continue
        delta_minutes = round((provider_naive - now_utc_naive).total_seconds() / 60)
        rounded_minutes = int(round(delta_minutes / 15) * 15)
        if -12 * 60 <= rounded_minutes <= 14 * 60:
            return timezone(timedelta(minutes=rounded_minutes))
    return None


def _usage_limit_reset_at(text: str, now: datetime | None = None) -> datetime | None:
    match = _USAGE_LIMIT_RESET_RE.search(str(text))
    if not match:
        return None
    try:
        reset_naive = datetime.strptime(match.group("reset"), "%Y-%m-%d %H:%M:%S")
    except ValueError:
        return None
    provider_tz = (
        _infer_provider_timezone(text, now)
        or _parse_utc_offset(os.environ.get("BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET"))
    )
    return reset_naive.replace(tzinfo=provider_tz)


def _format_local_time(moment: datetime) -> str:
    local = moment.astimezone()
    zone = local.tzname() or local.strftime("%z")
    return f"{local:%Y-%m-%d %H:%M:%S} {zone}"


def _usage_limit_message(reset_at: datetime) -> str:
    return (
        "Provider usage limit is active. "
        f"Agent attempts will resume after {_format_local_time(reset_at)}."
    )


def _context_window_backoff_s() -> float:
    try:
        return max(
            1.0,
            float(os.environ.get("BRIDGE_CONTEXT_WINDOW_BACKOFF_S", "900")),
        )
    except (TypeError, ValueError):
        return 900.0


def _context_window_message(reset_at: datetime) -> str:
    return (
        "SDK context-window limit repeated after session reset. "
        f"Agent attempts will resume after {_format_local_time(reset_at)}."
    )


def _set_context_window_cooldown(
    agent_name: str,
    log=None,
    now: datetime | None = None,
    seconds: float | None = None,
) -> datetime:
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    delay_s = seconds if seconds is not None else _context_window_backoff_s()
    reset_at = now + timedelta(seconds=delay_s)

    changed = False
    with _CONTEXT_WINDOW_COOLDOWNS_LOCK:
        existing = _CONTEXT_WINDOW_COOLDOWNS.get(agent_name)
        if existing is None or reset_at > existing:
            _CONTEXT_WINDOW_COOLDOWNS[agent_name] = reset_at
            changed = True
        else:
            reset_at = existing
    if log and changed:
        log.info(
            "sdk context-window cooldown active until {}; pausing agent attempts",
            _format_local_time(reset_at),
        )
    return reset_at


def _get_context_window_cooldown(
    agent_name: str,
    now: datetime | None = None,
) -> datetime | None:
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    with _CONTEXT_WINDOW_COOLDOWNS_LOCK:
        reset_at = _CONTEXT_WINDOW_COOLDOWNS.get(agent_name)
        if not reset_at:
            return None
        if reset_at <= now:
            _CONTEXT_WINDOW_COOLDOWNS.pop(agent_name, None)
            return None
        return reset_at


def _set_usage_limit_cooldown(
    agent_name: str,
    text: str,
    log=None,
    now: datetime | None = None,
) -> datetime | None:
    reset_at = _usage_limit_reset_at(text, now)
    if not reset_at:
        return None
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    if reset_at <= now:
        return None

    changed = False
    with _USAGE_LIMIT_COOLDOWNS_LOCK:
        existing = _USAGE_LIMIT_COOLDOWNS.get(agent_name)
        if existing is None or reset_at > existing:
            _USAGE_LIMIT_COOLDOWNS[agent_name] = reset_at
            changed = True
        else:
            reset_at = existing
    if log and changed:
        log.info(
            "provider usage limit active until {}; pausing agent attempts",
            _format_local_time(reset_at),
        )
    return reset_at


def _get_usage_limit_cooldown(
    agent_name: str,
    now: datetime | None = None,
) -> datetime | None:
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    with _USAGE_LIMIT_COOLDOWNS_LOCK:
        reset_at = _USAGE_LIMIT_COOLDOWNS.get(agent_name)
        if not reset_at:
            return None
        if reset_at <= now:
            _USAGE_LIMIT_COOLDOWNS.pop(agent_name, None)
            return None
        return reset_at


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
# turns, not a stalled TCP connection or a model response that never yields, so
# a tick is also wrapped in asyncio.wait_for. Override via BRIDGE_TICK_TIMEOUT_S.
_TICK_TIMEOUT_S = float(os.environ.get("BRIDGE_TICK_TIMEOUT_S", "2400"))

# A long tick is fine if the SDK keeps emitting messages, but a long silent gap
# after a tool result leaves the game looking dropped. Abort that invocation and
# let the bridge resume on the next autonomy tick.
_STREAM_IDLE_TIMEOUT_S = float(os.environ.get("BRIDGE_STREAM_IDLE_TIMEOUT_S", "300"))

# Abort a single active tick when it is making no useful game progress. This is
# deliberately separate from session reset: a stuck layout is not a dead SDK
# session.
_WATCHDOG_SAME_FAILURE_LIMIT = int(os.environ.get("BRIDGE_WATCHDOG_SAME_FAILURE_LIMIT", "3"))
_WATCHDOG_NO_PROGRESS_TIMEOUT_S = float(os.environ.get("BRIDGE_WATCHDOG_NO_PROGRESS_TIMEOUT_S", "900"))


class AgentStreamIdleTimeout(TimeoutError):
    pass


class AgentTickWatchdogAbort(TimeoutError):
    pass


async def _query_with_idle_timeout(prompt: str, options: ClaudeAgentOptions):
    stream = query(prompt=prompt, options=options)
    iterator = stream.__aiter__()
    try:
        while True:
            try:
                msg = await asyncio.wait_for(
                    iterator.__anext__(),
                    timeout=_STREAM_IDLE_TIMEOUT_S,
                )
            except StopAsyncIteration:
                break
            except asyncio.TimeoutError as exc:
                raise AgentStreamIdleTimeout(
                    f"agent stream idle for {_STREAM_IDLE_TIMEOUT_S:.0f}s"
                ) from exc
            yield msg
    finally:
        aclose = getattr(iterator, "aclose", None)
        if callable(aclose):
            await aclose()


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
    watchdog = AgentTickWatchdog()

    async for msg in _query_with_idle_timeout(prompt=prompt, options=options):
        if isinstance(msg, AssistantMessage):
            if msg.session_id:
                new_session_id = msg.session_id
            for block in msg.content:
                if isinstance(block, TextBlock):
                    text_parts.append(block.text)
                    _set_usage_limit_cooldown(agent_name, block.text, log)
                    if not _USAGE_LIMIT_RESET_RE.search(block.text):
                        watchdog.observe_text()
                    log.info("text: {}", block.text.strip())
                elif isinstance(block, ToolUseBlock):
                    watchdog.record_tool_use(getattr(block, "id", None), block.name)
                    display = _short_tool_name(block.name)
                    if _is_skill_tool(block):
                        log.info("skill: {}({})", display, _json_for_log(block.input))
                    else:
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
                classification = _log_tool_result(agent_name, log, text, sdk_is_error=False)
                watchdog.observe_tool_result(None, classification, text)
                if player_messages:
                    log.info("player_messages: {}", player_messages)
            else:
                for block in msg.content:
                    if isinstance(block, ToolResultBlock):
                        text, player_messages = _result_text_and_player_messages(block.content)
                        classification = _log_tool_result(
                            agent_name,
                            log,
                            text,
                            sdk_is_error=bool(block.is_error),
                        )
                        watchdog.observe_tool_result(
                            getattr(block, "tool_use_id", None),
                            classification,
                            text,
                        )
                        if player_messages:
                            log.info("player_messages: {}", player_messages)
        elif isinstance(msg, ResultMessage):
            new_session_id = msg.session_id or new_session_id
            if msg.result and msg.result not in text_parts:
                text_parts.append(msg.result)
            if msg.is_error:
                detail = msg.result or "; ".join(msg.errors or []) or "agent result marked as error"
                if _is_context_window_limit(detail):
                    log.warning(
                        "result sdk_context_window: {}; clearing SDK session before next attempt",
                        detail,
                    )
                elif not _set_usage_limit_cooldown(agent_name, detail, log):
                    classification = TOOL_RESULT_SDK_FAILURE
                    log.warning("result {}: {}", classification, detail)
                    append_event(
                        agent_name,
                        "failure",
                        f"{classification}: {_short_event_text(detail)}",
                    )
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
            if not _log_sdk_init(msg, options, log):
                if _should_log_system_message(msg):
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
    apply_learning_update(agent_name, reply)
    if ledger_update and ledger_update.get("progress"):
        append_event(agent_name, "progress", ledger_update["progress"])
    _record_anomaly(reply, agent_name)
    reply = strip_ledger_trailer(reply)
    reply = strip_reflection_trailer(reply)
    reply = strip_learning_trailers(reply)
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
    sdk_skills: list[str] | str | None = None,
    read_only_tools: bool = False,
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
    read_only_tool_gate = PlannerReadOnlyToolGate(log, enabled=read_only_tools)
    resolved_sdk_skills = (
        _resolve_sdk_skills(sdk_skills)
        if sdk_skills is not None
        else _resolve_sdk_skills()
    )
    factorio_skill_gate = FactorioSkillGate(
        log,
        required="factorio-control" in resolved_sdk_skills,
    )
    factorio_schema_gate = FactorioToolSchemaGate(log)
    options = ClaudeAgentOptions(
        system_prompt=system_prompt,
        model=model,
        max_turns=_resolve_max_turns(max_turns),
        mcp_servers=mcp_config,
        strict_mcp_config=True,
        tools=_claude_tools_for_sdk_skills(resolved_sdk_skills),
        disallowed_tools=_disallowed_tools_for_env(env),
        permission_mode="bypassPermissions",
        resume=session_id,
        setting_sources=_setting_sources_for_sdk_skills(resolved_sdk_skills),
        cwd=_PROJECT_ROOT,
        skills=resolved_sdk_skills,
        env=env,
        hooks={
            "PreToolUse": [
                HookMatcher(hooks=[
                    read_only_tool_gate.hook,
                    factorio_skill_gate.hook,
                    factorio_schema_gate.hook,
                    mutating_tool_gate.hook,
                ])
            ],
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
        if any(_is_context_window_limit(part) for part in text_parts):
            return _handle_context_window_limit(
                agent_name=agent_name,
                session_id=session_id,
                log=log,
                telemetry=telemetry,
                telemetry_name=tname,
                rcon=rcon,
                player_index=player_index,
                rcon_target=rcon_target,
            )

        cooldown_until = _get_usage_limit_cooldown(agent_name)
        if cooldown_until and any(_USAGE_LIMIT_RESET_RE.search(part) for part in text_parts):
            text_parts = [_usage_limit_message(cooldown_until)]
    except AgentStreamIdleTimeout:
        error_msg = (
            f"Error: agent stream was idle for {_STREAM_IDLE_TIMEOUT_S:.0f}s "
            "and was aborted"
        )
        log.error(
            "agent stream idle timeout after {:.0f}s; aborting invocation",
            _STREAM_IDLE_TIMEOUT_S,
        )
        append_event(
            agent_name, "failure",
            _short_event_text(f"stream idle timeout after {_STREAM_IDLE_TIMEOUT_S:.0f}s"),
        )
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return session_id
    except AgentTickWatchdogAbort as e:
        reason = _short_event_text(str(e))
        error_msg = f"Error: watchdog aborted stuck tick: {reason}"
        log.warning("watchdog aborted stuck tick: {}", reason)
        append_event(agent_name, "failure", f"watchdog_abort: {reason}")
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return session_id
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
        cooldown_until = (
            _set_usage_limit_cooldown(agent_name, str(e), log)
            or _get_usage_limit_cooldown(agent_name)
        )
        if _is_context_window_limit(str(e)):
            return _handle_context_window_limit(
                agent_name=agent_name,
                session_id=session_id,
                log=log,
                telemetry=telemetry,
                telemetry_name=tname,
                rcon=rcon,
                player_index=player_index,
                rcon_target=rcon_target,
            )
        if _is_sdk_terminal_error_echo(str(e)):
            if cooldown_until:
                log.debug(
                    "agent invocation paused by provider usage limit until {}",
                    _format_local_time(cooldown_until),
                )
                error_msg = _usage_limit_message(cooldown_until)
            else:
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
            raw_agent = json.loads(f.read_text())
            profile = AgentProfile.from_mapping(raw_agent)
        except (json.JSONDecodeError, OSError, ValueError):
            continue
        if profile.group == group:
            profiles.append(load_agent(profile.name))
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
                 max_turns: int | None = None,
                 sdk_skills: list[str] | str | None = None):
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
        self.sdk_skills = (
            _resolve_sdk_skills(sdk_skills)
            if sdk_skills is not None
            else _resolve_sdk_skills(agent.get("sdk_skills"))
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
        self._planner_interval = int(
            agent.get("planner_interval", planner_interval)
        )
        # A bridge restart or `just resume` keeps the Factorio save and ledger
        # but clears the SDK session. Reassess once before executing old plan
        # steps so live structures in the save can supersede stale progress.
        self._exec_ticks_since_plan = self._planner_interval
        self._reflect_interval = int(agent.get("reflect_interval", 16))
        self._planner_model = agent.get("planner_model") or "sonnet"
        # When True, autonomy ticks only fire while a human is connected to the
        # server, so the agent waits to "do its own thing" until you join (and
        # goes back to idle if you leave). Chat is always processed regardless.
        self.autonomy_requires_player = bool(
            agent.get("autonomy_requires_player", autonomy_requires_player)
        )
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
        """True if at least one human player is connected.

        AI agents are orphan character entities, so the mod-side remote counts
        only real client connections. On any RCON error, return False so we
        don't burn autonomy turns when we can't confirm a human is present.
        """
        try:
            lua = f'{_RCON_PRINT}(tostring(remote.call("claude_interface", "connected_player_count")))'
            out = self.rcon.execute(f"/silent-command {lua}")
            return int(out.strip() or "0") > 0
        except Exception as e:
            self.log.debug("human-connected check failed: {}", e)
            return False

    def _live_state_line(self) -> str:
        """Best-effort one-line live state for autonomy ticks."""
        try:
            agent = lua_long_string(self.agent_name)
            lua = f'{_RCON_PRINT}(remote.call("claude_interface", "live_state_line", {agent}))'
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
        events = load_events(self.agent_name, 20)
        memory = render_memory(events, load_reflection(self.agent_name))
        ledger_text = render_ledger(ledger)
        learned_text = render_accepted_learning(load_accepted_learning())
        live_state = self._live_state_line()
        mode = choose_autonomy_mode(
            ledger, self._exec_ticks_since_plan, self._planner_interval,
        )
        live_stale_reason = objective_satisfied_by_live_state(ledger, live_state)
        reflect_due = should_reflect(
            count_events(self.agent_name), getattr(self, "_reflect_interval", 16),
        )
        # If the last tick reported the plan/objective finished, re-plan NOW
        # instead of spinning "plan complete" for planner_interval ticks.
        if mode == "execute" and _events_indicate_plan_done(events):
            mode = "plan"
        if mode == "execute" and live_stale_reason:
            mode = "plan"
        if mode == "execute" and reflect_due:
            mode = "plan"
        if mode == "plan":
            self._exec_ticks_since_plan = 0
        else:
            self._exec_ticks_since_plan += 1
        parts = [memory, ledger_text, learned_text]
        continuity_parts = [part for part in parts if part]
        if live_stale_reason:
            live_state = "\n".join([
                live_state,
                f"Live-state completion signal: {live_stale_reason}",
            ]).strip()

        message = build_autonomy_prompt(
            mode, "\n\n".join(continuity_parts), live_state,
        )
        if mode == "plan":
            message = "\n\n".join([message, learning_proposal_prompt()])
        if reflect_due:
            message = "\n\n".join([
                message,
                "This is a reflection turn: emit a hidden <reflection> block "
                "summarizing only durable built structures and short gameplay "
                "mistake-avoidance tips. Do not include provider limits, SDK "
                "session failures, timeouts, max-turn failures, or fresh-start "
                "non-lessons. Use exactly this format:\n"
                "<reflection>\n"
                "structures:\n"
                "- what durable structure is built where\n"
                "error_tips:\n"
                "- short gameplay mistake to avoid next time\n"
                "</reflection>",
            ])

        tick = {
            "message": message,
            "player_index": 0,
            "player_name": "autonomy",
            "autonomy": True,
        }
        if mode == "plan" or reflect_due:
            tick["read_only_tools"] = True
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
                if _get_usage_limit_cooldown(self.agent_name):
                    continue
                if _get_context_window_cooldown(self.agent_name):
                    continue
                if self.autonomy_requires_player and not self._human_connected():
                    continue
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

        cooldown_until = _get_usage_limit_cooldown(self.agent_name)
        if cooldown_until:
            if player_index > 0:
                try:
                    send_response(
                        self.rcon,
                        player_index,
                        response_to or self.agent_name,
                        _usage_limit_message(cooldown_until),
                    )
                    set_status(self.rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
                except Exception as e:
                    self.log.debug("rate-limit status reply failed: {}", e)
            return

        context_cooldown_until = _get_context_window_cooldown(self.agent_name)
        if context_cooldown_until:
            if player_index > 0:
                try:
                    send_response(
                        self.rcon,
                        player_index,
                        response_to or self.agent_name,
                        _context_window_message(context_cooldown_until),
                    )
                    set_status(self.rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
                except Exception as e:
                    self.log.debug("context-window cooldown status reply failed: {}", e)
            return

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
            max_turns=self.max_turns, sdk_skills=self.sdk_skills,
            read_only_tools=bool(msg.get("read_only_tools")),
        )
        if new_session == SESSION_RESET:
            self.session_id = None
            return
        if new_session:
            self.session_id = new_session
            save_session(self.agent_name, self.session_id)


def main_multi(args, agent_profiles: list[dict]):
    """Multi-agent mode: one thread per agent, shared watcher."""
    log = logger.bind(agent="system")
    # Shared RCON (thread-safe)
    log.info("Connecting to Factorio RCON...")
    rcon_raw = RCONClient(
        args.rcon_host,
        args.rcon_port,
        args.rcon_password,
        log=log,
    )
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
    sdk_skills = _resolve_sdk_skills(args.sdk_skills)
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
                         max_turns=args.max_turns,
                         sdk_skills=sdk_skills)
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
    log.info("SDK skills: {}", sdk_skills if sdk_skills else "disabled")
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
    parser.add_argument(
        "--sdk-skills",
        default=None,
        help=(
            "Claude Code SDK skills to expose: comma list, 'all', or 'none' "
            f"(default: {DEFAULT_SDK_SKILLS}; env BRIDGE_SDK_SKILLS)"
        ),
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
    sdk_skills = _resolve_sdk_skills(
        args.sdk_skills if args.sdk_skills is not None else agent.get("sdk_skills")
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
    log.info("SDK skills: {}", sdk_skills if sdk_skills else "disabled")
    if mcp_bin:
        log.info("MCP server: {}", mcp_bin)
    else:
        log.warning("MCP server not found (chat-only)")

    # RCON
    log.info("Connecting to Factorio RCON...")
    rcon = RCONClient(args.rcon_host, args.rcon_port, args.rcon_password, log=log)
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

                cooldown_message = ""
                cooldown_until = _get_usage_limit_cooldown(agent_name)
                if cooldown_until:
                    cooldown_message = _usage_limit_message(cooldown_until)
                context_cooldown_until = _get_context_window_cooldown(agent_name)
                if context_cooldown_until:
                    cooldown_message = _context_window_message(context_cooldown_until)
                if cooldown_message:
                    if player_index > 0:
                        send_response(rcon, player_index, agent_name, cooldown_message)
                        set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
                    continue

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
                    sdk_skills=sdk_skills,
                    read_only_tools=bool(msg.get("read_only_tools")),
                )
                if new_session == SESSION_RESET:
                    session_id = None
                    continue
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
