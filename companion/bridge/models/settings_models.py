from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'live', 'input_models')

class BridgeRuntimeEnvField(BridgeModel):
    """Typed binding from an environment variable to a runtime settings field."""

    env_name: str
    field_name: str

    @field_validator("env_name", "field_name", mode="before")
    @classmethod
    def _coerce_non_empty_text(cls, value: Any) -> str:
        text = str(value or "").strip()
        if not text:
            raise ValueError("expected non-empty string")
        return text

    @classmethod
    def validate_unique(
        cls,
        fields: Iterable["BridgeRuntimeEnvField"],
        *,
        field_path: str,
    ) -> tuple["BridgeRuntimeEnvField", ...]:
        env_names: set[str] = set()
        field_names: set[str] = set()
        result: list[BridgeRuntimeEnvField] = []
        for field in fields:
            binding = cls.model_validate(field)
            if binding.env_name in env_names:
                raise BridgeValidationError(field_path, "duplicate env_name")
            if binding.field_name in field_names:
                raise BridgeValidationError(field_path, "duplicate field_name")
            env_names.add(binding.env_name)
            field_names.add(binding.field_name)
            result.append(binding)
        return tuple(result)

    @classmethod
    def read_source(
        cls,
        source: Any,
        fields: Iterable["BridgeRuntimeEnvField"],
    ) -> dict[str, Any]:
        env = source if isinstance(source, dict) or hasattr(source, "__contains__") else {}
        data: dict[str, Any] = {}
        for binding in fields:
            try:
                if binding.env_name in env:
                    data[binding.field_name] = env[binding.env_name]
            except (TypeError, KeyError):
                continue
        return data


ProviderUsageLimitSettings.ENV_FIELDS = (
    BridgeRuntimeEnvField(
        env_name="BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET",
        field_name="usage_limit_reset_utc_offset",
    ),
)

RawLuaPolicy.ENV_FIELDS = (
    BridgeRuntimeEnvField(
        env_name="FACTORIOCTL_ALLOW_RAW_LUA",
        field_name="allow_raw_lua",
    ),
)

SdkSkillConfig.ENV_FIELDS = (
    BridgeRuntimeEnvField(env_name="BRIDGE_SDK_SKILLS", field_name="skills"),
)


class TelemetryRelaySettings(BridgeModel):
    """Typed remote telemetry relay settings resolved from CLI and environment."""

    relay_url: str | None = None
    relay_token: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="RELAY_URL", field_name="relay_url"),
        BridgeRuntimeEnvField(env_name="RELAY_TOKEN", field_name="relay_token"),
    )

    @staticmethod
    def _optional_text(value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @field_validator("relay_url", "relay_token", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        return cls._optional_text(value)

    @classmethod
    def from_sources(
        cls,
        *,
        cli_url: Any = None,
        cli_token: Any = None,
        env: Any = None,
    ) -> "TelemetryRelaySettings":
        env_data = BridgeRuntimeEnvField.read_source(env, cls.env_fields())

        return cls(
            relay_url=cls._optional_text(cli_url) or env_data.get("relay_url"),
            relay_token=cls._optional_text(cli_token) or env_data.get("relay_token"),
        )

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="telemetry_relay_env_fields",
        )

    @property
    def enabled(self) -> bool:
        return self.relay_url is not None

    @property
    def ready(self) -> bool:
        return self.relay_url is not None and self.relay_token is not None


class BridgeRuntimeSettings(BridgeModel):
    """Typed runtime settings resolved from bridge environment variables."""

    max_turns: int = 200
    context_window_backoff_s: float = 900.0
    tick_timeout_s: float = 2400.0
    stream_idle_timeout_s: float = 300.0
    watchdog_same_failure_limit: int = 3
    watchdog_no_progress_timeout_s: float = 900.0
    mutating_tool_batch_window_s: float = 1.0

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="BRIDGE_MAX_TURNS", field_name="max_turns"),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_CONTEXT_WINDOW_BACKOFF_S",
            field_name="context_window_backoff_s",
        ),
        BridgeRuntimeEnvField(env_name="BRIDGE_TICK_TIMEOUT_S", field_name="tick_timeout_s"),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_STREAM_IDLE_TIMEOUT_S",
            field_name="stream_idle_timeout_s",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_WATCHDOG_SAME_FAILURE_LIMIT",
            field_name="watchdog_same_failure_limit",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_WATCHDOG_NO_PROGRESS_TIMEOUT_S",
            field_name="watchdog_no_progress_timeout_s",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S",
            field_name="mutating_tool_batch_window_s",
        ),
    )
    INT_DEFAULTS: ClassVar[dict[str, tuple[int, int]]] = {
        "max_turns": (200, 1),
        "watchdog_same_failure_limit": (3, 0),
    }
    FLOAT_DEFAULTS: ClassVar[dict[str, tuple[float, float]]] = {
        "context_window_backoff_s": (900.0, 1.0),
        "tick_timeout_s": (2400.0, 1.0),
        "stream_idle_timeout_s": (300.0, 1.0),
        "watchdog_no_progress_timeout_s": (900.0, 0.0),
        "mutating_tool_batch_window_s": (1.0, 0.0),
    }

    @field_validator("max_turns", "watchdog_same_failure_limit", mode="before")
    @classmethod
    def _coerce_int_setting(cls, value: Any, info) -> int:
        default, minimum = cls.INT_DEFAULTS[info.field_name]
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @field_validator(
        "context_window_backoff_s",
        "tick_timeout_s",
        "stream_idle_timeout_s",
        "watchdog_no_progress_timeout_s",
        "mutating_tool_batch_window_s",
        mode="before",
    )
    @classmethod
    def _coerce_float_setting(cls, value: Any, info) -> float:
        default, minimum = cls.FLOAT_DEFAULTS[info.field_name]
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @classmethod
    def from_env(cls, env: Any) -> "BridgeRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="runtime_env_fields",
        )


