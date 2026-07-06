"""Bridge <-> Factorio game transport: lifecycle commands out, JSONL file in."""

import asyncio
import contextlib
import os
import sys
import threading
from concurrent.futures import TimeoutError as FutureTimeoutError
from datetime import timedelta
from pathlib import Path
from typing import Any

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from models.settings_models import RconConnectionSettings
from models import (
    BridgeInputFileDelta,
    BridgeInputMessage,
    CharacterPlacementResult,
    SurfaceSetupResult,
    SurfaceSetupResults,
)


class McpLifecycleClient:
    """Persistent stdio MCP client for bridge lifecycle remotes."""

    def __init__(
        self,
        mcp_bin: str,
        *,
        rcon_host: str,
        rcon_port: int,
        rcon_password: str,
        agent_id: str = "bridge",
        timeout_s: float = 30.0,
        errlog=None,
    ):
        self.mcp_bin = str(mcp_bin)
        self.rcon = RconConnectionSettings(
            host=rcon_host,
            port=rcon_port,
            password=rcon_password,
        )
        self.agent_id = str(agent_id or "bridge")
        self.timeout_s = max(0.1, float(timeout_s))
        self.errlog = errlog if errlog is not None else sys.stderr
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(
            target=self._run_loop,
            name="factorio-mcp-lifecycle",
            daemon=True,
        )
        self._started = threading.Event()
        self._start_error: BaseException | None = None
        self._session: ClientSession | None = None
        self._stack: contextlib.AsyncExitStack | None = None
        self._call_lock = threading.Lock()
        self._thread.start()
        self._started.wait(timeout=self.timeout_s)
        if self._start_error is not None:
            raise RuntimeError(f"failed to start lifecycle MCP client: {self._start_error}")
        if self._session is None:
            raise RuntimeError("timed out starting lifecycle MCP client")

    def call_tool(self, tool_name: str, **arguments: Any) -> str:
        with self._call_lock:
            future = asyncio.run_coroutine_threadsafe(
                self._call_tool(tool_name, arguments),
                self._loop,
            )
            try:
                return future.result(timeout=self.timeout_s)
            except FutureTimeoutError as exc:
                future.cancel()
                raise TimeoutError(f"MCP lifecycle call timed out: {tool_name}") from exc

    def send_chat_response(self, player_index: int, agent_name: str, text: str) -> str:
        return self.call_tool(
            "send_chat_response",
            player_index=player_index,
            agent_name=agent_name,
            text=text,
        )

    def tool_status(self, player_index: int, agent_name: str, tool_name: str) -> str:
        return self.call_tool(
            "tool_status",
            player_index=player_index,
            agent_name=agent_name,
            tool_name=tool_name,
        )

    def set_status(self, player_index: int, status: str) -> str:
        return self.call_tool("set_status", player_index=player_index, status=status)

    def register_agent(self, agent_name: str, label: str | None = None) -> str:
        arguments: dict[str, Any] = {"agent_name": agent_name}
        if label is not None:
            arguments["label"] = label
        return self.call_tool("register_agent", **arguments)

    def unregister_agent(self, agent_name: str) -> str:
        return self.call_tool("unregister_agent", agent_name=agent_name)

    def ensure_surface(self, planet: str) -> str:
        return self.call_tool("ensure_surface", planet=planet)

    def place_character(self, agent_name: str, planet: str, spawn_x: float) -> str:
        return self.call_tool(
            "place_character",
            agent_name=agent_name,
            planet=planet,
            spawn_x=spawn_x,
        )

    def set_spectator_mode(self, enabled: bool = True) -> str:
        return self.call_tool("set_spectator_mode", enabled=enabled)

    def ping(self) -> str:
        return self.call_tool("ping")

    def live_state(self, agent_name: str) -> str:
        return self.call_tool("live_state", agent_name=agent_name)

    def connected_player_count(self) -> str:
        return self.call_tool("connected_player_count")

    def eval_production_snapshot(self, surface_name: str = "nauvis") -> str:
        return self.call_tool("eval_production_snapshot", surface_name=surface_name)

    def get_power_status(self, x: float, y: float, radius: float) -> str:
        return self.call_tool("get_power_status", x=x, y=y, radius=radius)

    def close(self):
        if not self._loop.is_running():
            return
        future = asyncio.run_coroutine_threadsafe(self._close_async(), self._loop)
        with contextlib.suppress(Exception):
            future.result(timeout=self.timeout_s)
        self._loop.call_soon_threadsafe(self._loop.stop)
        self._thread.join(timeout=self.timeout_s)

    def _run_loop(self):
        asyncio.set_event_loop(self._loop)
        try:
            self._loop.run_until_complete(self._start_async())
        except BaseException as exc:
            self._start_error = exc
            self._started.set()
            return
        self._started.set()
        self._loop.run_forever()
        with contextlib.suppress(Exception):
            self._loop.run_until_complete(self._close_async())
        self._loop.close()

    async def _start_async(self):
        self._stack = contextlib.AsyncExitStack()
        env = os.environ.copy()
        env.update(self.rcon.to_env(agent_id=self.agent_id))
        server = StdioServerParameters(command=self.mcp_bin, env=env)
        read_stream, write_stream = await self._stack.enter_async_context(
            stdio_client(server, errlog=self.errlog)
        )
        self._session = await self._stack.enter_async_context(
            ClientSession(
                read_stream,
                write_stream,
                read_timeout_seconds=timedelta(seconds=self.timeout_s),
            )
        )
        await self._session.initialize()

    async def _call_tool(self, tool_name: str, arguments: dict[str, Any]) -> str:
        if self._session is None:
            raise RuntimeError("lifecycle MCP client is not initialized")
        result = await self._session.call_tool(
            tool_name,
            arguments,
            read_timeout_seconds=timedelta(seconds=self.timeout_s),
        )
        parts = []
        for item in result.content or []:
            text = getattr(item, "text", None)
            if text is not None:
                parts.append(str(text))
            elif isinstance(item, dict) and item.get("type") == "text":
                parts.append(str(item.get("text", "")))
        return "\n".join(parts)

    async def _close_async(self):
        if self._stack is not None:
            stack = self._stack
            self._stack = None
            self._session = None
            await stack.aclose()


