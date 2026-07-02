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

from models import DotEnvFile

# Load .env
_env_file = Path(__file__).parent / ".env"
if _env_file.exists():
    DotEnvFile.from_text(_env_file.read_text()).apply_to_environ(os.environ)

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


from ledger import (apply_ledger_update_model, load_ledger_model,
                    parse_ledger_trailer_model, strip_ledger_trailer)
from journal import (append_event, apply_reflection_update_model, count_events,
                     load_events_model, load_reflection_model, render_memory,
                     should_reflect, strip_reflection_trailer)
from learning import (apply_learning_update, learning_proposal_prompt,
                      load_accepted_learning_model, render_accepted_learning,
                      strip_learning_trailers)
from models import (
    AgentProfile,
    AgentInvocationConfig,
    AgentInvocationExceptionSignal,
    AgentMessageResult,
    AgentNameSelection,
    AgentRunTranscript,
    AgentRuntimeConfig,
    AgentResponseFormat,
    AgentSessionIndex,
    AgentSessionState,
    AutonomyDecisionReason,
    AutonomyTickMessage,
    BridgeValidationError,
    BridgeInputMessage,
    BridgeLogMessage,
    BridgeRuntimeSettings,
    ConnectedPlayerCountResult,
    FactorioMcpServerConfig,
    FactorioModInfo,
    LiveState,
    ParsedAgentResponse,
    PreToolUseDecision,
    PreToolUseGuardBlock,
    PreToolUseHookResponse,
    ProviderUsageLimit,
    ProviderUsageLimitSettings,
    RawLuaPolicy,
    RconRemoteCall,
    SdkAssistantMessage,
    SdkAssistantTextObservation,
    SdkResultMessage,
    SdkSkillConfig,
    SdkStderrSignal,
    SdkSystemMessage,
    SdkToolUse,
    SdkUserToolResultMessage,
    TelemetryEvent,
    TelemetryRelaySettings,
    AutonomyPromptInput,
    TOOL_PARAM_BOOLEAN,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_STRING,
    ToolResultOutcome,
    ToolResultClassification,
    ToolResultContent,
    ToolResultLogLevel,
    ToolResultLogRecord,
    ToolParamSchemaRegistry,
    ToolCallRequest,
    WatchdogToolObservation,
)
from planner import (
    build_autonomy_prompt_model,
    choose_autonomy_decision,
    objective_completion_evidence,
    planner_advisory_for_decision,
)
from skills import strip_skill_trailer
from rcon import RCONClient, ThreadSafeRCON, lua_long_string
from paths import find_script_output, find_factorioctl_mcp
from transport import (InputWatcher, send_response, send_tool_status, set_status,
                       check_mod_loaded, register_agent, unregister_agent,
                       pre_place_character_model, setup_surfaces_model,
                       set_spectator_mode)
from paths import find_mod_source, find_mods_dir
from telemetry import SSEBroadcaster, start_sse_server, RelayPusher, Telemetry, emit_chat, emit_tool_call, emit_error, emit_status

_BRIDGE_DIR = Path(__file__).resolve().parent
_PROJECT_ROOT = _BRIDGE_DIR.parent.parent
_PLAYER_MESSAGES_MARKER = "\n\n--- Player Messages ---\n"
DEFAULT_MAX_TURNS = 200
DEFAULT_SDK_SKILLS = "factorio-control"
SESSIONS_FILE = _BRIDGE_DIR / ".sessions.json"
_USAGE_LIMIT_COOLDOWNS: dict[str, datetime] = {}
_USAGE_LIMIT_COOLDOWNS_LOCK = threading.Lock()
_CONTEXT_WINDOW_COOLDOWNS: dict[str, datetime] = {}
_CONTEXT_WINDOW_COOLDOWNS_LOCK = threading.Lock()

# ── Agent profiles ───────────────────────────────────────────

def load_agent(agent_name: str) -> AgentProfile:
    """Load and validate agent profile from bridge/agents/{name}.json.
    If response_format is present, auto-generates and appends format instructions."""
    agent_file = _BRIDGE_DIR / "agents" / f"{agent_name}.json"
    if not agent_file.exists():
        raise FileNotFoundError(
            f"Agent profile not found: {agent_file}\n"
            f"Create it or use --agent default"
        )
    profile = AgentProfile.from_file_text(agent_file.read_text())
    # Auto-generate formatting instructions from response_format
    fmt = profile.response_format
    if fmt:
        instructions = build_format_instructions(fmt)
        profile = profile.with_system_prompt(profile.system_prompt + "\n\n" + instructions)
    return profile


# ── Response formatting ───────────────────────────────────────

def build_format_instructions(fmt: dict | AgentResponseFormat) -> str:
    """Generate system prompt formatting instructions from response_format config."""
    response_format = (
        fmt if isinstance(fmt, AgentResponseFormat) else AgentResponseFormat.coerce(fmt)
    )
    if response_format is None:
        return ""
    header_label = response_format.header_label
    header_color = response_format.header_color
    action_label = response_format.action_label
    action_color = response_format.action_color
    footer_label = response_format.footer_label
    footer_color = response_format.footer_color
    sections = response_format.sections

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
        color = sec.color
        description = sec.description or sec.label.lower()
        lines.append("")
        lines.append(f"  [color={color}]{sec.label}:[/color] <{description}>")
    if footer_label:
        lines.append("")
        lines.append(f"  [color={footer_color}]{footer_label}:[/color] <closing status>")
    lines.append("")
    lines.append("Rules: No markdown (**, ##, ```). The [color=r,g,b]...[/color] tags are mandatory, not optional.")
    return "\n".join(lines)


def parse_response_model(text: str) -> ParsedAgentResponse:
    """Parse a rich-text agent response into structured sections.
    Falls back to a body-only parsed response when no sections exist."""
    return ParsedAgentResponse.from_text(text)


def parse_response(text: str) -> dict:
    """Parse a rich-text agent response into the telemetry dict shape."""
    return parse_response_model(text).to_dict()


def sanitize_response(text: str) -> str:
    """Remove markdown artifacts while preserving Factorio rich text tags."""
    return ParsedAgentResponse.sanitize_text(text)


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
            return AgentSessionState.from_file_text(f.read_text()).session_id
        except (BridgeValidationError, OSError):
            return None
    # Backward compat: check old shared file
    if SESSIONS_FILE.exists():
        try:
            return AgentSessionIndex.from_file_text(SESSIONS_FILE.read_text()).get(agent_name)
        except (BridgeValidationError, OSError):
            return None
    return None


def save_session(agent_name: str, session_id: str):
    """Persist session ID for an agent (per-agent file, thread-safe)."""
    f = _session_file(agent_name)
    f.write_text(AgentSessionState(session_id=session_id).to_json_line())


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
        sessions = AgentSessionIndex.from_file_text(SESSIONS_FILE.read_text())
    except (BridgeValidationError, OSError):
        return
    if agent_name not in sessions.sessions:
        return
    sessions = sessions.without(agent_name)
    try:
        SESSIONS_FILE.write_text(sessions.to_legacy_json_line())
    except OSError:
        pass


