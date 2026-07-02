"""Bridge <-> Factorio game transport: RCON commands out, JSONL file in."""

from pathlib import Path

from models import (
    BridgeInputFileDelta,
    BridgeInputMessage,
    CharacterPlacementResult,
    BridgeValidationError,
    ModInterfaceStatus,
    RconRemoteCall,
    SurfaceSetupResult,
    SurfaceSetupResults,
)
from rcon import RCONClient, lua_long_string


def send_response(rcon: RCONClient, player_index: int, agent_name: str, text: str):
    encoded = lua_long_string(text)
    agent_encoded = lua_long_string(agent_name)
    rcon.execute(RconRemoteCall.side_effect_command(
        "receive_response",
        player_index,
        agent_encoded,
        encoded,
    ))


def send_tool_status(rcon: RCONClient, player_index: int, agent_name: str, tool_name: str):
    agent_encoded = lua_long_string(agent_name)
    encoded = lua_long_string(tool_name)
    rcon.execute(RconRemoteCall.side_effect_command(
        "tool_status",
        player_index,
        agent_encoded,
        encoded,
    ))


def set_status(rcon: RCONClient, player_index: int, status: str):
    encoded = lua_long_string(status)
    rcon.execute(RconRemoteCall.side_effect_command("set_status", player_index, encoded))


def register_agent(rcon: RCONClient, agent_name: str, label: str | None = None):
    encoded = lua_long_string(agent_name)
    if label:
        label_encoded = lua_long_string(label)
        lua = RconRemoteCall.side_effect_command("register_agent", encoded, label_encoded)
    else:
        lua = RconRemoteCall.side_effect_command("register_agent", encoded)
    rcon.execute(lua)


def unregister_agent(rcon, agent_name: str):
    encoded = lua_long_string(agent_name)
    rcon.execute(RconRemoteCall.side_effect_command("unregister_agent", encoded))


def setup_surfaces_model(rcon, planets: list[str]) -> SurfaceSetupResults:
    """Ensure planet surfaces exist. Creates them if missing.
    Returns typed {planet: status} entries where status is 'exists' or 'created'."""
    results = []
    for planet in planets:
        planet_encoded = lua_long_string(planet)
        result = rcon.execute(RconRemoteCall.command("ensure_surface_result", planet_encoded))
        results.append(SurfaceSetupResult.from_rcon_response(result, planet=planet))
    return SurfaceSetupResults(results=results)


def setup_surfaces(rcon, planets: list[str]) -> dict[str, str]:
    """Legacy dict wrapper for setup_surfaces_model."""
    return setup_surfaces_model(rcon, planets).to_dict()


def pre_place_character_model(
    rcon,
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
    agent_encoded = lua_long_string(agent_name)
    planet_encoded = lua_long_string(planet)
    result = rcon.execute(RconRemoteCall.command(
        "pre_place_character_result",
        agent_encoded,
        planet_encoded,
        spawn_x,
    ))
    return CharacterPlacementResult.from_rcon_response(
        result,
        agent_name=agent_name,
        planet=planet,
    )


def pre_place_character(rcon, agent_name: str, planet: str, spawn_offset: int = 0) -> str:
    """Legacy status-string wrapper for pre_place_character_model."""
    return pre_place_character_model(
        rcon,
        agent_name,
        planet,
        spawn_offset=spawn_offset,
    ).to_status()


def set_spectator_mode(rcon, enabled: bool = True):
    """Enable/disable spectator mode via the mod. When enabled, all connecting
    players are automatically set to spectator (no character body).
    Persists across player joins — no timing issues."""
    val = "true" if enabled else "false"
    rcon.execute(RconRemoteCall.side_effect_command("set_spectator_mode", val))


def check_mod_loaded(rcon) -> bool:
    result = rcon.execute(
        "/silent-command "
        "rcon.print('{\"loaded\":' .. "
        "tostring(remote.interfaces[\"claude_interface\"] ~= nil) .. '}')"
    )
    try:
        return ModInterfaceStatus.from_rcon_response(result).loaded
    except BridgeValidationError:
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

    def poll(self) -> list[dict]:
        return self.poll_delta_model().to_dicts()