def send_response(lifecycle, player_index: int, agent_name: str, text: str):
    lifecycle.send_chat_response(player_index, agent_name, text)


def send_tool_status(lifecycle, player_index: int, agent_name: str, tool_name: str):
    lifecycle.tool_status(player_index, agent_name, tool_name)


def set_status(lifecycle, player_index: int, status: str):
    lifecycle.set_status(player_index, status)


def register_agent(lifecycle, agent_name: str, label: str | None = None):
    lifecycle.register_agent(agent_name, label=label)


def unregister_agent(lifecycle, agent_name: str):
    lifecycle.unregister_agent(agent_name)


def setup_surfaces_model(lifecycle, planets: list[str]) -> SurfaceSetupResults:
    """Ensure planet surfaces exist. Creates them if missing.
    Returns typed {planet: status} entries where status is 'exists' or 'created'."""
    results = []
    for planet in planets:
        result = lifecycle.ensure_surface(planet)
        results.append(SurfaceSetupResult.from_rcon_response(result, planet=planet))
    return SurfaceSetupResults(results=results)


def pre_place_character_model(
    lifecycle,
    agent_name: str,
    planet: str,
    spawn_offset: int = 0,
) -> CharacterPlacementResult:
    """Create or teleport an agent's character to the specified planet surface.
    Forces terrain generation around spawn so agents don't land in void.
    spawn_offset shifts the X position to avoid overlapping with the player.
    Returns a typed status: already_placed, teleported, created,
    surface_not_found, creation_failed.

    All character state lives in mod storage (synced in MP) — no _G.global usage."""
    spawn_x = spawn_offset * 5 + 5  # offset from player spawn at (0,0)
    result = lifecycle.place_character(agent_name, planet, spawn_x)
    return CharacterPlacementResult.from_rcon_response(
        result,
        agent_name=agent_name,
        planet=planet,
    )


def set_spectator_mode(lifecycle, enabled: bool = True):
    """Enable/disable spectator mode via the mod. When enabled, all connecting
    players are automatically set to spectator (no character body).
    Persists across player joins — no timing issues."""
    lifecycle.set_spectator_mode(enabled=enabled)


def check_mod_loaded(lifecycle) -> bool:
    try:
        return str(lifecycle.ping()).strip() == "pong"
    except Exception:
        return False


class InputWatcher:
    def __init__(self, input_file: Path):
        self.input_file = input_file
        self.last_size = 0
        if input_file.exists():
            self.last_size = input_file.stat().st_size

    def poll_delta_model(self) -> BridgeInputFileDelta:
        if not self.input_file.exists():
            return BridgeInputFileDelta.empty(
                previous_size=self.last_size,
                current_size=self.last_size,
            )
        current_size = self.input_file.stat().st_size
        if current_size <= self.last_size:
            return BridgeInputFileDelta.empty(
                previous_size=self.last_size,
                current_size=current_size,
            )
        with open(self.input_file, "r") as f:
            f.seek(self.last_size)
            new_data = f.read()
        delta = BridgeInputFileDelta.from_chunk(
            previous_size=self.last_size,
            current_size=current_size,
            text=new_data,
        )
        self.last_size = delta.next_size
        return delta

    def poll_model(self) -> list[BridgeInputMessage]:
        return self.poll_delta_model().messages