# ── MCP config ───────────────────────────────────────────────

McpServersConfig = dict[str, McpStdioServerConfig]


def build_mcp_servers(
    mcp_bin: str, rcon_host: str, rcon_port: int,
    rcon_password: str, agent_id: str = "default",
) -> McpServersConfig:
    """Build inline SDK MCP config for the factorioctl stdio server."""
    return FactorioMcpServerConfig(
        command=mcp_bin,
        rcon_host=rcon_host,
        rcon_port=rcon_port,
        rcon_password=rcon_password,
        agent_id=agent_id,
    ).to_sdk_config()


# ── Claude SDK ───────────────────────────────────────────────


def _short_tool_name(name: str) -> str:
    return SdkToolUse.display_name_for(name)


def _result_text(content: str | list[dict[str, Any]] | None) -> str:
    return ToolResultContent.from_sdk_content(
        content,
        player_marker=_PLAYER_MESSAGES_MARKER,
    ).text


def _result_text_and_player_messages(
    content: str | list[dict[str, Any]] | None,
) -> tuple[str, str]:
    result = ToolResultContent.from_sdk_content(
        content,
        player_marker=_PLAYER_MESSAGES_MARKER,
    )
    return result.text, result.player_message_text


_FACTORIO_TOOL_PARAM_SCHEMA_REGISTRY = ToolParamSchemaRegistry.from_mapping({
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
    "build_fuel_supply": {
        "required": {
            "consumer_unit_number": TOOL_PARAM_INTEGER,
            "from_x": TOOL_PARAM_INTEGER,
            "from_y": TOOL_PARAM_INTEGER,
            "pickup_x": TOOL_PARAM_INTEGER,
            "pickup_y": TOOL_PARAM_INTEGER,
            "inserter_x": TOOL_PARAM_NUMBER,
            "inserter_y": TOOL_PARAM_NUMBER,
            "inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "inserter_name": TOOL_PARAM_STRING,
            "inserter_fuel_item": TOOL_PARAM_STRING,
            "inserter_fuel_count": TOOL_PARAM_INTEGER,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "feed_lab_from_inventory": {
        "required": {
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "science_pack": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"dry_run": TOOL_PARAM_BOOLEAN},
    },
    "plan_automation_science": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "gear_from_x": TOOL_PARAM_INTEGER,
            "gear_from_y": TOOL_PARAM_INTEGER,
            "copper_from_x": TOOL_PARAM_INTEGER,
            "copper_from_y": TOOL_PARAM_INTEGER,
        },
        "optional": {
            "gear_side": TOOL_PARAM_STRING,
            "copper_side": TOOL_PARAM_STRING,
            "output_side": TOOL_PARAM_STRING,
            "lab_side": TOOL_PARAM_STRING,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_automation_science": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "gear_from_x": TOOL_PARAM_INTEGER,
            "gear_from_y": TOOL_PARAM_INTEGER,
            "gear_pickup_x": TOOL_PARAM_INTEGER,
            "gear_pickup_y": TOOL_PARAM_INTEGER,
            "gear_inserter_x": TOOL_PARAM_NUMBER,
            "gear_inserter_y": TOOL_PARAM_NUMBER,
            "gear_inserter_direction": TOOL_PARAM_STRING,
            "copper_from_x": TOOL_PARAM_INTEGER,
            "copper_from_y": TOOL_PARAM_INTEGER,
            "copper_pickup_x": TOOL_PARAM_INTEGER,
            "copper_pickup_y": TOOL_PARAM_INTEGER,
            "copper_inserter_x": TOOL_PARAM_NUMBER,
            "copper_inserter_y": TOOL_PARAM_NUMBER,
            "copper_inserter_direction": TOOL_PARAM_STRING,
            "science_drop_x": TOOL_PARAM_INTEGER,
            "science_drop_y": TOOL_PARAM_INTEGER,
            "science_to_x": TOOL_PARAM_INTEGER,
            "science_to_y": TOOL_PARAM_INTEGER,
            "output_inserter_x": TOOL_PARAM_NUMBER,
            "output_inserter_y": TOOL_PARAM_NUMBER,
            "output_inserter_direction": TOOL_PARAM_STRING,
            "lab_from_x": TOOL_PARAM_INTEGER,
            "lab_from_y": TOOL_PARAM_INTEGER,
            "lab_pickup_x": TOOL_PARAM_INTEGER,
            "lab_pickup_y": TOOL_PARAM_INTEGER,
            "lab_inserter_x": TOOL_PARAM_NUMBER,
            "lab_inserter_y": TOOL_PARAM_NUMBER,
            "lab_inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "plan_recipe_assembler_cell": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
            "input_item_name": TOOL_PARAM_STRING,
            "output_item_name": TOOL_PARAM_STRING,
            "input_from_x": TOOL_PARAM_INTEGER,
            "input_from_y": TOOL_PARAM_INTEGER,
            "output_to_x": TOOL_PARAM_INTEGER,
            "output_to_y": TOOL_PARAM_INTEGER,
        },
        "optional": {
            "input_side": TOOL_PARAM_STRING,
            "output_side": TOOL_PARAM_STRING,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_recipe_assembler_cell": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
            "input_item_name": TOOL_PARAM_STRING,
            "output_item_name": TOOL_PARAM_STRING,
            "input_from_x": TOOL_PARAM_INTEGER,
            "input_from_y": TOOL_PARAM_INTEGER,
            "input_pickup_x": TOOL_PARAM_INTEGER,
            "input_pickup_y": TOOL_PARAM_INTEGER,
            "input_inserter_x": TOOL_PARAM_NUMBER,
            "input_inserter_y": TOOL_PARAM_NUMBER,
            "input_inserter_direction": TOOL_PARAM_STRING,
            "output_drop_x": TOOL_PARAM_INTEGER,
            "output_drop_y": TOOL_PARAM_INTEGER,
            "output_to_x": TOOL_PARAM_INTEGER,
            "output_to_y": TOOL_PARAM_INTEGER,
            "output_inserter_x": TOOL_PARAM_NUMBER,
            "output_inserter_y": TOOL_PARAM_NUMBER,
            "output_inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
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
    "execute_entity_placement_near": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "limit": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
        },
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
    "execute_edge_miner": {
        "required": {
            "resource_type": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "drill_name": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "fuel_item": TOOL_PARAM_STRING,
            "fuel_count": TOOL_PARAM_INTEGER,
            "verify_radius": TOOL_PARAM_INTEGER,
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
})


def _tool_request_from_hook_input(hook_input: Any) -> ToolCallRequest | None:
    try:
        return ToolCallRequest.from_hook_input(hook_input)
    except BridgeValidationError:
        return None


def _log_tool_result(
    agent_name: str,
    log,
    text: str,
    sdk_is_error: bool = False,
    outcome: ToolResultOutcome | None = None,
) -> ToolResultClassification:
    if outcome is None:
        outcome = ToolResultOutcome.from_text(text, sdk_is_error=sdk_is_error)
    record = ToolResultLogRecord.from_outcome(outcome, text=text)
    return _log_tool_result_record(agent_name, log, record)


def _log_tool_result_record(
    agent_name: str,
    log,
    record: ToolResultLogRecord,
) -> ToolResultClassification:
    if record.should_emit_log:
        if record.log_level == ToolResultLogLevel.INFO:
            log.info("{}: {}", record.log_label, record.text)
        elif record.log_level == ToolResultLogLevel.WARNING:
            log.warning("{}: {}", record.log_label, record.text)
        else:
            log.debug("{}: {}", record.log_label, record.text)
    if record.should_journal_failure:
        append_event(agent_name, "failure", record.journal_failure_text)
    return record.classification


class MutatingToolBatchGate:
    """Block same-message mutating MCP batches before they race inventory state."""

    def __init__(self, log, window_s: float | None = None):
        self.log = log
        runtime = _runtime_settings()
        self.window_s = BridgeRuntimeSettings(
            mutating_tool_batch_window_s=(
                window_s
                if window_s is not None
                else runtime.mutating_tool_batch_window_s
            )
        ).mutating_tool_batch_window_s
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
        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_mutating_factorio_tool:
            return PreToolUseHookResponse.noop().to_dict()

        now = time.monotonic()
        short_name = request.short_name
        async with self._lock:
            if (
                self._last_tool_use_id
                and tool_use_id != self._last_tool_use_id
                and now - self._last_at < self.window_s
            ):
                previous = ToolCallRequest.short_factorio_tool_name(
                    self._last_tool_name or "",
                )
                block = PreToolUseGuardBlock.parallel_mutation(
                    tool_name=short_name,
                    previous_tool_name=previous,
                    elapsed_s=now - self._last_at,
                )
                self.log.debug(block.debug_message)
                return PreToolUseHookResponse.block(block).to_dict()

            self._last_at = now
            self._last_tool_use_id = tool_use_id
            self._last_tool_name = request.tool_name
        return PreToolUseHookResponse.allow().to_dict()


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
            return PreToolUseHookResponse.noop().to_dict()

        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.is_read_only_factorio_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.is_read_only_dry_run:
            return PreToolUseHookResponse.noop().to_dict()

        block = PreToolUseGuardBlock.read_only_turn(tool_name=request.short_name)
        self.log.debug(block.debug_message)
        return PreToolUseHookResponse.block(block).to_dict()


class ManualAutomationDriftGate:
    """Block manual transfer tools when the committed plan is stale automation."""

    MANUAL_TRANSFER_TOOLS = frozenset({
        "craft",
        "extract_items",
        "feed_lab_from_inventory",
        "hand_feed_furnace",
        "insert_items",
    })

    def __init__(
        self,
        log,
        agent_name: str,
        ledger_loader: Any = load_ledger_model,
        live_state_loader: Any | None = None,
    ):
        self.log = log
        self.agent_name = str(agent_name or "")
        self.ledger_loader = ledger_loader
        self.live_state_loader = live_state_loader

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.short_name not in self.MANUAL_TRANSFER_TOOLS:
            return PreToolUseHookResponse.noop().to_dict()

        try:
            ledger = self.ledger_loader(self.agent_name)
        except Exception as exc:
            self.log.debug("manual automation guard ledger lookup failed: {}", exc)
            return PreToolUseHookResponse.noop().to_dict()
        live_state = None
        if self.live_state_loader is not None:
            try:
                live_state = self.live_state_loader(self.agent_name)
            except Exception as exc:
                self.log.debug("manual automation guard live-state lookup failed: {}", exc)
        durable_recovery_context = self._ledger_has_durable_recovery_context(ledger)
        if durable_recovery_context and (
            request.is_manual_fuel_transfer or request.is_manual_material_transfer
        ):
            return PreToolUseHookResponse.noop().to_dict()
        bootstrap_infrastructure_context = self._ledger_has_bootstrap_infrastructure_context(ledger)
        if bootstrap_infrastructure_context and (
            request.is_manual_fuel_transfer or request.is_manual_material_transfer
        ):
            return PreToolUseHookResponse.noop().to_dict()
        if (
            request.is_bootstrap_infrastructure_craft
            and bootstrap_infrastructure_context
        ):
            return PreToolUseHookResponse.noop().to_dict()
        if (
            request.is_manual_fuel_transfer
            and live_state is not None
            and live_state.has_automation_capable_footprint()
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_science_transfer
            and live_state is not None
            and live_state.has_any((
                "assembling-machine-1",
                "assembling-machine-2",
                "assembling-machine-3",
            ))
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_material_transfer
            and live_state is not None
            and live_state.has_automation_capable_footprint()
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_component_craft
            and live_state is not None
            and live_state.has_any((
                "assembling-machine-1",
                "assembling-machine-2",
                "assembling-machine-3",
            ))
            and self._ledger_is_science_automation_context(ledger)
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if not ledger.has_stale_manual_automation_plan(live_state):
            return PreToolUseHookResponse.noop().to_dict()

        block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
        self.log.debug(block.debug_message)
        return PreToolUseHookResponse.block(block).to_dict()

    @staticmethod
    def _ledger_is_science_automation_context(ledger: Any) -> bool:
        try:
            text = str(ledger.active_text() or "")
        except Exception:
            text = ""
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "automation-science",
                "science pack",
                "red science",
                "plan_automation_science",
                "build_automation_science",
                "build_lab_feed",
            )
        )

    @staticmethod
    def _ledger_has_durable_recovery_context(ledger: Any) -> bool:
        try:
            text = str(ledger.active_text() or "").lower()
        except Exception:
            text = ""
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "route_belt",
                "build_fuel_supply",
            )
        )

    @staticmethod
    def _ledger_has_bootstrap_infrastructure_context(ledger: Any) -> bool:
        try:
            text = str(ledger.active_text() or "").lower()
        except Exception:
            text = ""
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "bootstrap",
                "build_recipe_assembler_cell",
                "durable automation",
                "furnace output",
                "inserter",
                "plan_recipe_assembler_cell",
                "plate output",
                "recipe assembler",
            )
        )


