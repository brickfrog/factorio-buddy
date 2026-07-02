#!/usr/bin/env python3
"""
Runtime smoke checks for the real factorioctl/claude-interface surface.

This runner is intentionally separate from Doug/the bridge. It is an
operator-facing disposable-save test harness, so it may seed the smoke
character with raw Lua through the CLI while keeping raw Lua disabled for MCP
agents by default.
"""

from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import sys
import time
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
MOD_SOURCE = REPO_ROOT / "companion" / "mod" / "claude-interface"
DEFAULT_SYNCED_MOD = Path.home() / ".factorio" / "mods" / "claude-interface"
DEFAULT_CLI = REPO_ROOT / "target" / "release" / "factorioctl"
DEFAULT_MCP = REPO_ROOT / "target" / "release" / "mcp"
_JSON_MISSING = object()


class SmokeError(Exception):
    def __init__(self, message: str, classification: str = "smoke-runner"):
        super().__init__(message)
        self.classification = classification


@dataclass
class StepResult:
    name: str
    tool_call: str
    classification: str
    result: str


@dataclass(frozen=True)
class JsonRpcResponse:
    id: int | str | None = None
    result: Any = None
    error: Any = None


@dataclass(frozen=True)
class McpContentItem:
    type: str = ""
    text: str = ""


@dataclass(frozen=True)
class McpToolResult:
    content: list[McpContentItem] = field(default_factory=list)


@dataclass(frozen=True)
class SmokeResultPayload:
    success: bool | None = None
    error: str | None = None


@dataclass(frozen=True)
class SmokeBlocker:
    type: str = ""
    name: str = ""
    bounding_box: Any = None


@dataclass(frozen=True)
class PlacementCheckPayload:
    allowed: bool | None = None


@dataclass(frozen=True)
class PlacedEntityPayload:
    unit_number: int | None = None


@dataclass(frozen=True)
class PlacementFailurePayload:
    blockers: list[SmokeBlocker] = field(default_factory=list)
    alternate_belt_placements: Any = None
    candidate_alternate_path: Any = None
    recommended_action: str = ""
    rotate_entity: Any = None
    unit_number: int | None = None


@dataclass(frozen=True)
class RotateEntityPayload:
    success: bool = False
    direction: int | None = None


@dataclass(frozen=True)
class SmokeSuggestedTool:
    tool: str = ""


@dataclass(frozen=True)
class SmokeToolStep:
    tool: str = ""
    tool_args: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class SteamNoPlantDiagnostic:
    status: str = ""
    next_action: str = ""


@dataclass(frozen=True)
class SteamNoPlantRepair:
    blockers: list[SmokeBlocker] = field(default_factory=list)
    suggested_next_tool: SmokeSuggestedTool | None = None


@dataclass(frozen=True)
class PowerExtensionPlan:
    dry_run: bool = False
    steps: list[SmokeToolStep] = field(default_factory=list)
    missing_items: list[Any] = field(default_factory=list)


@dataclass(frozen=True)
class LabFeedPlan:
    dry_run: bool = False
    success: bool | None = None
    ready: bool | None = None
    inserted: int = 0
    lab_after: int = 0
    steps: list[SmokeToolStep] = field(default_factory=list)
    blockers: list[SmokeBlocker] = field(default_factory=list)


@dataclass(frozen=True)
class SmokePosition:
    x: float
    y: float


@dataclass(frozen=True)
class SteamPlannerResult:
    placement_success: bool = False
    target: SmokePosition | None = None
    blockers: list[SmokeBlocker] = field(default_factory=list)
    existing_plant: Any = None


@dataclass(frozen=True)
class ExistingPlantSummary:
    offshore_pumps: int = 0
    boilers: int = 0
    steam_engines: int = 0


@dataclass(frozen=True)
class ExistingPlantDiagnostic:
    has_existing_plant: bool = False
    summary: ExistingPlantSummary = field(default_factory=ExistingPlantSummary)
    issues: list[SmokeBlocker] = field(default_factory=list)


@dataclass(frozen=True)
class SteamRepairPlan:
    dry_run: bool = False
    repair_steps: list[SmokeToolStep] = field(default_factory=list)
    missing_items: list[Any] = field(default_factory=list)


@dataclass(frozen=True)
class SmokePlacement:
    position: SmokePosition


@dataclass(frozen=True)
class FindPlacementsPayload:
    placements: list[SmokePlacement] = field(default_factory=list)


@dataclass(frozen=True)
class NearestResourcePayload:
    center_x: float
    center_y: float


@dataclass(frozen=True)
class DrillOutputDiagnostics:
    belt_tile: SmokePosition | None = None
    belt_direction: int | str | None = 8


@dataclass(frozen=True)
class DrillPlacement:
    output: DrillOutputDiagnostics | None = None
    output_buildable: bool = False
    output_clear: bool = False
    resource_tiles: int = 0


@dataclass(frozen=True)
class DrillPlacementsPayload:
    placements: list[DrillPlacement] = field(default_factory=list)


@dataclass(frozen=True)
class EdgeMinerPlan:
    dry_run: bool = False
    success: bool | None = None
    ready: bool | None = None
    selected: DrillPlacement | None = None
    steps: list[SmokeToolStep] = field(default_factory=list)
    missing_items: list[Any] = field(default_factory=list)


@dataclass(frozen=True)
class DirectSmelterSelection:
    input_inserter: Any = None


@dataclass(frozen=True)
class DirectSmelterPlan:
    dry_run: bool = False
    success: bool | None = None
    ready: bool | None = None
    selected: DirectSmelterSelection | None = None
    steps: list[SmokeToolStep] = field(default_factory=list)
    missing_items: list[Any] = field(default_factory=list)
    verify_step: SmokeToolStep | None = None


@dataclass(frozen=True)
class MineAtPayload:
    mined_count: int | None = None


@dataclass(frozen=True)
class LabFixturePayload:
    lab_unit_number: int
    chest_unit_number: int


@dataclass(frozen=True)
class SteamPlaceArgs:
    entity_name: str
    x: float
    y: float
    direction: str = "north"


@dataclass(frozen=True)
class SteamPlannedEntity:
    place_args: SteamPlaceArgs


