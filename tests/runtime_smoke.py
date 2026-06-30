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
from dataclasses import dataclass
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
MOD_SOURCE = REPO_ROOT / "companion" / "mod" / "claude-interface"
DEFAULT_SYNCED_MOD = Path.home() / ".factorio" / "mods" / "claude-interface"
DEFAULT_CLI = REPO_ROOT / "target" / "release" / "factorioctl"
DEFAULT_MCP = REPO_ROOT / "target" / "release" / "mcp"


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
                parsed = json.loads(proc.stdout)
            except json.JSONDecodeError as exc:
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
            try:
                payload = json.loads(line)
            except json.JSONDecodeError:
                continue
            if payload.get("id") != request_id:
                continue
            if "error" in payload:
                raise SmokeError(json.dumps(payload["error"]), "bridge")
            return payload.get("result")
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
                parsed = json.loads(text)
            except json.JSONDecodeError as exc:
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
    if not isinstance(result, dict):
        return json.dumps(result)
    parts: list[str] = []
    for item in result.get("content", []):
        if isinstance(item, dict) and item.get("type") == "text":
            parts.append(str(item.get("text", "")))
    return "\n".join(parts)


def _json_error_text(value: Any) -> str | None:
    if isinstance(value, dict):
        error = value.get("error")
        if isinstance(error, str) and error:
            return error
        if value.get("success") is False and isinstance(error, str):
            return error
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
    placements = value.get("placements") if isinstance(value, dict) else None
    if not placements:
        raise SmokeError(f"no placements returned: {value}", "factorio-game-rejection")
    pos = placements[0].get("position", {})
    return int(round(pos["x"])), int(round(pos["y"]))


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


def _seed_lab_fixture(cli: CliRunner) -> tuple[StepResult, dict[str, Any]]:
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
        payload = json.loads(result.result.splitlines()[-1])
    except (IndexError, json.JSONDecodeError) as exc:
        raise SmokeError(f"failed to parse seeded lab fixture: {result.result}", "mod-lua") from exc
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


def _steam_plan_entries(plan_result: Any) -> list[dict[str, Any]]:
    plan = plan_result.get("plan") if isinstance(plan_result, dict) else None
    if not isinstance(plan, dict):
        raise SmokeError(f"steam plan missing plan object: {plan_result}", "mod-lua")

    entries: list[dict[str, Any]] = []
    for key in ["offshore_pump", "boiler", "steam_engine"]:
        entity = plan.get(key)
        place_args = entity.get("place_args") if isinstance(entity, dict) else None
        if not isinstance(place_args, dict):
            raise SmokeError(f"steam plan missing {key}.place_args: {plan}", "mod-lua")
        entries.append(place_args)
    for pipe in plan.get("pipes", []):
        place_args = pipe.get("place_args") if isinstance(pipe, dict) else None
        if not isinstance(place_args, dict):
            raise SmokeError(f"steam plan has malformed pipe entry: {pipe}", "mod-lua")
        entries.append(place_args)
    return entries