class FactorioToolSchemaGate:
    """Reject clearly malformed Factorio MCP parameters before Rust deserialization."""

    def __init__(self, log, schema_registry: Any = None):
        self.log = log
        try:
            self.schema_registry = ToolParamSchemaRegistry.from_mapping(
                _FACTORIO_TOOL_PARAM_SCHEMA_REGISTRY
                if schema_registry is None
                else schema_registry
            )
            self.schema_registry_error: BridgeValidationError | None = None
        except BridgeValidationError as exc:
            self.schema_registry = ToolParamSchemaRegistry()
            self.schema_registry_error = exc

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        try:
            request = ToolCallRequest.from_hook_input(hook_input)
        except BridgeValidationError as exc:
            block = PreToolUseGuardBlock.param_schema(detail=str(exc))
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        if not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()

        if self.schema_registry_error:
            block = PreToolUseGuardBlock.param_schema(
                detail=str(self.schema_registry_error),
                tool_name=request.short_name,
            )
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        try:
            self.schema_registry.validate_request(request)
        except BridgeValidationError as exc:
            block = PreToolUseGuardBlock.param_schema(
                detail=str(exc),
                tool_name=request.short_name,
            )
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        return PreToolUseHookResponse.noop().to_dict()


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
        request = _tool_request_from_hook_input(hook_input)
        if not self.required or request is None:
            return PreToolUseHookResponse.noop().to_dict()

        async with self._lock:
            if request.tool_name == "Skill":
                self._saw_skill = True
                return PreToolUseHookResponse.allow().to_dict()

            if request.is_factorio_mcp_tool and not self._saw_skill:
                block = PreToolUseGuardBlock.skill_required(
                    tool_name=request.short_name,
                )
                self.log.debug(block.debug_message)
                return PreToolUseHookResponse.block(block).to_dict()

        return PreToolUseHookResponse.noop().to_dict()