@dataclass(frozen=True)
class SteamPlan:
    offshore_pump: SteamPlannedEntity
    boiler: SteamPlannedEntity
    steam_engine: SteamPlannedEntity
    pipes: list[SteamPlannedEntity] = field(default_factory=list)


@dataclass(frozen=True)
class SteamPlanPayload:
    plan: SteamPlan


@lru_cache(maxsize=1)
def _pydantic_runtime() -> tuple[Any, type[Exception]]:
    try:
        from pydantic import TypeAdapter, ValidationError
    except ModuleNotFoundError as exc:
        raise SmokeError(
            "runtime smoke JSON validation requires pydantic; run tests/smoke.sh "
            "or invoke this script with companion/.venv/bin/python after "
            "`cd companion && just install`",
            "smoke-runner",
        ) from exc
    return TypeAdapter, ValidationError


@lru_cache(maxsize=None)
def _adapter(schema: Any) -> Any:
    TypeAdapter, _ = _pydantic_runtime()
    return TypeAdapter(schema)


def _json_value_or_missing(text: Any) -> Any:
    _, ValidationError = _pydantic_runtime()
    try:
        return _adapter(Any).validate_json(str(text if text is not None else ""))
    except (TypeError, ValueError, ValidationError):
        return _JSON_MISSING


def _json_value(
    text: Any,
    *,
    classification: str,
    context: str,
) -> Any:
    parsed = _json_value_or_missing(text)
    if parsed is _JSON_MISSING:
        raise SmokeError(
            f"{context}: expected JSON value: {_clip(str(text if text is not None else ''))}",
            classification,
        )
    return parsed


def _validated(
    schema: Any,
    value: Any,
    *,
    classification: str,
    context: str,
) -> Any:
    _, ValidationError = _pydantic_runtime()
    try:
        return _adapter(schema).validate_python(value)
    except (TypeError, ValueError, ValidationError) as exc:
        raise SmokeError(
            f"{context}: payload shape mismatch: {value}",
            classification,
        ) from exc


def _json_dataclass(
    schema: Any,
    text: Any,
    *,
    classification: str,
    context: str,
) -> Any:
    value = _json_value(text, classification=classification, context=context)
    return _validated(schema, value, classification=classification, context=context)


def _payload(schema: Any, value: Any, *, context: str) -> Any:
    return _validated(schema, value, classification="mod-lua", context=context)


def _has_blocker(blockers: list[SmokeBlocker], *types: str) -> bool:
    expected = set(types)
    return any(blocker.type in expected for blocker in blockers)


def _has_named_blocker_with_footprint(blockers: list[SmokeBlocker], name: str) -> bool:
    return any(blocker.name == name and blocker.bounding_box for blocker in blockers)


def _has_entity_step(steps: list[SmokeToolStep], *, tool: str, entity_name: str) -> bool:
    return any(
        step.tool == tool and step.tool_args.get("entity_name") == entity_name
        for step in steps
    )


def _has_execute_lab_feed_step(steps: list[SmokeToolStep]) -> bool:
    return any(
        step.tool == "feed_lab_from_inventory"
        and step.tool_args.get("dry_run") is False
        for step in steps
    )


def _has_tool_step(steps: list[SmokeToolStep], tool: str) -> bool:
    return any(step.tool == tool for step in steps)


def _target_args(target: SmokePosition) -> dict[str, int]:
    return {
        "x": int(round(float(target.x))),
        "y": int(round(float(target.y))),
    }


def _clip(text: str, limit: int = 4000) -> str:
    text = text.strip()
    if len(text) <= limit:
        return text
    return text[:limit] + "\n...<truncated>"


def classify_failure(text: str, returncode: int = 0) -> str:
    lowered = text.lower()
    if '"expected_miss":true' in lowered.replace(" ", ""):
        return "expected-miss"
    if any(
        needle in lowered
        for needle in [
            "no electric poles found",
            "no steam power entities",
            "no offshore pump",
            "no boiler",
            "no steam engine",
        ]
    ):
        return "expected-miss"
    if any(
        needle in lowered
        for needle in [
            "failed to connect",
            "connection refused",
            "connection reset",
            "connection timed out",
            "rcon",
            "server did not start",
        ]
    ):
        return "rcon"
    if any(
        needle in lowered
        for needle in [
            "failed to deserialize",
            "invalid type:",
            "invalid params",
            "missing field",
            "json-rpc",
            "jsonrpc",
        ]
    ):
        return "bridge"
    if any(
        needle in lowered
        for needle in [
            "stack traceback",
            "attempt to",
            "sync_or_restart_mod",
            "fix_get_power_status",
            "remote interface",
            "remote.call",
        ]
    ):
        return "mod-lua"
    if any(
        needle in lowered
        for needle in [
            "cannot place",
            "no items of that type",
            "insufficient materials",
            "entity has no such inventory",
            "factorio cannot place",
            "teleport blocked",
            "create_entity returned nil after can_place_entity succeeded",
        ]
    ):
        return "factorio-game-rejection"
    if returncode != 0:
        return "rust-wrapper"
    return "ok"


def ensure_disposable_port(port: int, allow_main_port: bool) -> None:
    if port == 27015 and not allow_main_port:
        raise SmokeError(
            "Refusing to smoke-test against RCON port 27015. "
            "Use the isolated test server on 27016, or pass --allow-main-port "
            "only if the current save is disposable.",
            "smoke-runner",
        )


def verify_synced_mod(source_dir: Path, synced_dir: Path) -> StepResult:
    if not source_dir.is_dir():
        raise SmokeError(f"mod source does not exist: {source_dir}", "mod-sync")
    if not synced_dir.is_dir():
        raise SmokeError(f"synced mod copy does not exist: {synced_dir}", "mod-sync")

    checked: list[str] = []
    for source_file in sorted(path for path in source_dir.rglob("*") if path.is_file()):
        rel = source_file.relative_to(source_dir)
        synced_file = synced_dir / rel
        if not synced_file.is_file():
            raise SmokeError(f"synced mod missing {rel}", "mod-sync")
        if source_file.read_bytes() != synced_file.read_bytes():
            raise SmokeError(f"synced mod differs for {rel}", "mod-sync")
        checked.append(str(rel))

    return StepResult(
        name="synced mod matches repo",
        tool_call=f"compare {source_dir} -> {synced_dir}",
        classification="ok",
        result=f"{len(checked)} files match",
    )


