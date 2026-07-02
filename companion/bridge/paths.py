"""Path discovery for Factorio script-output, mods, and factorioctl binaries."""

import os
import shutil
from pathlib import Path

from models import FactorioPathSettings


def _path_settings() -> FactorioPathSettings:
    return FactorioPathSettings.from_env(os.environ)


def find_script_output() -> Path:
    """Find the Factorio script-output directory."""
    configured = _path_settings().script_output_dir
    if configured:
        p = configured
        p.mkdir(parents=True, exist_ok=True)
        return p

    search = Path.cwd()
    while search != search.parent:
        candidate = search / ".factorio-server-data" / "script-output"
        if candidate.parent.exists():
            candidate.mkdir(parents=True, exist_ok=True)
            return candidate
        search = search.parent

    fallback_candidates = [
        Path("~/.factorio/script-output").expanduser(),
        Path(
            "~/.var/app/com.valvesoftware.Steam/.local/share/Steam/"
            "steamapps/common/Factorio/script-output"
        ).expanduser(),
    ]
    for c in fallback_candidates:
        if c.parent.exists():
            c.mkdir(parents=True, exist_ok=True)
            return c

    raise FileNotFoundError(
        "Could not find Factorio script-output directory. "
        "Set FACTORIO_SERVER_DATA or run from the project root."
    )


def find_mod_source() -> Path:
    """Find the mod source directory (mod/claude-interface/)."""
    search = Path.cwd()
    while search != search.parent:
        candidate = search / "mod" / "claude-interface"
        if candidate.is_dir():
            return candidate
        search = search.parent
    raise FileNotFoundError(
        "Could not find mod/claude-interface/ directory. Run from the project root."
    )


def find_mods_dir() -> Path:
    """Find the Factorio mods directory for deployment.
    Checks FACTORIO_MODS_DIR env var, then common locations."""
    configured = _path_settings().mods_dir_path
    if configured:
        p = configured
        if p.is_dir():
            return p
        raise FileNotFoundError(f"FACTORIO_MODS_DIR={configured} does not exist")

    candidates = [
        Path("~/.factorio/mods").expanduser(),
        Path(
            "~/.var/app/com.valvesoftware.Steam/.factorio/mods"
        ).expanduser(),
        Path(
            "~/Library/Application Support/factorio/mods"
        ).expanduser(),
        Path("~/AppData/Roaming/Factorio/mods").expanduser(),
    ]
    for c in candidates:
        if c.is_dir():
            return c

    raise FileNotFoundError(
        "Could not find Factorio mods directory. "
        "Set FACTORIO_MODS_DIR env var or add it to bridge/.env"
    )


def find_factorioctl_mcp() -> str | None:
    """Find the factorioctl MCP server binary."""
    configured = _path_settings().mcp_bin_path
    if configured and configured.is_file():
        return str(configured)

    # Walk up looking for the built mcp binary. Supports both the monorepo
    # layout (<repo>/target/release/mcp, with the bridge under <repo>/companion)
    # and the legacy split-repo layout (<root>/factorioctl/target/release/mcp).
    search = Path.cwd()
    while search != search.parent:
        for rel in (
            ("target", "release", "mcp"),
            ("factorioctl", "target", "release", "mcp"),
        ):
            candidate = search.joinpath(*rel)
            if candidate.is_file():
                return str(candidate)
        search = search.parent

    found = shutil.which("factorioctl-mcp")
    if found:
        return found

    return None