class AgentTickWatchdog:
    """Abort a single SDK tick that is looping without useful game progress."""

    def __init__(
        self,
        *,
        same_failure_limit: int | None = None,
        no_progress_timeout_s: float | None = None,
        clock=time.monotonic,
    ):
        runtime = _runtime_settings()
        resolved = BridgeRuntimeSettings(
            watchdog_same_failure_limit=(
                same_failure_limit
                if same_failure_limit is not None
                else runtime.watchdog_same_failure_limit
            ),
            watchdog_no_progress_timeout_s=(
                no_progress_timeout_s
                if no_progress_timeout_s is not None
                else runtime.watchdog_no_progress_timeout_s
            ),
        )
        self.same_failure_limit = resolved.watchdog_same_failure_limit
        self.no_progress_timeout_s = resolved.watchdog_no_progress_timeout_s
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
        classification: ToolResultClassification | str,
        text: str,
        *,
        indicates_progress: bool | None = None,
    ) -> None:
        tool_name = self._tool_names.get(str(tool_use_id), "") if tool_use_id else ""
        observation = WatchdogToolObservation.from_result(
            tool_use_id=tool_use_id,
            tool_name=tool_name,
            classification=classification,
            text=text,
            indicates_progress=indicates_progress,
        )
        if observation.is_ok:
            if (
                observation.is_mutating_tool
                and observation.indicates_mutating_progress()
            ):
                self.mark_progress()
            self._last_failure_signature = None
            self._same_failure_count = 0
            self.check_no_progress()
            return

        if observation.is_expected_miss:
            self.check_no_progress()
            return

        if observation.is_game_rejected:
            signature = observation.failure_signature()
            if signature == self._last_failure_signature:
                self._same_failure_count += 1
            else:
                self._last_failure_signature = signature
                self._same_failure_count = 1
            if (
                self.same_failure_limit > 0
                and self._same_failure_count >= self.same_failure_limit
            ):
                short_tool = observation.short_tool_name or "tool"
                raise AgentTickWatchdogAbort(
                    "repeated same game rejection "
                    f"({self._same_failure_count}x) from {short_tool}: "
                    f"{BridgeLogMessage.single_line(observation.text, limit=300)}"
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
) -> AgentMessageResult:
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
    return AgentMessageResult.reset()


def _disallowed_tools_for_env(env: dict[str, str]) -> list[str]:
    return RawLuaPolicy.from_env(env).disallowed_tools


def _runtime_settings(env: Any = None) -> BridgeRuntimeSettings:
    return BridgeRuntimeSettings.from_env(os.environ if env is None else env)


def _resolve_max_turns(value: Any = None) -> int:
    if value is None:
        return _runtime_settings().max_turns
    return BridgeRuntimeSettings(max_turns=value).max_turns


def _resolve_sdk_skills(value: Any = None) -> list[str] | str:
    return _sdk_skill_config(value).sdk_value


def _telemetry_relay_settings(args, env: Any = None) -> TelemetryRelaySettings:
    return TelemetryRelaySettings.from_sources(
        cli_url=getattr(args, "relay", None),
        cli_token=getattr(args, "relay_token", None),
        env=os.environ if env is None else env,
    )


def _claude_tools_for_sdk_skills(skills: list[str] | str) -> list[str]:
    return SdkSkillConfig.resolve(skills).claude_tools


def _setting_sources_for_sdk_skills(skills: list[str] | str) -> list[str] | None:
    return SdkSkillConfig.resolve(skills).setting_sources


def _sdk_skill_config(value: Any = None) -> SdkSkillConfig:
    return SdkSkillConfig.from_env(
        os.environ,
        value=value,
        default=DEFAULT_SDK_SKILLS,
    )


def _log_sdk_init(
    system_message: SdkSystemMessage,
    options: ClaudeAgentOptions,
    log,
) -> bool:
    if not system_message.is_loggable_init:
        return False
    log.info(
        "sdk init: cwd={} skill_tool={} configured_skills={} visible_skills={}",
        system_message.cwd,
        system_message.skill_tool_label,
        options.skills if options.skills is not None else "default",
        system_message.bounded_visible_skills(),
    )
    return True


def _is_skill_tool(block: ToolUseBlock) -> bool:
    return SdkToolUse.from_sdk_block(block).is_skill_tool


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
    return _runtime_settings().context_window_backoff_s


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
    settings = ProviderUsageLimitSettings.from_env(os.environ)
    return _set_usage_limit_cooldown_from_limit(
        agent_name,
        ProviderUsageLimit.from_text(
            text,
            now=now,
            default_utc_offset=settings.usage_limit_reset_utc_offset,
        ),
        log=log,
        now=now,
    )


def _set_usage_limit_cooldown_from_limit(
    agent_name: str,
    limit: ProviderUsageLimit | None,
    log=None,
    now: datetime | None = None,
) -> datetime | None:
    if limit is None:
        return None
    reset_at = limit.reset_at
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
    text = parse_response_model(reply).meaningful_anomaly_text()
    if text:
        append_event(
            agent_name,
            "discovery",
            BridgeLogMessage.single_line(text, limit=300),
        )


# Hard wall-clock cap on a single agent tick. The SDK's max_turns bounds tool
# turns, not a stalled TCP connection or a model response that never yields, so
# a tick is also wrapped in asyncio.wait_for. Override via BRIDGE_TICK_TIMEOUT_S.
_RUNTIME_SETTINGS = _runtime_settings()
_TICK_TIMEOUT_S = _RUNTIME_SETTINGS.tick_timeout_s

# A long tick is fine if the SDK keeps emitting messages, but a long silent gap
# after a tool result leaves the game looking dropped. Abort that invocation and
# let the bridge resume on the next autonomy tick.
_STREAM_IDLE_TIMEOUT_S = _RUNTIME_SETTINGS.stream_idle_timeout_s