class CliRunner:
    def __init__(
        self,
        cli: Path,
        host: str,
        port: int,
        password: str,
        agent_id: str,
        timeout_s: float,
    ) -> None:
        self.cli = cli
        self.host = host
        self.port = port
        self.password = password
        self.agent_id = agent_id
        self.timeout_s = timeout_s

    def command(self, args: list[str], json_output: bool = True) -> list[str]:
        cmd = [
            str(self.cli),
            "--host",
            self.host,
            "--port",
            str(self.port),
            "--password",
            self.password,
            "--agent-id",
            self.agent_id,
        ]
        if json_output:
            cmd.extend(["--output", "json"])
        cmd.extend(args)
        return cmd

    def run(
        self,
        name: str,
        args: list[str],
        *,
        json_output: bool = True,
        allow_factorio_rejection: bool = False,
    ) -> tuple[StepResult, Any]:
        cmd = self.command(args, json_output=json_output)
        proc = subprocess.run(
            cmd,
            text=True,
            capture_output=True,
            timeout=self.timeout_s,
            check=False,
        )
        combined = "\n".join(part for part in [proc.stdout, proc.stderr] if part)
        classification = classify_failure(combined, proc.returncode)
        if proc.returncode != 0 and not (
            allow_factorio_rejection and classification == "factorio-game-rejection"
        ):
            raise SmokeError(
                _format_failure(name, cmd, classification, combined),
                classification,
            )

        parsed: Any = None
        if json_output and proc.stdout.strip():
            try:
                parsed = _json_value(
                    proc.stdout,
                    classification="rust-wrapper",
                    context=f"{name} stdout",
                )
            except SmokeError as exc:
                raise SmokeError(
                    _format_failure(name, cmd, "rust-wrapper", f"{proc.stdout}\n{exc}"),
                    "rust-wrapper",
                ) from exc
            embedded_error = _json_error_text(parsed)
            if embedded_error:
                classification = classify_failure(embedded_error)
                if not (
                    allow_factorio_rejection
                    and classification == "factorio-game-rejection"
                ):
                    raise SmokeError(
                        _format_failure(name, cmd, classification, proc.stdout),
                        classification,
                    )

        return (
            StepResult(
                name=name,
                tool_call=" ".join(cmd),
                classification=classification,
                result=_clip(combined or proc.stdout),
            ),
            parsed,
        )


class McpClient:
    def __init__(
        self,
        mcp: Path,
        host: str,
        port: int,
        password: str,
        agent_id: str,
        timeout_s: float,
    ) -> None:
        self.mcp = mcp
        self.timeout_s = timeout_s
        self._next_id = 1
        env = os.environ.copy()
        env.update(
            {
                "FACTORIO_RCON_HOST": host,
                "FACTORIO_RCON_PORT": str(port),
                "FACTORIO_RCON_PASSWORD": password,
                "FACTORIO_AGENT_ID": agent_id,
            }
        )
        self.proc = subprocess.Popen(
            [str(mcp)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        if not self.proc.stdin or not self.proc.stdout:
            raise SmokeError("failed to open MCP stdio pipes", "bridge")
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.proc.stdout, selectors.EVENT_READ)
        self._request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "factorioctl-runtime-smoke", "version": "0"},
            },
        )
        self._notify("notifications/initialized", {})

    def close(self) -> None:
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def _notify(self, method: str, params: dict[str, Any]) -> None:
        assert self.proc.stdin
        self.proc.stdin.write(
            json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n"
        )
        self.proc.stdin.flush()

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        request_id = self._next_id
        self._next_id += 1
        assert self.proc.stdin
        self.proc.stdin.write(
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": params,
                }
            )
            + "\n"
        )
        self.proc.stdin.flush()
        deadline = time.monotonic() + self.timeout_s
        while time.monotonic() < deadline:
            events = self.selector.select(deadline - time.monotonic())
            if not events:
                continue
            line = self.proc.stdout.readline()
            if not line:
                break
            parsed = _json_value_or_missing(line)
            if parsed is _JSON_MISSING:
                continue
            try:
                payload = _validated(
                    JsonRpcResponse,
                    parsed,
                    classification="bridge",
                    context=f"MCP {method} response",
                )
            except SmokeError:
                continue
            if payload.id != request_id:
                continue
            if payload.error is not None:
                raise SmokeError(json.dumps(payload.error), "bridge")
            return payload.result
        stderr = self.proc.stderr.read() if self.proc.stderr else ""
        raise SmokeError(f"MCP request timed out: {method}\n{stderr}", "bridge")

    def call_tool(
        self,
        name: str,
        arguments: dict[str, Any],
        *,
        allow_factorio_rejection: bool = False,
        allow_expected_miss: bool = False,
        require_json: bool = True,
    ) -> tuple[StepResult, Any]:
        call = {"name": name, "arguments": arguments}
        result = self._request("tools/call", call)
        text = _mcp_text(result)
        classification = classify_failure(text)
        if classification != "ok" and not (
            allow_factorio_rejection and classification == "factorio-game-rejection"
        ) and not (
            allow_expected_miss and classification == "expected-miss"
        ):
            raise SmokeError(
                _format_failure(name, [f"mcp:{name}", json.dumps(arguments)], classification, text),
                classification,
            )
        parsed: Any = None
        if require_json and text.strip():
            try:
                parsed = _json_value(
                    text,
                    classification="mod-lua",
                    context=f"{name} MCP text",
                )
            except SmokeError as exc:
                raise SmokeError(
                    _format_failure(name, [f"mcp:{name}", json.dumps(arguments)], "mod-lua", text),
                    "mod-lua",
                ) from exc
            embedded_error = _json_error_text(parsed)
            if embedded_error:
                embedded_class = classify_failure(embedded_error)
                if not (
                    allow_factorio_rejection
                    and embedded_class == "factorio-game-rejection"
                ) and not (
                    allow_expected_miss and embedded_class == "expected-miss"
                ):
                    raise SmokeError(
                        _format_failure(name, [f"mcp:{name}", json.dumps(arguments)], embedded_class, text),
                        embedded_class,
                    )
                classification = embedded_class
        return (
            StepResult(
                name=name,
                tool_call=f"mcp:{name} {json.dumps(arguments, sort_keys=True)}",
                classification=classification,
                result=_clip(text),
            ),
            parsed,
        )