def _build_existing_steam_plant(cli: CliRunner, plan_result: Any) -> StepResult:
    entries = _steam_plan_entries(plan_result)
    lua_entries = []
    for entry in entries:
        lua_entries.append(
            "{{name={name}, x={x}, y={y}, direction={direction}}}".format(
                name=json.dumps(str(entry["entity_name"])),
                x=float(entry["x"]),
                y=float(entry["y"]),
                direction=json.dumps(str(entry.get("direction", "north"))),
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
        if not isinstance(no_plant_diag, dict) or no_plant_diag.get("status") != "no_plant":
            raise SmokeError(f"no-plant diagnostic missing status=no_plant: {no_plant_diag}", "mod-lua")
        if no_plant_diag.get("next_action") != "build_steam_power":
            raise SmokeError(f"no-plant diagnostic missing build action: {no_plant_diag}", "mod-lua")
        result, no_plant_repair = mcp.call_tool(
            "repair_steam_power",
            {"x": 0, "y": 0, "radius": 50, "target_x": 0, "target_y": 0},
            allow_expected_miss=True,
        )
        steps.append(result)
        blockers = no_plant_repair.get("blockers") if isinstance(no_plant_repair, dict) else []
        if not any(
            isinstance(blocker, dict)
            and blocker.get("type") == "no_steam_power_found"
            for blocker in blockers
        ):
            raise SmokeError(f"repair helper did not report no-plant blocker: {no_plant_repair}", "mod-lua")
        suggested = no_plant_repair.get("suggested_next_tool") if isinstance(no_plant_repair, dict) else None
        if not isinstance(suggested, dict) or suggested.get("tool") != "plan_steam_power":
            raise SmokeError(f"repair helper missing plan-steam fallback: {no_plant_repair}", "mod-lua")
        result, no_grid_extension = mcp.call_tool(
            "extend_power_to",
            {"x": 0, "y": 0, "radius": 20, "target_x": 2, "target_y": 0},
            allow_expected_miss=True,
        )
        steps.append(result)
        blockers = no_grid_extension.get("blockers") if isinstance(no_grid_extension, dict) else []
        if not any(
            isinstance(blocker, dict)
            and blocker.get("type") == "no_power_grid_found"
            for blocker in blockers
        ):
            raise SmokeError(f"extend helper did not report no-grid blocker: {no_grid_extension}", "mod-lua")
        steps.append(_seed_existing_power_pole(cli))
        result, extension_plan = mcp.call_tool(
            "extend_power_to",
            {"x": 0, "y": 0, "radius": 20, "target_x": 2, "target_y": 0},
        )
        steps.append(result)
        if not isinstance(extension_plan, dict) or not extension_plan.get("dry_run"):
            raise SmokeError(f"extend helper did not return dry_run plan: {extension_plan}", "mod-lua")
        extension_steps = extension_plan.get("steps") if isinstance(extension_plan, dict) else []
        if not any(
            isinstance(step, dict)
            and step.get("tool") == "place_entity"
            and step.get("tool_args", {}).get("entity_name") == "small-electric-pole"
            for step in extension_steps
        ):
            raise SmokeError(f"extend helper missing pole placement step: {extension_plan}", "mod-lua")
        if extension_plan.get("missing_items"):
            raise SmokeError(f"extend helper unexpectedly missing seeded pole item: {extension_plan}", "mod-lua")

        lab_unit = lab_fixture.get("lab_unit_number")
        chest_unit = lab_fixture.get("chest_unit_number")
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
        if not isinstance(lab_feed_plan, dict) or not lab_feed_plan.get("dry_run"):
            raise SmokeError(f"lab feed helper did not default to dry_run: {lab_feed_plan}", "mod-lua")
        if lab_feed_plan.get("success") is not True or lab_feed_plan.get("ready") is not True:
            raise SmokeError(f"lab feed dry-run was not ready with seeded packs: {lab_feed_plan}", "mod-lua")
        feed_steps = lab_feed_plan.get("steps") if isinstance(lab_feed_plan, dict) else []
        if not any(
            isinstance(step, dict)
            and step.get("tool") == "feed_lab_from_inventory"
            and step.get("tool_args", {}).get("dry_run") is False
            for step in feed_steps
        ):
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
        if not isinstance(lab_feed_exec, dict) or lab_feed_exec.get("inserted") != 3:
            raise SmokeError(f"lab feed execute did not insert 3 packs: {lab_feed_exec}", "mod-lua")
        if lab_feed_exec.get("lab_after", 0) < 3:
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
        blockers = missing_lab_feed.get("blockers") if isinstance(missing_lab_feed, dict) else []
        if not any(
            isinstance(blocker, dict)
            and blocker.get("type") == "missing_science_pack"
            for blocker in blockers
        ):
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
        blockers = wrong_lab_feed.get("blockers") if isinstance(wrong_lab_feed, dict) else []
        if not any(
            isinstance(blocker, dict)
            and blocker.get("type") in {"not_a_lab", "no_lab_inventory"}
            for blocker in blockers
        ):
            raise SmokeError(f"lab feed wrong-entity path missing blocker: {wrong_lab_feed}", "mod-lua")

        result, steam_plan = cli.run(
            "power plan-steam",
            ["power", "plan-steam", "--water-area", "-64,-64,64,64", "--target", "0,0"],
            allow_factorio_rejection=True,
        )
        steps.append(result)
        if not isinstance(steam_plan, dict) or not steam_plan.get("placement_success"):
            raise SmokeError(f"steam planner did not find a placeable layout: {steam_plan}", "mod-lua")

        steps.append(_build_existing_steam_plant(cli, steam_plan))
        target = steam_plan.get("target") if isinstance(steam_plan, dict) else {}
        result, existing_diag = mcp.call_tool(
            "diagnose_steam_power",
            {
                "x": int(round(float(target.get("x", 0)))),
                "y": int(round(float(target.get("y", 0)))),
                "radius": 80,
            },
        )
        steps.append(result)
        if not isinstance(existing_diag, dict) or not existing_diag.get("has_existing_plant"):
            raise SmokeError(f"existing-plant diagnostic did not detect plant: {existing_diag}", "mod-lua")
        summary = existing_diag.get("summary", {})
        if summary.get("offshore_pumps", 0) < 1 or summary.get("boilers", 0) < 1 or summary.get("steam_engines", 0) < 1:
            raise SmokeError(f"existing-plant diagnostic missing steam entities: {existing_diag}", "mod-lua")
        issue_types = {
            issue.get("type")
            for issue in existing_diag.get("issues", [])
            if isinstance(issue, dict)
        }
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
                "x": int(round(float(target.get("x", 0)))),
                "y": int(round(float(target.get("y", 0)))),
                "radius": 80,
                "target_x": 0,
                "target_y": 0,
            },
        )
        steps.append(result)
        if not isinstance(repair_plan, dict) or not repair_plan.get("dry_run"):
            raise SmokeError(f"repair helper did not return dry_run plan: {repair_plan}", "mod-lua")
        repair_steps = repair_plan.get("repair_steps") if isinstance(repair_plan, dict) else []
        repair_tools = {
            step.get("tool")
            for step in repair_steps
            if isinstance(step, dict)
        }
        if "insert_items" not in repair_tools or "place_entity" not in repair_tools:
            raise SmokeError(f"repair helper missing fuel or pole steps: {repair_plan}", "mod-lua")
        if repair_plan.get("missing_items"):
            raise SmokeError(f"repair helper unexpectedly missing seeded materials: {repair_plan}", "mod-lua")

        result, existing_plan = cli.run(
            "power plan-steam existing plant",
            ["power", "plan-steam", "--water-area", "-64,-64,64,64", "--target", "0,0"],
            allow_factorio_rejection=True,
        )
        steps.append(result)
        blockers = existing_plan.get("blockers") if isinstance(existing_plan, dict) else []
        if not any(
            isinstance(blocker, dict)
            and blocker.get("type") == "existing_steam_power_found"
            for blocker in blockers
        ):
            raise SmokeError(f"steam planner did not prefer existing diagnostic: {existing_plan}", "mod-lua")
        if "existing_plant" not in existing_plan:
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
        if not isinstance(check, dict) or "allowed" not in check:
            raise SmokeError(f"check_placement returned no allowed field: {check}", "mod-lua")

        result, placed_chest = cli.run(
            "place_entity wooden-chest",
            ["place", "wooden-chest", "--at", f"{chest_x},{chest_y}"],
        )
        steps.append(result)
        unit_number = placed_chest.get("unit_number") if isinstance(placed_chest, dict) else None
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
        if not isinstance(placed_furnace, dict) or not placed_furnace.get("unit_number"):
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
        text = json.dumps(belt_on_furnace, sort_keys=True) if belt_on_furnace is not None else result.result
        if "alternate_belt_placements" not in text or "candidate_alternate_path" not in text:
            raise SmokeError(
                _format_failure(
                    "belt blocked by furnace footprint",
                    ["mcp:place_entity", json.dumps({"x": furnace_x, "y": furnace_y})],
                    "mod-lua",
                    text,
                ),
                "mod-lua",
            )
        blockers = (
            belt_on_furnace.get("blockers")
            if isinstance(belt_on_furnace, dict)
            else None
        )
        if not any(
            isinstance(blocker, dict)
            and blocker.get("name") == "stone-furnace"
            and blocker.get("bounding_box")
            for blocker in (blockers or [])
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
        belt_unit_number = placed_belt.get("unit_number") if isinstance(placed_belt, dict) else None
        if not belt_unit_number:
            raise SmokeError(f"belt place result missing unit_number: {placed_belt}", "mod-lua")
        result, same_tile = mcp.call_tool(
            "place_entity",
            {"entity_name": "transport-belt", "x": belt_x, "y": belt_y, "direction": "north"},
            allow_factorio_rejection=True,
        )
        text = json.dumps(same_tile, sort_keys=True) if same_tile is not None else result.result
        if "rotate_entity" not in text and "unit_number" not in text:
            raise SmokeError(
                _format_failure(
                    "same-tile belt placement",
                    ["mcp:place_entity", json.dumps({"x": belt_x, "y": belt_y})],
                    "mod-lua",
                    text,
                ),
                "mod-lua",
            )
        steps.append(result)
        result, rotated = mcp.call_tool(
            "rotate_entity",
            {"unit_number": belt_unit_number, "direction": "north"},
        )
        steps.append(result)
        if not isinstance(rotated, dict) or rotated.get("success") is not True:
            raise SmokeError(f"rotate_entity returned failure: {rotated}", "mod-lua")
        if rotated.get("direction") != 0:
            raise SmokeError(f"rotate_entity did not set north direction: {rotated}", "mod-lua")

        result, resource = mcp.call_tool(
            "find_nearest_resource",
            {"resource_type": "iron-ore", "x": 0, "y": 0},
        )
        steps.append(result)
        if isinstance(resource, dict) and "center_x" in resource and "center_y" in resource:
            drill_center_x = int(round(resource["center_x"]))
            drill_center_y = int(round(resource["center_y"]))
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
            placements = (
                drill_plan.get("placements")
                if isinstance(drill_plan, dict)
                else None
            )
            if not placements:
                raise SmokeError(f"drill placement search returned no placements: {drill_plan}", "mod-lua")
            first = placements[0]
            output = first.get("output") if isinstance(first, dict) else None
            if not isinstance(output, dict):
                raise SmokeError(f"drill placement missing output diagnostics: {first}", "mod-lua")
            if first.get("output_buildable") is not True:
                raise SmokeError(f"drill placement output is not buildable: {first}", "mod-lua")
            if first.get("output_clear") is not True:
                raise SmokeError(f"drill placement did not prefer a clear output tile: {first}", "mod-lua")
            if "belt_tile" not in output:
                raise SmokeError(f"drill output diagnostics missing belt_tile: {output}", "mod-lua")

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
            if not isinstance(edge_miner, dict) or not edge_miner.get("dry_run"):
                raise SmokeError(f"edge miner helper did not return dry_run plan: {edge_miner}", "mod-lua")
            if edge_miner.get("success") is not True or edge_miner.get("ready") is not True:
                raise SmokeError(f"edge miner helper was not ready with seeded inventory: {edge_miner}", "mod-lua")
            selected = edge_miner.get("selected") if isinstance(edge_miner, dict) else None
            if not isinstance(selected, dict):
                raise SmokeError(f"edge miner helper missing selected candidate: {edge_miner}", "mod-lua")
            if selected.get("output_buildable") is not True or selected.get("output_clear") is not True:
                raise SmokeError(f"edge miner did not select a clear buildable output: {edge_miner}", "mod-lua")
            if selected.get("resource_tiles", 0) < 1:
                raise SmokeError(f"edge miner selected candidate not backed by resource tiles: {edge_miner}", "mod-lua")
            edge_steps = edge_miner.get("steps") if isinstance(edge_miner, dict) else []
            edge_tools = [
                step.get("tool_args", {}).get("entity_name")
                for step in edge_steps
                if isinstance(step, dict)
            ]
            if "burner-mining-drill" not in edge_tools or "transport-belt" not in edge_tools:
                raise SmokeError(f"edge miner helper missing drill or belt step: {edge_miner}", "mod-lua")
            if edge_miner.get("missing_items"):
                raise SmokeError(f"edge miner unexpectedly missing seeded items: {edge_miner}", "mod-lua")

            selected_output = selected.get("output") if isinstance(selected, dict) else None
            output_tile = selected_output.get("belt_tile") if isinstance(selected_output, dict) else None
            if not isinstance(output_tile, dict):
                raise SmokeError(f"edge miner selected output missing belt_tile: {edge_miner}", "mod-lua")
            result, direct_smelter = mcp.call_tool(
                "build_direct_smelter",
                {
                    "output_x": output_tile.get("x"),
                    "output_y": output_tile.get("y"),
                    "output_direction": str(selected_output.get("belt_direction", 8)),
                    "furnace_name": "stone-furnace",
                    "inserter_name": "burner-inserter",
                    "belt_name": "transport-belt",
                    "radius": 6,
                },
            )
            steps.append(result)
            if not isinstance(direct_smelter, dict) or not direct_smelter.get("dry_run"):
                raise SmokeError(f"direct smelter helper did not return dry_run plan: {direct_smelter}", "mod-lua")
            if direct_smelter.get("success") is not True or direct_smelter.get("ready") is not True:
                raise SmokeError(f"direct smelter helper was not ready with seeded inventory: {direct_smelter}", "mod-lua")
            smelter_steps = direct_smelter.get("steps") if isinstance(direct_smelter, dict) else []
            smelter_tools = [
                step.get("tool_args", {}).get("entity_name")
                for step in smelter_steps
                if isinstance(step, dict)
            ]
            for expected in ["transport-belt", "stone-furnace", "burner-inserter"]:
                if expected not in smelter_tools:
                    raise SmokeError(f"direct smelter helper missing {expected} step: {direct_smelter}", "mod-lua")
            if direct_smelter.get("missing_items"):
                raise SmokeError(f"direct smelter unexpectedly missing seeded items: {direct_smelter}", "mod-lua")
            selected_smelter = direct_smelter.get("selected")
            if not isinstance(selected_smelter, dict) or "input_inserter" not in selected_smelter:
                raise SmokeError(f"direct smelter missing selected inserter geometry: {direct_smelter}", "mod-lua")
            verify_step = direct_smelter.get("verify_step") if isinstance(direct_smelter, dict) else None
            if not isinstance(verify_step, dict) or verify_step.get("tool") != "verify_production":
                raise SmokeError(f"direct smelter missing verify_production step: {direct_smelter}", "mod-lua")

            result, mined = mcp.call_tool(
                "mine_at",
                {"x": resource["center_x"], "y": resource["center_y"], "count": 3},
            )
            steps.append(result)
            if isinstance(mined, dict) and "mined_count" not in mined:
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