# Abort a single active tick when it is making no useful game progress. This is
# deliberately separate from session reset: a stuck layout is not a dead SDK
# session.
_WATCHDOG_SAME_FAILURE_LIMIT = _RUNTIME_SETTINGS.watchdog_same_failure_limit
_WATCHDOG_NO_PROGRESS_TIMEOUT_S = _RUNTIME_SETTINGS.watchdog_no_progress_timeout_s


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
        if SdkStderrSignal.is_benign(text):
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
) -> AgentRunTranscript:
    text_parts: list[str] = []
    new_session_id: str | None = None
    context_window_limit = False
    usage_limit_seen = False
    watchdog = AgentTickWatchdog()

    async for msg in _query_with_idle_timeout(prompt=prompt, options=options):
        if isinstance(msg, AssistantMessage):
            assistant_message = SdkAssistantMessage.from_sdk_message(
                msg,
                text_block_type=TextBlock,
                tool_use_block_type=ToolUseBlock,
                thinking_block_type=ThinkingBlock,
            )
            if assistant_message.session_id:
                new_session_id = assistant_message.session_id
            for event in assistant_message.events:
                if event.is_text:
                    observation = SdkAssistantTextObservation.from_event(
                        event,
                        default_utc_offset=ProviderUsageLimitSettings.from_env(
                            os.environ,
                        ).usage_limit_reset_utc_offset,
                    )
                    text_parts.append(observation.text)
                    if observation.usage_limit:
                        usage_limit_seen = True
                        _set_usage_limit_cooldown_from_limit(
                            agent_name,
                            observation.usage_limit,
                            log,
                        )
                    elif observation.counts_as_watchdog_progress:
                        watchdog.observe_text()
                    log.info("text: {}", observation.text.strip())
                elif event.is_tool_use and event.tool_use is not None:
                    tool_use = event.tool_use
                    watchdog.record_tool_use(event.tool_use_id, tool_use.name)
                    display = tool_use.display_name
                    if tool_use.is_skill_tool:
                        log.info("skill: {}({})", display, tool_use.log_input_text)
                    else:
                        log.debug("tool: {}({})", display, tool_use.log_input_text)
                    emit_tool_call(telemetry, display, tool_use.tool_input, agent=telemetry_name)
                    if tool_use.is_broadcast_thought:
                        thought = tool_use.thought_message
                        if thought:
                            emit_chat(telemetry, "agent", thought, agent=telemetry_name)
                    if player_index > 0 and tool_use.should_send_tool_status:
                        try:
                            send_tool_status(rcon, player_index, agent_name, display)
                        except Exception as e:
                            log.debug("tool status update failed: {}", e)
                elif event.is_thinking:
                    log.debug("thinking: {}", event.text)
        elif isinstance(msg, UserMessage):
            tool_results = SdkUserToolResultMessage.from_sdk_message(
                msg,
                tool_result_block_type=ToolResultBlock,
                player_marker=_PLAYER_MESSAGES_MARKER,
            )
            for result in tool_results.results:
                observation = result.observation()
                classification = _log_tool_result_record(
                    agent_name,
                    log,
                    observation.log_record,
                )
                watchdog.observe_tool_result(
                    observation.tool_use_id,
                    classification,
                    observation.text,
                    indicates_progress=observation.indicates_progress,
                )
                if observation.player_message_text:
                    log.info("player_messages: {}", observation.player_message_text)
        elif isinstance(msg, ResultMessage):
            result_message = SdkResultMessage.from_sdk_message(msg)
            observation = result_message.observation(
                default_utc_offset=ProviderUsageLimitSettings.from_env(
                    os.environ,
                ).usage_limit_reset_utc_offset,
            )
            new_session_id = observation.session_id or new_session_id
            if (
                observation.has_transcript_text
                and observation.transcript_text not in text_parts
            ):
                text_parts.append(observation.transcript_text)
            if observation.is_error:
                if observation.context_window_limit:
                    context_window_limit = True
                    log.warning(
                        "result sdk_context_window: {}; clearing SDK session before next attempt",
                        observation.error_detail,
                    )
                elif _set_usage_limit_cooldown_from_limit(
                    agent_name,
                    observation.usage_limit,
                    log,
                ):
                    usage_limit_seen = True
                elif observation.failure_classification:
                    log.warning(
                        "result {}: {}",
                        observation.failure_classification.value,
                        observation.error_detail,
                    )
                    append_event(
                        agent_name,
                        "failure",
                        observation.failure_journal_text,
                    )
            if observation.has_cost:
                log.info(
                    "done: ${:.4f} | {} turns | {:.1f}s",
                    observation.total_cost_usd,
                    observation.num_turns,
                    observation.duration_s,
                )
                if telemetry:
                    telemetry.emit(TelemetryEvent.compute_cost(
                        observation.compute_cost_payload,
                        agent=telemetry_name,
                    ))
        elif isinstance(msg, SystemMessage):
            system_message = SdkSystemMessage.from_sdk_message(msg)
            if not _log_sdk_init(system_message, options, log):
                if system_message.should_log:
                    log.debug("system: {}", msg)
        else:
            log.debug("stream event: {}", msg)

    return AgentRunTranscript.from_parts(
        text_parts=text_parts,
        session_id=new_session_id,
        context_window_limit=context_window_limit,
        usage_limit_seen=usage_limit_seen,
    )


def _finalize_reply(reply: str, agent_name: str) -> str:
    """Persist any <ledger> trailer the agent emitted, strip it from the
    human-visible reply, and fall back to a placeholder if the reply was ONLY a
    ledger block (so the bridge never logs/sends a blank message). This is the
    tested seam for the ledger persist + empty-reply guard."""
    ledger_update = parse_ledger_trailer_model(reply)
    apply_ledger_update_model(agent_name, reply)
    apply_reflection_update_model(agent_name, reply)
    apply_learning_update(agent_name, reply)
    if ledger_update and ledger_update.progress:
        append_event(
            agent_name,
            "progress",
            ledger_update.progress,
            signal=ledger_update.signal,
        )
    _record_anomaly(reply, agent_name)
    reply = strip_ledger_trailer(reply)
    reply = strip_reflection_trailer(reply)
    reply = strip_learning_trailers(reply)
    reply = strip_skill_trailer(reply)
    if not reply.strip():
        return "(action complete)"
    return reply


def _load_live_state_for_agent(
    rcon: RCONClient,
    agent_name: str,
    log: Any = logger,
) -> LiveState:
    """Best-effort live state for hook-time automation guards."""
    try:
        agent = lua_long_string(agent_name)
        out = rcon.execute(RconRemoteCall.command(
            "live_state_result",
            agent,
        ))
        try:
            return LiveState.from_rcon_response(out)
        except BridgeValidationError:
            return LiveState.from_line(out)
    except Exception as exc:
        log.debug("live-state lookup failed: {}", exc)
        return LiveState()