def _mcp_text(result: Any) -> str:
    try:
        payload = _validated(
            McpToolResult,
            result,
            classification="bridge",
            context="MCP tool result",
        )
    except SmokeError:
        return json.dumps(result)
    return "\n".join(item.text for item in payload.content if item.type == "text")


def _json_error_text(value: Any) -> str | None:
    try:
        payload = _validated(
            SmokeResultPayload,
            value,
            classification="mod-lua",
            context="result error payload",
        )
    except SmokeError:
        return None
    if payload.error:
        return payload.error
    return None


def _format_failure(
    name: str,
    tool_call: list[str],
    classification: str,
    result: str,
) -> str:
    return (
        f"FAIL: {name}\n"
        f"classification: {classification}\n"
        f"tool_call: {' '.join(tool_call)}\n"
        f"result:\n{_clip(result)}"
    )


def _position_from_find_result(value: Any) -> tuple[int, int]:
    payload = _validated(
        FindPlacementsPayload,
        value,
        classification="mod-lua",
        context="find_entity_placements payload",
    )
    if not payload.placements:
        raise SmokeError(f"no placements returned: {value}", "factorio-game-rejection")
    pos = payload.placements[0].position
    return int(round(pos.x)), int(round(pos.y))


def _seed_inventory(cli: CliRunner) -> StepResult:
    lua = r'''
local c = remote.call("claude_interface", "get_character", "smoke")
if not (c and c.valid) then error("smoke character missing") end
c.clear_items_inside()
c.insert{name="burner-mining-drill", count=1}
c.insert{name="transport-belt", count=8}
c.insert{name="wooden-chest", count=2}
c.insert{name="stone-furnace", count=2}
c.insert{name="burner-inserter", count=1}
c.insert{name="small-electric-pole", count=8}
c.insert{name="coal", count=20}
c.insert{name="automation-science-pack", count=5}
rcon.print('{"success":true}')
'''
    result, _ = cli.run(
        "seed smoke inventory",
        ["exec", lua],
        json_output=False,
    )
    return result


def _seed_lab_fixture(cli: CliRunner) -> tuple[StepResult, LabFixturePayload]:
    lua = r'''
local surface = game.surfaces[1]
local function create_first(name, candidates)
    for _, position in ipairs(candidates) do
        if surface.can_place_entity{
            name = name,
            position = position,
            force = "player",
            build_check_type = defines.build_check_type.manual,
        } then
            local entity = surface.create_entity{
                name = name,
                position = position,
                force = "player",
            }
            if entity then return entity end
        end
    end
    error("failed to seed " .. name)
end

local lab = create_first("lab", {
    {12, 0}, {12, 4}, {-8, 6}, {20, 0}, {0, 12},
})
local chest = create_first("wooden-chest", {
    {16, 0}, {16, 4}, {-10, 6}, {22, 0}, {0, 15},
})
rcon.print(
    '{"success":true,"lab_unit_number":'
    .. tostring(lab.unit_number)
    .. ',"chest_unit_number":'
    .. tostring(chest.unit_number)
    .. '}'
)
'''
    result, _ = cli.run(
        "seed lab fixture",
        ["exec", lua],
        json_output=False,
    )
    try:
        text = result.result.splitlines()[-1]
    except IndexError as exc:
        raise SmokeError(f"failed to parse seeded lab fixture: {result.result}", "mod-lua") from exc
    payload = _json_dataclass(
        LabFixturePayload,
        text,
        classification="mod-lua",
        context="seed lab fixture",
    )
    return result, payload


def _seed_existing_power_pole(cli: CliRunner) -> StepResult:
    lua = r'''
local surface = game.surfaces[1]
local position = {8, 0}
if not surface.can_place_entity{
    name = "small-electric-pole",
    position = position,
    force = "player",
    build_check_type = defines.build_check_type.manual,
} then
    error("cannot seed smoke power pole")
end
local pole = surface.create_entity{
    name = "small-electric-pole",
    position = position,
    force = "player",
}
if not pole then error("failed to seed smoke power pole") end
rcon.print('{"success":true,"unit_number":' .. tostring(pole.unit_number) .. '}')
'''
    result, _ = cli.run(
        "seed existing power pole",
        ["exec", lua],
        json_output=False,
    )
    return result


def _steam_plan_entries(plan_result: Any) -> list[SteamPlaceArgs]:
    payload = _validated(
        SteamPlanPayload,
        plan_result,
        classification="mod-lua",
        context="steam plan payload",
    )
    plan = payload.plan
    return [
        plan.offshore_pump.place_args,
        plan.boiler.place_args,
        plan.steam_engine.place_args,
        *(pipe.place_args for pipe in plan.pipes),
    ]


def _build_existing_steam_plant(cli: CliRunner, plan_result: Any) -> StepResult:
    entries = _steam_plan_entries(plan_result)
    lua_entries = []
    for entry in entries:
        lua_entries.append(
            "{{name={name}, x={x}, y={y}, direction={direction}}}".format(
                name=json.dumps(entry.entity_name),
                x=entry.x,
                y=entry.y,
                direction=json.dumps(entry.direction or "north"),
            )
        )

    lua = """
local dirs = {north=0, east=4, south=8, west=12}
local surface = game.surfaces[1]
local entries = {
%s
}
local engine_position = nil
for _, entry in ipairs(entries) do
    local entity = surface.create_entity{
        name = entry.name,
        position = {entry.x, entry.y},
        direction = dirs[entry.direction] or 0,
        force = "player",
    }
    if not entity then error("failed to create " .. entry.name) end
    if entry.name == "steam-engine" then
        engine_position = {x = entry.x, y = entry.y}
    end
end
local far_pole_created = false
if engine_position then
    local offsets = {
        {12, 0}, {-12, 0}, {0, 12}, {0, -12},
        {14, 4}, {14, -4}, {-14, 4}, {-14, -4},
        {4, 14}, {-4, 14}, {4, -14}, {-4, -14},
        {18, 0}, {-18, 0}, {0, 18}, {0, -18},
    }
    for _, offset in ipairs(offsets) do
        local position = {engine_position.x + offset[1], engine_position.y + offset[2]}
        if surface.can_place_entity{
            name = "small-electric-pole",
            position = position,
            force = "player",
            build_check_type = defines.build_check_type.manual,
        } then
            local pole = surface.create_entity{
                name = "small-electric-pole",
                position = position,
                force = "player",
            }
            if pole then
                far_pole_created = true
                break
            end
        end
    end
end
if engine_position and not far_pole_created then error("failed to create distant steam diagnostic pole") end
rcon.print('{"success":true,"created":' .. tostring(#entries) .. ',"far_pole_created":' .. tostring(far_pole_created) .. '}')
""" % (",\n".join(lua_entries))
    result, _ = cli.run(
        "seed existing steam plant from plan",
        ["exec", lua],
        json_output=False,
    )
    return result


