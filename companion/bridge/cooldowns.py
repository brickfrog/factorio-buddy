from __future__ import annotations

import os
import threading
from datetime import datetime, timedelta, timezone
from typing import Any

from models import (
    BridgeRuntimeSettings,
    ProviderUsageLimit,
    ProviderUsageLimitSettings,
)

_USAGE_LIMIT_COOLDOWNS: dict[str, datetime] = {}
_USAGE_LIMIT_COOLDOWNS_LOCK = threading.Lock()
_CONTEXT_WINDOW_COOLDOWNS: dict[str, datetime] = {}
_CONTEXT_WINDOW_COOLDOWNS_LOCK = threading.Lock()
_PROVIDER_USAGE_LIMIT_SETTINGS = ProviderUsageLimitSettings.from_env(os.environ)


def _runtime_settings(env: Any = None) -> BridgeRuntimeSettings:
    return BridgeRuntimeSettings.from_env(os.environ if env is None else env)


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
    return _set_usage_limit_cooldown_from_limit(
        agent_name,
        ProviderUsageLimit.from_text(
            text,
            now=now,
            default_utc_offset=_PROVIDER_USAGE_LIMIT_SETTINGS.usage_limit_reset_utc_offset,
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


def _set_usage_limit_cooldown_for(
    agent_name: str,
    seconds: float,
    log=None,
    now: datetime | None = None,
) -> datetime:
    if now is None:
        now = datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.astimezone()
    delay_s = max(1.0, float(seconds))
    reset_at = now + timedelta(seconds=delay_s)

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