def handle_message_model(
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
) -> AgentMessageResult:
    """Pipe a message through the Claude SDK. Returns a typed session result.
    agent_name: registered agent name (for RCON/mod).
    telemetry_name: display name for telemetry/logs (defaults to agent_name).
    response_to: if set, send response to this tab instead of agent_name (group chat)."""
    invocation = AgentInvocationConfig.from_sources(
        system_prompt=system_prompt,
        agent_name=agent_name,
        telemetry_name=telemetry_name,
        response_to=response_to,
        session_id=session_id,
        model=model,
        max_turns=max_turns,
        sdk_skills=sdk_skills,
        read_only_tools=read_only_tools,
        default_sdk_skills=DEFAULT_SDK_SKILLS,
        env=os.environ,
    )
    tname = invocation.telemetry_label
    rcon_target = invocation.rcon_target
    log = logger.bind(agent=tname)
    log.info(
        "spawning claude sdk [model={}]{}",
        invocation.model or "default",
        invocation.resume_tag,
    )

    env = os.environ.copy()
    env.pop("CLAUDECODE", None)
    mutating_tool_gate = MutatingToolBatchGate(log)
    read_only_tool_gate = PlannerReadOnlyToolGate(
        log,
        enabled=invocation.read_only_tools,
    )
    sdk_options_spec = invocation.to_sdk_options_spec(
        mcp_servers=mcp_config,
        env=env,
        project_root=_PROJECT_ROOT,
    )
    factorio_skill_gate = FactorioSkillGate(
        log,
        required=invocation.skill_config.requires_factorio_control,
    )
    factorio_schema_gate = FactorioToolSchemaGate(log)
    manual_automation_gate = ManualAutomationDriftGate(
        log,
        agent_name=invocation.agent_name,
        live_state_loader=lambda agent_name: _load_live_state_for_agent(
            rcon,
            agent_name,
            log,
        ),
    )
    options = ClaudeAgentOptions(
        system_prompt=sdk_options_spec.system_prompt,
        model=sdk_options_spec.model,
        max_turns=sdk_options_spec.max_turns,
        mcp_servers=sdk_options_spec.mcp_servers,
        strict_mcp_config=sdk_options_spec.strict_mcp_config,
        tools=sdk_options_spec.tools,
        disallowed_tools=sdk_options_spec.disallowed_tools,
        permission_mode=sdk_options_spec.permission_mode,
        resume=sdk_options_spec.resume,
        setting_sources=sdk_options_spec.setting_sources,
        cwd=Path(sdk_options_spec.cwd),
        skills=sdk_options_spec.skills,
        env=env,
        hooks={
            "PreToolUse": [
                HookMatcher(hooks=[
                    read_only_tool_gate.hook,
                    factorio_skill_gate.hook,
                    factorio_schema_gate.hook,
                    manual_automation_gate.hook,
                    mutating_tool_gate.hook,
                ])
            ],
        },
        stderr=_stderr_callback(log),
    )
    try:
        run = asyncio.run(
            asyncio.wait_for(
                _run_agent(
                    prompt,
                    options,
                    invocation.agent_name,
                    telemetry,
                    tname,
                    rcon,
                    player_index,
                    log,
                ),
                timeout=_TICK_TIMEOUT_S,
            )
        )
        if run.context_window_limit:
            return _handle_context_window_limit(
                agent_name=invocation.agent_name,
                session_id=invocation.session_id,
                log=log,
                telemetry=telemetry,
                telemetry_name=tname,
                rcon=rcon,
                player_index=player_index,
                rcon_target=rcon_target,
            )

        cooldown_until = _get_usage_limit_cooldown(invocation.agent_name)
        if cooldown_until and run.usage_limit_seen:
            run = run.with_text_parts([_usage_limit_message(cooldown_until)])
        text_parts = list(run.text_parts)
        new_session_id = run.session_id
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
            invocation.agent_name, "failure",
            BridgeLogMessage.single_line(
                f"stream idle timeout after {_STREAM_IDLE_TIMEOUT_S:.0f}s",
                limit=300,
            ),
        )
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return AgentMessageResult.keep_session(invocation.session_id)
    except AgentTickWatchdogAbort as e:
        reason = BridgeLogMessage.single_line(str(e), limit=300)
        error_msg = f"Error: watchdog aborted stuck tick: {reason}"
        log.warning("watchdog aborted stuck tick: {}", reason)
        append_event(invocation.agent_name, "failure", f"watchdog_abort: {reason}")
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return AgentMessageResult.keep_session(invocation.session_id)
    except (asyncio.TimeoutError, TimeoutError):
        error_msg = f"Error: agent tick exceeded {_TICK_TIMEOUT_S:.0f}s and was aborted"
        log.error("agent tick timed out after {:.0f}s; aborting", _TICK_TIMEOUT_S)
        append_event(
            invocation.agent_name, "failure",
            BridgeLogMessage.single_line(
                f"tick timeout after {_TICK_TIMEOUT_S:.0f}s",
                limit=300,
            ),
        )
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return AgentMessageResult.keep_session(invocation.session_id)
    except FileNotFoundError:
        error_msg = "Error: claude CLI not installed"
        log.error("'claude' CLI not found. Install: npm install -g @anthropic-ai/claude-code")
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return AgentMessageResult.keep_session(invocation.session_id)
    except Exception as e:
        exception_signal = AgentInvocationExceptionSignal.from_exception(
            e,
            default_utc_offset=ProviderUsageLimitSettings.from_env(
                os.environ,
            ).usage_limit_reset_utc_offset,
        )
        error_msg = exception_signal.error_message
        cooldown_until = (
            _set_usage_limit_cooldown_from_limit(
                invocation.agent_name,
                exception_signal.usage_limit,
                log,
            )
            or _get_usage_limit_cooldown(invocation.agent_name)
        )
        if exception_signal.context_window_limit:
            return _handle_context_window_limit(
                agent_name=invocation.agent_name,
                session_id=invocation.session_id,
                log=log,
                telemetry=telemetry,
                telemetry_name=tname,
                rcon=rcon,
                player_index=player_index,
                rcon_target=rcon_target,
            )
        if exception_signal.terminal_result_echo:
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
            append_event(invocation.agent_name, "failure", exception_signal.short_text)
        emit_error(telemetry, error_msg, agent=tname)
        if player_index > 0:
            send_response(rcon, player_index, rcon_target, error_msg)
            set_status(rcon, player_index, "[color=0.4,0.8,0.4]Ready[/color]")
        return AgentMessageResult.keep_session(invocation.session_id)

    # Send response — join all text parts so intermediate messages aren't lost
    reply = "\n\n".join(text_parts) if text_parts else "(action complete)"
    reply = sanitize_response(reply)
    reply = _finalize_reply(reply, invocation.agent_name)

    log.info("reply: {}", reply)
    parsed_reply = parse_response_model(reply)
    emit_chat(telemetry, "agent", reply, agent=tname, sections=parsed_reply)
    # For group chat, prefix reply with agent name so reader knows who said what
    if invocation.response_to:
        reply = f"[color=1,0.6,0.2]{tname}:[/color] {reply}"
    if player_index > 0:
        # A dropped RCON connection on this final send must not bubble out and
        # kill the agent thread (loguru no longer tees raw thread tracebacks).
        try:
            send_response(rcon, player_index, rcon_target, reply)
        except Exception as e:
            log.exception("failed to send reply to RCON")
            append_event(
                invocation.agent_name,
                "failure",
                BridgeLogMessage.single_line(f"rcon send failed: {e}", limit=300),
            )

    return AgentMessageResult.keep_session(new_session_id or invocation.session_id)


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
    """Legacy wrapper returning a session id, None, or SESSION_RESET."""
    result = handle_message_model(
        prompt,
        mcp_config,
        system_prompt,
        session_id,
        rcon,
        player_index,
        telemetry,
        agent_name=agent_name,
        telemetry_name=telemetry_name,
        response_to=response_to,
        model=model,
        max_turns=max_turns,
        sdk_skills=sdk_skills,
        read_only_tools=read_only_tools,
    )
    return result.to_legacy_session_value(SESSION_RESET)


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

    relay_settings = _telemetry_relay_settings(args)
    if relay_settings.enabled:
        if not relay_settings.ready:
            logger.warning("relay URL set but no RELAY_TOKEN")
        else:
            relay_pusher = RelayPusher(
                relay_settings.relay_url or "",
                relay_settings.relay_token or "",
            )
            logger.info("Relay: {}", relay_settings.relay_url)

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