class LedgerRuntimeSettings(BridgeModel):
    """Typed ledger persistence settings resolved from bridge environment."""

    stale_bootstrap_ledger_max_age_s: float = 1800.0

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(
            env_name="BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S",
            field_name="stale_bootstrap_ledger_max_age_s",
        ),
    )

    @field_validator("stale_bootstrap_ledger_max_age_s", mode="before")
    @classmethod
    def _coerce_stale_bootstrap_max_age(cls, value: Any) -> float:
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return 1800.0
        return parsed if parsed >= 0.0 else 1800.0

    @classmethod
    def from_env(cls, env: Any) -> "LedgerRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="ledger_env_fields",
        )


class LearningRuntimeSettings(BridgeModel):
    """Typed learning-memory settings resolved from bridge environment."""

    learning_dir: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="BRIDGE_LEARNING_DIR", field_name="learning_dir"),
    )

    @field_validator("learning_dir", mode="before")
    @classmethod
    def _coerce_learning_dir(cls, value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @classmethod
    def from_env(cls, env: Any) -> "LearningRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="learning_env_fields",
        )

    def resolved_learning_dir(self, project_root: Any) -> Path:
        if self.learning_dir:
            return Path(self.learning_dir)
        return Path(project_root) / ".factorioctl" / "learned"


class DotEnvAssignmentLine(BridgeModel):
    """Typed parse result for one bridge-local .env line."""

    line: str = ""
    key: str = ""
    value: str = ""
    valid: bool = False

    @field_validator("line", "key", "value", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def from_line(cls, value: Any) -> "DotEnvAssignmentLine":
        line = str(value or "").strip()
        if not line or line.startswith("#") or "=" not in line:
            return cls(line=line)
        key, raw_value = line.split("=", 1)
        key = key.strip()
        if not key:
            return cls(line=line, value=raw_value.strip())
        return cls(line=line, key=key, value=raw_value.strip(), valid=True)


class DotEnvFile(BridgeModel):
    """Typed parse result for the bridge-local .env file."""

    assignments: dict[str, str] = Field(default_factory=dict)

    @field_validator("assignments", mode="before")
    @classmethod
    def _coerce_assignments(cls, value: Any) -> dict[str, str]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, str] = {}
        for raw_key, raw_value in value.items():
            key = str(raw_key).strip()
            value_text = str(raw_value).strip() if raw_value is not None else ""
            if key:
                result[key] = value_text
        return result

    @classmethod
    def from_text(cls, value: Any) -> "DotEnvFile":
        assignments: dict[str, str] = {}
        for line in BridgeTextLines.from_text(value, keep_blank=False).lines:
            assignment = DotEnvAssignmentLine.from_line(line)
            if assignment.valid:
                assignments[assignment.key] = assignment.value
        return cls(assignments=assignments)

    def apply_to_environ(self, environ: Any) -> None:
        if not hasattr(environ, "__contains__") or not hasattr(environ, "__setitem__"):
            return
        for key, value in self.assignments.items():
            if value and key not in environ:
                environ[key] = value