def run_smoke(args: argparse.Namespace) -> list[StepResult]:
    ensure_disposable_port(args.port, args.allow_main_port)
    cli = CliRunner(args.cli, args.host, args.port, args.password, args.agent_id, args.timeout)
    steps: list[StepResult] = []

    steps.append(verify_synced_mod(MOD_SOURCE, args.synced_mod_dir))
    if args.skip_live:
        return steps

    result, _ = cli.run("get tick", ["get", "tick"])
    steps.append(result)
    result, _ = cli.run("character init", ["character", "init", "--x", "0", "--y", "0"])
    steps.append(result)
    steps.append(_seed_inventory(cli))
    result, lab_fixture = _seed_lab_fixture(cli)
    steps.append(result)

    mcp = McpClient(args.mcp, args.host, args.port, args.password, args.agent_id, args.timeout)
    try:
        result, _ = cli.run("situation_report", ["situation", "--radius", "20"])
        steps.append(result)

        result, character_overlap_check = mcp.call_tool(
            "check_placement",
            {"entity_name": "wooden-chest", "x": 0, "y": 0, "direction": "north"},
            allow_factorio_rejection=True,
        )
        steps.append(result)
        check_text = (
            json.dumps(character_overlap_check, sort_keys=True)
            if character_overlap_check is not None
            else result.result
        )
        if "character_overlap" not in check_text or "walk_to_clear_placement" not in check_text:
            raise SmokeError(
                _format_failure(
                    "character-overlap check_placement",
                    ["mcp:check_placement", json.dumps({"x": 0, "y": 0})],
                    "mod-lua",
                    check_text,
                ),
                "mod-lua",
            )

        result, character_overlap_place = mcp.call_tool(
            "place_entity",
            {"entity_name": "wooden-chest", "x": 0, "y": 0, "direction": "north"},
            allow_factorio_rejection=True,
        )
        steps.append(result)
        place_text = (
            json.dumps(character_overlap_place, sort_keys=True)
            if character_overlap_place is not None
            else result.result
        )
        if "Placement overlaps agent character" not in place_text or "character_overlap" not in place_text:
            raise SmokeError(
                _format_failure(
                    "character-overlap place_entity",
                    ["mcp:place_entity", json.dumps({"x": 0, "y": 0})],
                    "mod-lua",
                    place_text,
                ),
                "mod-lua",
            )

        result, _ = mcp.call_tool(
            "get_power_status",
            {"x": 0, "y": 0, "radius": 50},
            allow_expected_miss=True,
        )
        steps.append(result)
        result, no_plant_diag = mcp.call_tool(
            "diagnose_steam_power",
            {"x": 0, "y": 0, "radius": 50},
            allow_expected_miss=True,
        )
        steps.append(result)
        no_plant_diag = _payload(
            SteamNoPlantDiagnostic,
            no_plant_diag,
            context="no-plant diagnostic",
        )
        if no_plant_diag.status != "no_plant":
            raise SmokeError(f"no-plant diagnostic missing status=no_plant: {no_plant_diag}", "mod-lua")
        if no_plant_diag.next_action != "build_steam_power":
            raise SmokeError(f"no-plant diagnostic missing build action: {no_plant_diag}", "mod-lua")
        result, no_plant_repair = mcp.call_tool(
            "repair_steam_power",
            {"x": 0, "y": 0, "radius": 50, "target_x": 0, "target_y": 0},
            allow_expected_miss=True,
        )
        steps.append(result)
        no_plant_repair = _payload(
            SteamNoPlantRepair,
            no_plant_repair,
            context="no-plant repair",
        )
        if not _has_blocker(no_plant_repair.blockers, "no_steam_power_found"):
            raise SmokeError(f"repair helper did not report no-plant blocker: {no_plant_repair}", "mod-lua")
        suggested = no_plant_repair.suggested_next_tool
        if not suggested or suggested.tool != "plan_steam_power":
            raise SmokeError(f"repair helper missing plan-steam fallback: {no_plant_repair}", "mod-lua")
        result, no_grid_extension = mcp.call_tool(
            "extend_power_to",
            {"x": 0, "y": 0, "radius": 20, "target_x": 2, "target_y": 0},
            allow_expected_miss=True,
        )
        steps.append(result)
        no_grid_extension = _payload(
            SteamNoPlantRepair,
            no_grid_extension,
            context="no-grid extension",
        )
        if not _has_blocker(no_grid_extension.blockers, "no_power_grid_found"):
            raise SmokeError(f"extend helper did not report no-grid blocker: {no_grid_extension}", "mod-lua")
        steps.append(_seed_existing_power_pole(cli))
        result, extension_plan = mcp.call_tool(
            "extend_power_to",
            {"x": 0, "y": 0, "radius": 20, "target_x": 2, "target_y": 0},
        )
        steps.append(result)
        extension_plan = _payload(
            PowerExtensionPlan,
            extension_plan,
            context="power extension plan",
        )
        if not extension_plan.dry_run:
            raise SmokeError(f"extend helper did not return dry_run plan: {extension_plan}", "mod-lua")
        if not _has_entity_step(
            extension_plan.steps,
            tool="place_entity",
            entity_name="small-electric-pole",
        ):
            raise SmokeError(f"extend helper missing pole placement step: {extension_plan}", "mod-lua")
        if extension_plan.missing_items:
            raise SmokeError(f"extend helper unexpectedly missing seeded pole item: {extension_plan}", "mod-lua")

        lab_unit = lab_fixture.lab_unit_number
        chest_unit = lab_fixture.chest_unit_number
        if not isinstance(lab_unit, int) or not isinstance(chest_unit, int):
            raise SmokeError(f"lab fixture missing unit numbers: {lab_fixture}", "mod-lua")
        result, lab_feed_plan = mcp.call_tool(
            "feed_lab_from_inventory",
            {
                "lab_unit_number": lab_unit,
                "science_pack": "automation-science-pack",
                "count": 3,
            },
        )
        steps.append(result)
        lab_feed_plan = _payload(
            LabFeedPlan,
            lab_feed_plan,
            context="lab feed dry-run",
        )
        if not lab_feed_plan.dry_run:
            raise SmokeError(f"lab feed helper did not default to dry_run: {lab_feed_plan}", "mod-lua")
        if lab_feed_plan.success is not True or lab_feed_plan.ready is not True:
            raise SmokeError(f"lab feed dry-run was not ready with seeded packs: {lab_feed_plan}", "mod-lua")
        if not _has_execute_lab_feed_step(lab_feed_plan.steps):
            raise SmokeError(f"lab feed helper missing guarded execute step: {lab_feed_plan}", "mod-lua")

        result, lab_feed_exec = mcp.call_tool(
            "feed_lab_from_inventory",
            {
                "lab_unit_number": lab_unit,
                "science_pack": "automation-science-pack",
                "count": 3,
                "dry_run": False,
            },
        )
        steps.append(result)
        lab_feed_exec = _payload(
            LabFeedPlan,
            lab_feed_exec,
            context="lab feed execute",
        )
        if lab_feed_exec.inserted != 3:
            raise SmokeError(f"lab feed execute did not insert 3 packs: {lab_feed_exec}", "mod-lua")
        if lab_feed_exec.lab_after < 3:
            raise SmokeError(f"lab feed execute did not update lab inventory: {lab_feed_exec}", "mod-lua")

        result, missing_lab_feed = mcp.call_tool(
            "feed_lab_from_inventory",
            {
                "lab_unit_number": lab_unit,
                "science_pack": "automation-science-pack",
                "count": 99,
            },
            allow_expected_miss=True,
        )
        steps.append(result)
        missing_lab_feed = _payload(
            LabFeedPlan,
            missing_lab_feed,
            context="lab feed missing-pack",
        )
        if not _has_blocker(missing_lab_feed.blockers, "missing_science_pack"):
            raise SmokeError(f"lab feed missing-pack path missing blocker: {missing_lab_feed}", "mod-lua")

        result, wrong_lab_feed = mcp.call_tool(
            "feed_lab_from_inventory",
            {
                "lab_unit_number": chest_unit,
                "science_pack": "automation-science-pack",
                "count": 1,
            },
            allow_expected_miss=True,
        )
        steps.append(result)
        wrong_lab_feed = _payload(
            LabFeedPlan,
            wrong_lab_feed,
            context="lab feed wrong-entity",
        )
        if not _has_blocker(wrong_lab_feed.blockers, "not_a_lab", "no_lab_inventory"):
            raise SmokeError(f"lab feed wrong-entity path missing blocker: {wrong_lab_feed}", "mod-lua")

        result, steam_plan = cli.run(
            "power plan-steam",
            ["power", "plan-steam", "--water-area", "-64,-64,64,64", "--target", "0,0"],
            allow_factorio_rejection=True,
        )
        steps.append(result)
        steam_plan_result = _payload(
            SteamPlannerResult,
            steam_plan,
            context="steam planner result",
        )
        if not steam_plan_result.placement_success:
            raise SmokeError(f"steam planner did not find a placeable layout: {steam_plan}", "mod-lua")
        if steam_plan_result.target is None:
            raise SmokeError(f"steam planner missing target: {steam_plan}", "mod-lua")

        steps.append(_build_existing_steam_plant(cli, steam_plan))
        target = steam_plan_result.target
        result, existing_diag = mcp.call_tool(
            "diagnose_steam_power",
            {
                **_target_args(target),
                "radius": 80,
            },
        )
        steps.append(result)
        existing_diag = _payload(
            ExistingPlantDiagnostic,
            existing_diag,
            context="existing plant diagnostic",
        )
        if not existing_diag.has_existing_plant:
            raise SmokeError(f"existing-plant diagnostic did not detect plant: {existing_diag}", "mod-lua")
        summary = existing_diag.summary
        if summary.offshore_pumps < 1 or summary.boilers < 1 or summary.steam_engines < 1:
            raise SmokeError(f"existing-plant diagnostic missing steam entities: {existing_diag}", "mod-lua")
        issue_types = {issue.type for issue in existing_diag.issues}
        if "boiler_no_fuel" not in issue_types:
            raise SmokeError(f"existing-plant diagnostic missing boiler_no_fuel: {existing_diag}", "mod-lua")
        if "steam_engine_pole_route_incomplete" not in issue_types:
            raise SmokeError(
                f"existing-plant diagnostic missing pole-route issue: {existing_diag}",
                "mod-lua",
            )
        result, repair_plan = mcp.call_tool(
            "repair_steam_power",
            {
                **_target_args(target),
                "radius": 80,
                "target_x": 0,
                "target_y": 0,
            },
        )
        steps.append(result)
        repair_plan = _payload(
            SteamRepairPlan,
            repair_plan,
            context="steam repair plan",
        )
        if not repair_plan.dry_run:
            raise SmokeError(f"repair helper did not return dry_run plan: {repair_plan}", "mod-lua")
        if not _has_tool_step(repair_plan.repair_steps, "insert_items") or not _has_tool_step(
            repair_plan.repair_steps,
            "place_entity",
        ):
            raise SmokeError(f"repair helper missing fuel or pole steps: {repair_plan}", "mod-lua")
        if repair_plan.missing_items:
            raise SmokeError(f"repair helper unexpectedly missing seeded materials: {repair_plan}", "mod-lua")

        result, existing_plan = cli.run(
            "power plan-steam existing plant",
            ["power", "plan-steam", "--water-area", "-64,-64,64,64", "--target", "0,0"],
            allow_factorio_rejection=True,
        )
        steps.append(result)
        existing_plan = _payload(
            SteamPlannerResult,
            existing_plan,
            context="existing steam planner result",
        )
        if not _has_blocker(existing_plan.blockers, "existing_steam_power_found"):
            raise SmokeError(f"steam planner did not prefer existing diagnostic: {existing_plan}", "mod-lua")
        if existing_plan.existing_plant is None:
            raise SmokeError(f"steam planner missing existing_plant diagnostic: {existing_plan}", "mod-lua")

        result, chest_plan = mcp.call_tool(
            "find_entity_placements",
            {"entity_name": "wooden-chest", "x": 0, "y": 0, "radius": 8, "limit": 1},
        )
        steps.append(result)
        chest_x, chest_y = _position_from_find_result(chest_plan)
        result, check = mcp.call_tool(
            "check_placement",
            {"entity_name": "wooden-chest", "x": chest_x, "y": chest_y, "direction": "north"},
        )
        steps.append(result)
        check = _payload(
            PlacementCheckPayload,
            check,
            context="check_placement payload",
        )
        if check.allowed is None:
            raise SmokeError(f"check_placement returned no allowed field: {check}", "mod-lua")

        result, placed_chest = cli.run(
            "place_entity wooden-chest",
            ["place", "wooden-chest", "--at", f"{chest_x},{chest_y}"],
        )
        steps.append(result)
        placed_chest = _payload(
            PlacedEntityPayload,
            placed_chest,
            context="wooden chest placement",
        )
        unit_number = placed_chest.unit_number
        if not unit_number:
            raise SmokeError(f"place result missing unit_number: {placed_chest}", "rust-wrapper")
        result, _ = cli.run(
            "remove_entity wooden-chest",
            ["remove", "--unit-number", str(unit_number)],
            json_output=False,
        )
        steps.append(result)

        result, furnace_plan = mcp.call_tool(
            "find_entity_placements",
            {"entity_name": "stone-furnace", "x": 3, "y": 3, "radius": 8, "limit": 1},
        )
        steps.append(result)
        furnace_x, furnace_y = _position_from_find_result(furnace_plan)
        result, placed_furnace = mcp.call_tool(
            "place_entity",
            {"entity_name": "stone-furnace", "x": furnace_x, "y": furnace_y, "direction": "north"},
        )
        steps.append(result)
        placed_furnace = _payload(
            PlacedEntityPayload,
            placed_furnace,
            context="stone furnace placement",
        )
        if not placed_furnace.unit_number:
            raise SmokeError(f"furnace place result missing unit_number: {placed_furnace}", "mod-lua")
        result, belt_on_furnace = mcp.call_tool(
            "place_entity",
            {
                "entity_name": "transport-belt",
                "x": furnace_x,
                "y": furnace_y,
                "direction": "east",
            },
            allow_factorio_rejection=True,
        )
        steps.append(result)
        belt_on_furnace = _payload(
            PlacementFailurePayload,
            belt_on_furnace,
            context="belt blocked by furnace payload",
        )
        if (
            belt_on_furnace.alternate_belt_placements is None
            or belt_on_furnace.candidate_alternate_path is None
        ):
            raise SmokeError(
                _format_failure(
                    "belt blocked by furnace footprint",
                    ["mcp:place_entity", json.dumps({"x": furnace_x, "y": furnace_y})],
                    "mod-lua",
                    str(belt_on_furnace),
                ),
                "mod-lua",
            )
        if not _has_named_blocker_with_footprint(
            belt_on_furnace.blockers,
            "stone-furnace",
        ):
            raise SmokeError(f"belt failure missing blocker footprint: {belt_on_furnace}", "mod-lua")

        result, belt_plan = mcp.call_tool(
            "find_entity_placements",
            {"entity_name": "transport-belt", "x": 0, "y": 0, "radius": 8, "limit": 1},
        )
        steps.append(result)
        belt_x, belt_y = _position_from_find_result(belt_plan)
        result, placed_belt = mcp.call_tool(
            "place_entity",
            {"entity_name": "transport-belt", "x": belt_x, "y": belt_y, "direction": "east"},
        )
        steps.append(result)
        placed_belt = _payload(
            PlacedEntityPayload,
            placed_belt,
            context="belt placement",
        )
        belt_unit_number = placed_belt.unit_number
        if not belt_unit_number:
            raise SmokeError(f"belt place result missing unit_number: {placed_belt}", "mod-lua")
        result, same_tile = mcp.call_tool(
            "place_entity",
            {"entity_name": "transport-belt", "x": belt_x, "y": belt_y, "direction": "north"},
            allow_factorio_rejection=True,
        )
        same_tile = _payload(
            PlacementFailurePayload,
            same_tile,
            context="same-tile belt placement",
        )
        if (
            same_tile.recommended_action != "rotate_entity"
            and same_tile.rotate_entity is None
            and same_tile.unit_number is None
        ):
            raise SmokeError(
                _format_failure(
                    "same-tile belt placement",
                    ["mcp:place_entity", json.dumps({"x": belt_x, "y": belt_y})],
                    "mod-lua",
                    str(same_tile),
                ),
                "mod-lua",
            )
        steps.append(result)
        result, rotated = mcp.call_tool(
            "rotate_entity",
            {"unit_number": belt_unit_number, "direction": "north"},
        )
        steps.append(result)
        rotated = _payload(
            RotateEntityPayload,
            rotated,
            context="rotate entity payload",
        )
        if rotated.success is not True:
            raise SmokeError(f"rotate_entity returned failure: {rotated}", "mod-lua")
        if rotated.direction != 0:
            raise SmokeError(f"rotate_entity did not set north direction: {rotated}", "mod-lua")

        result, resource = mcp.call_tool(
            "find_nearest_resource",
            {"resource_type": "iron-ore", "x": 0, "y": 0},
        )
        steps.append(result)
        resource = _payload(
            NearestResourcePayload,
            resource,
            context="nearest resource payload",
        )
        drill_center_x = int(round(resource.center_x))
        drill_center_y = int(round(resource.center_y))
        result, drill_plan = mcp.call_tool(
            "find_entity_placements",
            {
                "entity_name": "burner-mining-drill",
                "x": drill_center_x,
                "y": drill_center_y,
                "radius": 25,
                "limit": 5,
            },
        )
        steps.append(result)
        drill_plan = _payload(
            DrillPlacementsPayload,
            drill_plan,
            context="drill placement payload",
        )
        if not drill_plan.placements:
            raise SmokeError(f"drill placement search returned no placements: {drill_plan}", "mod-lua")
        first = drill_plan.placements[0]
        if first.output is None:
            raise SmokeError(f"drill placement missing output diagnostics: {first}", "mod-lua")
        if first.output_buildable is not True:
            raise SmokeError(f"drill placement output is not buildable: {first}", "mod-lua")
        if first.output_clear is not True:
            raise SmokeError(f"drill placement did not prefer a clear output tile: {first}", "mod-lua")
        if first.output.belt_tile is None:
            raise SmokeError(f"drill output diagnostics missing belt_tile: {first.output}", "mod-lua")

        result, edge_miner = mcp.call_tool(
            "build_edge_miner",
            {
                "resource_type": "iron-ore",
                "x": drill_center_x,
                "y": drill_center_y,
                "radius": 25,
                "drill_name": "burner-mining-drill",
                "limit": 5,
            },
        )
        steps.append(result)
        edge_miner = _payload(
            EdgeMinerPlan,
            edge_miner,
            context="edge miner helper payload",
        )
        if not edge_miner.dry_run:
            raise SmokeError(f"edge miner helper did not return dry_run plan: {edge_miner}", "mod-lua")
        if edge_miner.success is not True or edge_miner.ready is not True:
            raise SmokeError(f"edge miner helper was not ready with seeded inventory: {edge_miner}", "mod-lua")
        selected = edge_miner.selected
        if selected is None:
            raise SmokeError(f"edge miner helper missing selected candidate: {edge_miner}", "mod-lua")
        if selected.output_buildable is not True or selected.output_clear is not True:
            raise SmokeError(f"edge miner did not select a clear buildable output: {edge_miner}", "mod-lua")
        if selected.resource_tiles < 1:
            raise SmokeError(f"edge miner selected candidate not backed by resource tiles: {edge_miner}", "mod-lua")
        if not _has_entity_step(
            edge_miner.steps,
            tool="place_entity",
            entity_name="burner-mining-drill",
        ) or not _has_entity_step(
            edge_miner.steps,
            tool="place_entity",
            entity_name="transport-belt",
        ):
            raise SmokeError(f"edge miner helper missing drill or belt step: {edge_miner}", "mod-lua")
        if edge_miner.missing_items:
            raise SmokeError(f"edge miner unexpectedly missing seeded items: {edge_miner}", "mod-lua")

        selected_output = selected.output
        if selected_output is None or selected_output.belt_tile is None:
            raise SmokeError(f"edge miner selected output missing belt_tile: {edge_miner}", "mod-lua")
        output_tile = selected_output.belt_tile
        result, direct_smelter = mcp.call_tool(
            "build_direct_smelter",
            {
                "output_x": output_tile.x,
                "output_y": output_tile.y,
                "output_direction": str(selected_output.belt_direction or 8),
                "furnace_name": "stone-furnace",
                "inserter_name": "burner-inserter",
                "belt_name": "transport-belt",
                "radius": 6,
            },
        )
        steps.append(result)
        direct_smelter = _payload(
            DirectSmelterPlan,
            direct_smelter,
            context="direct smelter helper payload",
        )
        if not direct_smelter.dry_run:
            raise SmokeError(f"direct smelter helper did not return dry_run plan: {direct_smelter}", "mod-lua")
        if direct_smelter.success is not True or direct_smelter.ready is not True:
            raise SmokeError(f"direct smelter helper was not ready with seeded inventory: {direct_smelter}", "mod-lua")
        for expected in ["transport-belt", "stone-furnace", "burner-inserter"]:
            if not _has_entity_step(direct_smelter.steps, tool="place_entity", entity_name=expected):
                raise SmokeError(f"direct smelter helper missing {expected} step: {direct_smelter}", "mod-lua")
        if direct_smelter.missing_items:
            raise SmokeError(f"direct smelter unexpectedly missing seeded items: {direct_smelter}", "mod-lua")
        if direct_smelter.selected is None or direct_smelter.selected.input_inserter is None:
            raise SmokeError(f"direct smelter missing selected inserter geometry: {direct_smelter}", "mod-lua")
        if direct_smelter.verify_step is None or direct_smelter.verify_step.tool != "verify_production":
            raise SmokeError(f"direct smelter missing verify_production step: {direct_smelter}", "mod-lua")

        result, mined = mcp.call_tool(
            "mine_at",
            {"x": resource.center_x, "y": resource.center_y, "count": 3},
        )
        steps.append(result)
        mined = _payload(
            MineAtPayload,
            mined,
            context="mine_at payload",
        )
        if mined.mined_count is None:
            raise SmokeError(f"mine_at result missing mined_count: {mined}", "mod-lua")
    finally:
        mcp.close()

    return steps