def _agent_sort_key(agent: AgentProfile | dict) -> tuple:
    """Sort agents by planet progression order, then name."""
    return AgentProfile.coerce(agent).sort_key(PLANET_ORDER)


def discover_agents(group: str | None = None, names: list[str] | None = None) -> list[AgentProfile]:
    """Load agent profiles by group name or explicit name list."""
    if names:
        return [load_agent(n) for n in names]
    agents_dir = _BRIDGE_DIR / "agents"
    profiles = []
    for f in agents_dir.glob("*.json"):
        try:
            profile = AgentProfile.from_file_text(f.read_text())
        except (OSError, ValueError):
            continue
        if profile.group == group:
            profiles.append(load_agent(profile.name))
    if not profiles:
        raise ValueError(f"No agents found with group '{group}'")
    profiles.sort(key=_agent_sort_key)
    return profiles


class AgentThread:
    """Manages one agent's Claude SDK sessions in a dedicated thread."""

    def __init__(self, agent: AgentProfile | dict,
                 mcp_config: McpServersConfig | str | Path | None, rcon,
                 telemetry: 'Telemetry | None', model: str | None,
                 heartbeat_interval: float = 0.0,
                 planner_interval: int = 5,
                 autonomy_requires_player: bool = True,
                 max_turns: int | None = None,
                 sdk_skills: list[str] | str | None = None):
        runtime = AgentRuntimeConfig.from_sources(
            agent,
            cli_model=model,
            cli_max_turns=max_turns,
            cli_sdk_skills=sdk_skills,
            default_sdk_skills=DEFAULT_SDK_SKILLS,
            heartbeat_interval=heartbeat_interval,
            planner_interval=planner_interval,
            autonomy_requires_player=autonomy_requires_player,
            env=os.environ,
        )
        profile = runtime.profile
        self.runtime = runtime
        self.profile = profile
        self.agent = profile
        self.agent_name = runtime.agent_name
        self.system_prompt = runtime.system_prompt
        # Tiered models: default to the fast "haiku" tier (.env -> glm-5-turbo)
        # for the frequent execution/reflection/chat ticks; planner ticks
        # override up to "sonnet" (.env -> glm-5.2) via _planner_model below.
        self.model = runtime.model
        self.max_turns = runtime.max_turns
        self.sdk_skills = runtime.sdk_skills
        self.telemetry_name = runtime.telemetry_name
        self.log = logger.bind(agent=self.telemetry_name)
        self.mcp_config = mcp_config
        self.rcon = rcon
        self.telemetry = telemetry
        # Autonomy: when no human message arrives within heartbeat_interval
        # seconds, the agent prompts itself to keep playing. <= 0 disables
        # autonomy (agent acts only in response to chat). A profile may
        # override this default.
        self.heartbeat_interval = runtime.heartbeat_interval
        self._planner_interval = runtime.planner_interval
        # A bridge restart or `just resume` keeps the Factorio save and ledger
        # but clears the SDK session. Reassess once before executing old plan
        # steps so live structures in the save can supersede stale progress.
        self._exec_ticks_since_plan = self._planner_interval
        self._reflect_interval = runtime.reflect_interval
        self._planner_model = runtime.planner_model
        # When True, autonomy ticks only fire while a human is connected to the
        # server, so the agent waits to "do its own thing" until you join (and
        # goes back to idle if you leave). Chat is always processed regardless.
        self.autonomy_requires_player = runtime.autonomy_requires_player
        self.session_id = load_session(self.agent_name)
        self.inbox: queue.Queue[BridgeInputMessage | AutonomyTickMessage] = queue.Queue()
        self._thread = threading.Thread(
            target=self._run, name=f"agent-{self.agent_name}", daemon=True,
        )

    def start(self):
        self._thread.start()

    def enqueue(self, msg: BridgeInputMessage | dict):
        message = (
            msg
            if isinstance(msg, BridgeInputMessage)
            else BridgeInputMessage.from_mapping(msg)
        )
        if message is not None:
            self.inbox.put(message)

    def _human_connected(self) -> bool:
        """True if at least one human player is connected.

        AI agents are orphan character entities, so the mod-side remote counts
        only real client connections. On any RCON error, return False so we
        don't burn autonomy turns when we can't confirm a human is present.
        """
        try:
            out = self.rcon.execute(RconRemoteCall.command(
                "connected_player_count_result",
            ))
            return ConnectedPlayerCountResult.from_rcon_response(
                out,
            ).has_connected_players
        except Exception as e:
            self.log.debug("human-connected check failed: {}", e)
            return False

    def _live_state(self) -> LiveState:
        """Best-effort typed live state for autonomy ticks."""
        try:
            agent = lua_long_string(self.agent_name)
            out = self.rcon.execute(RconRemoteCall.command(
                "live_state_result",
                agent,
            ))
            try:
                return LiveState.from_rcon_response(out)
            except BridgeValidationError:
                return LiveState.from_line(out)
        except Exception as e:
            getattr(self, "log", logger).debug("live-state lookup failed: {}", e)
            return LiveState()

    def _live_state_line(self) -> str:
        """Best-effort one-line live state for autonomy ticks."""
        return self._live_state().to_line()

    def _compose_autonomy_prompt(self) -> str:
        """Assemble the autonomy-tick prompt for the current plan/execute mode."""
        return self._autonomy_tick().message

    def _autonomy_tick(self) -> AutonomyTickMessage:
        """Choose plan/execute mode, update cadence state, and build the message."""
        ledger = load_ledger_model(self.agent_name)
        journal_window = load_events_model(self.agent_name, 20)
        memory = render_memory(journal_window, load_reflection_model(self.agent_name))
        learned_text = render_accepted_learning(load_accepted_learning_model())
        live_state = self._live_state()
        live_completion = objective_completion_evidence(ledger, live_state)
        live_completion_reason = live_completion.reason
        reflect_due = should_reflect(
            count_events(self.agent_name), getattr(self, "_reflect_interval", 16),
        )
        decision = choose_autonomy_decision(
            ledger,
            self._exec_ticks_since_plan,
            self._planner_interval,
            journal_window=journal_window,
            live_state=live_state,
            live_completion_evidence=live_completion,
            reflect_due=reflect_due,
        )
        self._exec_ticks_since_plan = decision.next_exec_ticks_since_plan(
            self._exec_ticks_since_plan,
        )
        if decision.reason == AutonomyDecisionReason.REPEATED_PLAN_PROGRESS:
            append_event(
                self.agent_name,
                "progress",
                "scheduler override: repeated read-only planning produced no "
                "mutation; forcing execution tick",
            )
        include_reflection = decision.is_plan and reflect_due
        message = build_autonomy_prompt_model(
            AutonomyPromptInput(
                mode=decision.mode,
                ledger=ledger,
                live_state=live_state,
                memory_text=memory,
                learned_text=learned_text,
                live_completion_reason=live_completion_reason,
                planner_advisory=planner_advisory_for_decision(decision.reason),
            ),
        )
        if decision.is_plan:
            message = "\n\n".join([message, learning_proposal_prompt()])
        if include_reflection:
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

        return AutonomyTickMessage.create(
            message,
            read_only_tools=decision.read_only_tools,
            model=self._planner_model,
        )

    def _next_message(self) -> BridgeInputMessage | AutonomyTickMessage:
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
        raw_msg = self._next_message()
        if isinstance(raw_msg, AutonomyTickMessage):
            msg = raw_msg.to_bridge_input()
        else:
            msg = raw_msg
        if msg.autonomy:
            self.log.info("{} autonomy tick", self.agent_name)
        player_index = msg.player_index
        player_name = msg.player_name
        message = msg.message
        response_to = msg.response_to  # Group chat routing

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

        message_result = handle_message_model(
            message, self.mcp_config, self.system_prompt, self.session_id,
            self.rcon, player_index, self.telemetry,
            agent_name=self.agent_name, telemetry_name=self.telemetry_name,
            response_to=response_to, model=msg.model or self.model,
            max_turns=self.max_turns, sdk_skills=self.sdk_skills,
            read_only_tools=msg.read_only_tools,
        )
        if message_result.reset_session:
            self.session_id = None
            return
        if message_result.session_id:
            self.session_id = message_result.session_id
            save_session(self.agent_name, self.session_id)