class FactorioPathSettings(BridgeModel):
    """Typed path-related bridge settings resolved from environment variables."""

    server_data: str | None = None
    mods_dir: str | None = None
    mcp_bin: str | None = None
    bridge_state_dir: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="FACTORIO_SERVER_DATA", field_name="server_data"),
        BridgeRuntimeEnvField(env_name="FACTORIO_MODS_DIR", field_name="mods_dir"),
        BridgeRuntimeEnvField(env_name="FACTORIOCTL_MCP_BIN", field_name="mcp_bin"),
        BridgeRuntimeEnvField(
            env_name="FACTORIOCTL_BRIDGE_STATE_DIR",
            field_name="bridge_state_dir",
        ),
    )

    @field_validator("server_data", "mods_dir", "mcp_bin", "bridge_state_dir", mode="before")
    @classmethod
    def _coerce_optional_path(cls, value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @classmethod
    def from_env(cls, env: Any) -> "FactorioPathSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="factorio_path_env_fields",
        )

    @property
    def script_output_dir(self) -> Path | None:
        return Path(self.server_data) / "script-output" if self.server_data else None

    @property
    def mods_dir_path(self) -> Path | None:
        return Path(self.mods_dir) if self.mods_dir else None

    @property
    def mcp_bin_path(self) -> Path | None:
        return Path(self.mcp_bin) if self.mcp_bin else None

    @property
    def bridge_state_dir_path(self) -> Path | None:
        return Path(self.bridge_state_dir) if self.bridge_state_dir else None


class FactorioModInfo(BridgeModel):
    """Typed view of a Factorio mod info.json file."""

    model_config = ConfigDict(frozen=True, extra="ignore")

    name: str = ""
    version: str = ""
    title: str = ""

    @field_validator("name", "version", "title", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @classmethod
    def from_file_text(cls, value: Any) -> "FactorioModInfo":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "mod_info")
        return cls.model_validate(data)

    @property
    def version_label(self) -> str:
        return self.version or "?"


class RconConnectionSettings(BridgeModel):
    """Typed RCON connection settings shared by bridge tools."""

    host: str = "localhost"
    port: int = 27015
    password: str = "factorio"

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_HOST", field_name="host"),
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_PORT", field_name="port"),
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_PASSWORD", field_name="password"),
    )

    @field_validator("host", mode="before")
    @classmethod
    def _coerce_host(cls, value: Any) -> str:
        return _input_text(value, "localhost")

    @field_validator("password", mode="before")
    @classmethod
    def _coerce_password(cls, value: Any) -> str:
        return _input_text(value, "factorio")

    @field_validator("port", mode="before")
    @classmethod
    def _coerce_port(cls, value: Any) -> int:
        try:
            port = int(value)
        except (TypeError, ValueError):
            return 27015
        return port if 1 <= port <= 65535 else 27015

    @classmethod
    def from_env(cls, env: Any) -> "RconConnectionSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="rcon_env_fields",
        )

    def to_env(self, *, agent_id: Any = None) -> dict[str, str]:
        result = {
            "FACTORIO_RCON_HOST": self.host,
            "FACTORIO_RCON_PORT": str(self.port),
            "FACTORIO_RCON_PASSWORD": self.password,
        }
        if agent_id is not None:
            result["FACTORIO_AGENT_ID"] = _input_text(agent_id, "default")
        return result


class FactorioMcpServerConfig(BridgeModel):
    """Typed Claude SDK stdio MCP config for the factorioctl server."""

    server_name: str = "factorioctl"
    command: str
    args: list[str] = Field(default_factory=list)
    rcon_host: str = "localhost"
    rcon_port: int = 27015
    rcon_password: str = "factorio"
    agent_id: str = "default"

    @field_validator("server_name", "rcon_host", "rcon_password", "agent_id", mode="before")
    @classmethod
    def _coerce_text_setting(cls, value: Any, info) -> str:
        defaults = {
            "server_name": "factorioctl",
            "rcon_host": "localhost",
            "rcon_password": "factorio",
            "agent_id": "default",
        }
        return _input_text(value, defaults[info.field_name])

    @field_validator("command", mode="before")
    @classmethod
    def _coerce_command(cls, value: Any) -> str:
        text = _input_text(value)
        if not text:
            raise ValueError("command is required")
        return text

    @field_validator("args", mode="before")
    @classmethod
    def _coerce_args(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("rcon_port", mode="before")
    @classmethod
    def _coerce_port(cls, value: Any) -> int:
        return RconConnectionSettings(port=value).port

    def to_sdk_config(self) -> dict[str, dict[str, Any]]:
        rcon = RconConnectionSettings(
            host=self.rcon_host,
            port=self.rcon_port,
            password=self.rcon_password,
        )
        return {
            self.server_name: {
                "type": "stdio",
                "command": self.command,
                "args": list(self.args),
                "env": rcon.to_env(agent_id=self.agent_id),
            }
        }