def print_results(steps: list[StepResult]) -> None:
    for step in steps:
        print(f"PASS: {step.name} [{step.classification}]")
        print(f"  tool_call: {step.tool_call}")
        if step.result:
            print(f"  result: {_clip(step.result, 500).replace(chr(10), chr(10) + '  ')}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default=os.environ.get("FACTORIO_RCON_HOST", "localhost"))
    parser.add_argument("--port", type=int, default=int(os.environ.get("FACTORIO_RCON_PORT", "27016")))
    parser.add_argument("--password", default=os.environ.get("FACTORIO_RCON_PASSWORD", "test_password"))
    parser.add_argument("--agent-id", default=os.environ.get("FACTORIO_AGENT_ID", "smoke"))
    parser.add_argument("--cli", type=Path, default=DEFAULT_CLI)
    parser.add_argument("--mcp", type=Path, default=DEFAULT_MCP)
    parser.add_argument(
        "--synced-mod-dir",
        type=Path,
        default=Path(os.environ.get("FACTORIOCTL_SYNCED_MOD_DIR", DEFAULT_SYNCED_MOD)),
        help="Synced claude-interface mod directory to compare against the repo copy.",
    )
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument(
        "--skip-live",
        action="store_true",
        help="Only verify the synced mod copy; do not connect to RCON or mutate a save.",
    )
    parser.add_argument(
        "--allow-main-port",
        action="store_true",
        help="Allow port 27015. Use only with a disposable current save.",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    try:
        steps = run_smoke(args)
        print_results(steps)
        print(f"\nRuntime smoke completed: {len(steps)} checks passed")
        return 0
    except SmokeError as exc:
        print(f"Runtime smoke failed [{exc.classification}]: {exc}", file=sys.stderr)
        return 1
    except subprocess.TimeoutExpired as exc:
        print(f"Runtime smoke failed [bridge]: command timed out: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