def main_multi(args, agent_profiles: list[AgentProfile | dict]):
    """Multi-agent mode: one thread per agent, shared watcher."""
    log = logger.bind(agent="system")
    agent_profiles = [AgentProfile.coerce(agent) for agent in agent_profiles]
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
            label = agent.registration_label
            register_agent(rcon, agent.name, label=label)
            log.info("Registered agent: {} [{}]", agent.name, label)
        unregister_agent(rcon, "default")
    else:
        log.warning("claude-interface mod not detected")

    # Create planet surfaces if requested (for fresh worlds)
    if args.setup_surfaces:
        planets = list({agent.planet_name for agent in agent_profiles} - {"nauvis"})
        if planets:
            log.info("Setting up planet surfaces")
            results = setup_surfaces_model(rcon, sorted(planets))
            for planet, status in results.items():
                log.info("{}: {}", planet, status)

    # Pre-place characters on correct planets (offset to avoid overlapping with player)
    log.info("Pre-placing characters")
    for i, agent in enumerate(agent_profiles):
        planet = agent.planet_name
        result = pre_place_character_model(rcon, agent.name, planet, spawn_offset=i)
        log.info("{} -> {}: {}", result.agent_name, result.planet, result.status)

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
                args.rcon_password, agent_id=agent.name,
            )
        at = AgentThread(agent, mcp_config, rcon, telemetry, args.model,
                         heartbeat_interval=args.heartbeat_interval,
                         planner_interval=args.planner_interval,
                         autonomy_requires_player=args.autonomy_requires_player,
                         max_turns=args.max_turns,
                         sdk_skills=args.sdk_skills)
        agents[agent.name] = at

    # Resolve paths and start watcher
    script_output = Path(args.script_output) if args.script_output else find_script_output()
    input_file = script_output / "claude-chat" / "input.jsonl"
    input_file.parent.mkdir(parents=True, exist_ok=True)
    watcher = InputWatcher(input_file)

    # Banner
    agent_names = ", ".join(agent.name for agent in agent_profiles)
    log.info("Factorio companion - multi-agent")
    log.info("Agents: {}", agent_names)
    log.info("RCON: {}:{}", args.rcon_host, args.rcon_port)
    log.info("Input: {}", input_file)
    resolved_skill_sets = {
        agent_name: at.sdk_skills if at.sdk_skills else "disabled"
        for agent_name, at in agents.items()
    }
    log.info("SDK skills: {}", resolved_skill_sets)
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
            for msg in watcher.poll_model():
                target = msg.target_agent
                if target == "all":
                    # Fan out to all agents with staggered delivery
                    for i, at in enumerate(agents.values()):
                        at.enqueue(msg.model_copy(update={"response_to": "all"}))
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

    mod_info = FactorioModInfo.from_file_text((src / "info.json").read_text())
    logger.info("Synced claude-interface v{} ({} files)", mod_info.version_label, count)
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
        names = AgentNameSelection.from_cli_arg(args.agents).filter_or_none
        group = args.group or "doug-squad"
        profiles = discover_agents(group=group, names=names)
        if args.scale:
            profiles = profiles[:args.scale]
        main_multi(args, profiles)
        return

    # Single-agent mode
    profile = load_agent(args.agent or "default")
    runtime = AgentRuntimeConfig.from_sources(
        profile,
        cli_model=args.model,
        cli_max_turns=args.max_turns,
        cli_sdk_skills=args.sdk_skills,
        default_sdk_skills=DEFAULT_SDK_SKILLS,
        env=os.environ,
    )
    agent_name = runtime.agent_name
    system_prompt = runtime.system_prompt
    model = runtime.model
    max_turns = runtime.max_turns
    sdk_skills = runtime.sdk_skills
    telemetry_name = runtime.telemetry_name
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
    planet = runtime.planet_name
    result = pre_place_character_model(rcon, agent_name, planet, spawn_offset=0)
    log.info("Character: {} -> {}: {}", result.agent_name, result.planet, result.status)

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

            for msg in watcher.poll_model():
                target = msg.target_agent
                if target != agent_name:
                    continue

                player_index = msg.player_index
                player_name = msg.player_name
                message = msg.message

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

                message_result = handle_message_model(
                    message, mcp_config, system_prompt, session_id,
                    rcon, player_index, telemetry,
                    agent_name=agent_name, telemetry_name=telemetry_name,
                    model=model, max_turns=max_turns,
                    sdk_skills=sdk_skills,
                    read_only_tools=msg.read_only_tools,
                )
                if message_result.reset_session:
                    session_id = None
                    continue
                if message_result.session_id:
                    session_id = message_result.session_id
                    save_session(agent_name, session_id)

    except (KeyboardInterrupt, SystemExit):
        log.info("Shutting down...")
    finally:
        rcon.close()
        log.info("Done")


if __name__ == "__main__":
    main()
