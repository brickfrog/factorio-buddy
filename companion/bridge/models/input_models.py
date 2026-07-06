from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base')

def _input_text(value: Any, default: str = "") -> str:
    if value is None:
        return default
    text = str(value).strip()
    return text if text else default


def _input_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in {"1", "true", "yes", "y", "on"}:
            return True
        if normalized in {"0", "false", "no", "n", "off", ""}:
            return False
    return False


def _input_player_index(value: Any) -> int:
    if isinstance(value, bool):
        return 1
    try:
        index = int(value)
    except (TypeError, ValueError):
        return 1
    return index if index >= 0 else 1


class BridgeInputMessage(RemotePayloadModel):
    """Typed inbound game/autonomy message while preserving JSONL dict shape."""

    message: str
    player_index: int = 1
    player_name: str = "Player"
    target_agent: str = "default"
    response_to: str | None = None
    model: str | None = None
    autonomy: bool = False
    read_only_tools: bool = False

    @field_validator("message", mode="before")
    @classmethod
    def _coerce_message(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @field_validator("player_index", mode="before")
    @classmethod
    def _coerce_player_index(cls, value: Any) -> int:
        return _input_player_index(value)

    @field_validator("player_name", mode="before")
    @classmethod
    def _coerce_player_name(cls, value: Any) -> str:
        return _input_text(value, "Player")

    @field_validator("target_agent", mode="before")
    @classmethod
    def _coerce_target_agent(cls, value: Any) -> str:
        return _input_text(value, "default")

    @field_validator("response_to", "model", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @field_validator("autonomy", "read_only_tools", mode="before")
    @classmethod
    def _coerce_flag(cls, value: Any) -> bool:
        return _input_bool(value)

    @classmethod
    def from_mapping(cls, value: Any) -> "BridgeInputMessage | None":
        if isinstance(value, cls):
            return value if value.message else None
        if not isinstance(value, dict):
            return None
        try:
            message = cls.model_validate(value)
        except ValidationError:
            return None
        return message if message.message else None

    @classmethod
    def from_json_line(cls, value: Any) -> "BridgeInputMessage | None":
        if isinstance(value, cls):
            return cls.from_mapping(value)
        if not isinstance(value, str):
            return None
        line = value.strip()
        if not line:
            return None
        data = _json_value_or_missing(line)
        if data is _JSON_MISSING:
            return None
        return cls.from_mapping(data)

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "message": self.message,
            "player_index": self.player_index,
            "player_name": self.player_name,
            "target_agent": self.target_agent,
        })
        if self.response_to is not None:
            result["response_to"] = self.response_to
        if self.model is not None:
            result["model"] = self.model
        if self.autonomy:
            result["autonomy"] = True
        if self.read_only_tools:
            result["read_only_tools"] = True
        return result


class BridgeInputMessageCollection(BridgeModel):
    """Typed sequence of bridge input messages from parsed JSONL payloads."""

    items: tuple[BridgeInputMessage, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[BridgeInputMessage, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        messages: list[BridgeInputMessage] = []
        for item in value:
            message = BridgeInputMessage.from_mapping(item)
            if message:
                messages.append(message)
        return tuple(messages)

    @classmethod
    def from_value(cls, value: Any) -> "BridgeInputMessageCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[BridgeInputMessage]:
        return list(self.items)


class BridgeInputBatch(BridgeModel):
    """Typed JSONL ingress batch from the Factorio chat input file."""

    messages: list[BridgeInputMessage] = Field(default_factory=list)

    @classmethod
    def from_jsonl_text(cls, value: Any) -> "BridgeInputBatch":
        if isinstance(value, cls):
            return value
        collection = BridgeInputMessageCollection.from_value(value)
        if collection.items:
            return cls(messages=collection.to_list())
        if not isinstance(value, str):
            return cls()
        messages = []
        for line in BridgeTextLines.from_text(value, strip=False).lines:
            message = BridgeInputMessage.from_json_line(line)
            if message:
                messages.append(message)
        return cls(messages=messages)

    def to_dicts(self) -> list[dict[str, Any]]:
        return [message.to_dict() for message in self.messages]


class BridgeInputFileDelta(BridgeModel):
    """Typed view of a newly-read slice from the Factorio chat input JSONL file."""

    previous_size: int = 0
    current_size: int = 0
    text: str = ""
    batch: BridgeInputBatch = Field(default_factory=BridgeInputBatch)

    @field_validator("previous_size", "current_size", mode="before")
    @classmethod
    def _coerce_size(cls, value: Any) -> int:
        try:
            size = int(value)
        except (TypeError, ValueError):
            return 0
        return max(0, size)

    @field_validator("text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value if value is not None else "")

    @field_validator("batch", mode="before")
    @classmethod
    def _coerce_batch(cls, value: Any) -> BridgeInputBatch:
        return BridgeInputBatch.from_jsonl_text(value)

    @classmethod
    def from_chunk(
        cls,
        *,
        previous_size: Any,
        current_size: Any,
        text: Any,
    ) -> "BridgeInputFileDelta":
        return cls(
            previous_size=previous_size,
            current_size=current_size,
            text=text,
            batch=BridgeInputBatch.from_jsonl_text(text),
        )

    @classmethod
    def empty(cls, *, previous_size: Any = 0, current_size: Any = 0) -> "BridgeInputFileDelta":
        return cls(previous_size=previous_size, current_size=current_size)

    @property
    def advanced(self) -> bool:
        return self.current_size > self.previous_size

    @property
    def next_size(self) -> int:
        return self.current_size if self.advanced else self.previous_size

    @property
    def messages(self) -> list[BridgeInputMessage]:
        return list(self.batch.messages)

    def to_dicts(self) -> list[dict[str, Any]]:
        return self.batch.to_dicts()


class AutonomyTickMessage(BridgeModel):
    """Typed synthetic input message generated by the bridge autonomy loop."""

    message: str
    player_index: int = 0
    player_name: str = "autonomy"
    autonomy: bool = True
    read_only_tools: bool = False
    model: str | None = None

    @field_validator("message", mode="before")
    @classmethod
    def _coerce_message(cls, value: Any) -> str:
        text = str(value).strip() if value is not None else ""
        if not text:
            raise ValueError("message is required")
        return text

    @field_validator("player_index", mode="before")
    @classmethod
    def _coerce_player_index(cls, value: Any) -> int:
        try:
            index = int(value)
        except (TypeError, ValueError):
            return 0
        return index if index >= 0 else 0

    @field_validator("player_name", mode="before")
    @classmethod
    def _coerce_player_name(cls, value: Any) -> str:
        return _input_text(value, "autonomy")

    @field_validator("autonomy", "read_only_tools", mode="before")
    @classmethod
    def _coerce_flag(cls, value: Any) -> bool:
        return _input_bool(value)

    @field_validator("model", mode="before")
    @classmethod
    def _coerce_optional_model(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @classmethod
    def create(
        cls,
        message: Any,
        *,
        read_only_tools: bool = False,
        model: str | None = None,
    ) -> "AutonomyTickMessage":
        return cls(
            message=message,
            read_only_tools=read_only_tools,
            model=model if read_only_tools else None,
        )

    def __getitem__(self, key: str) -> Any:
        return self.to_dict()[key]

    def __contains__(self, key: object) -> bool:
        return isinstance(key, str) and key in self.to_dict()

    def get(self, key: str, default: Any = None) -> Any:
        return self.to_dict().get(key, default)

    def to_bridge_input(self) -> BridgeInputMessage:
        message = BridgeInputMessage.from_mapping(self.to_dict())
        if message is None:
            raise BridgeValidationError("autonomy_tick", "expected bridge input message")
        return message

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "message": self.message,
            "player_index": self.player_index,
            "player_name": self.player_name,
            "autonomy": True,
        }
        if self.read_only_tools:
            result["read_only_tools"] = True
        if self.model is not None:
            result["model"] = self.model
        return result


class SdkSkillConfig(BridgeModel):
    """Normalized Claude SDK skill configuration used by bridge launch code."""

    skills: list[str] = Field(default_factory=list)
    all_skills: bool = False

    ENV_FIELDS: ClassVar[tuple["BridgeRuntimeEnvField", ...]] = ()

    @field_validator("skills", mode="before")
    @classmethod
    def _coerce_skills(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @classmethod
    def resolve(
        cls,
        value: Any = None,
        *,
        default: Any = None,
    ) -> "SdkSkillConfig":
        if isinstance(value, cls):
            return value
        if value is None:
            value = default
        if isinstance(value, cls):
            return value
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            return cls(skills=value)

        raw = str(value or "").strip()
        if not raw:
            return cls()
        lowered = raw.lower()
        if lowered in {"0", "false", "no", "none", "off", "disabled"}:
            return cls()
        if lowered == "all":
            return cls(all_skills=True)
        return cls(skills=CommaSeparatedItems.from_value(raw).to_list())

    @classmethod
    def from_env(
        cls,
        env: Any,
        *,
        value: Any = None,
        default: Any = None,
    ) -> "SdkSkillConfig":
        if isinstance(env, cls) and value is None:
            return env
        if value is None:
            data = BridgeRuntimeEnvField.read_source(env, cls.env_fields())
            value = data.get("skills")
        return cls.resolve(value, default=default)

    @classmethod
    def env_fields(cls) -> tuple["BridgeRuntimeEnvField", ...]:
        # Initialized after BridgeRuntimeEnvField is defined.
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="sdk_skill_env_fields",
        )

    @property
    def enabled(self) -> bool:
        return self.all_skills or bool(self.skills)

    @property
    def sdk_value(self) -> list[str] | str:
        return "all" if self.all_skills else list(self.skills)

    @property
    def claude_tools(self) -> list[str]:
        # The SDK documents `skills=` as auto-configuring the Skill tool, but
        # the Claude Code init stream used by this bridge still reports
        # `skill_tool=no` without this explicit entry. Keep the explicit tool
        # until the live init payload proves the native path works here.
        return ["Skill"] if self.enabled else []

    @property
    def setting_sources(self) -> list[str]:
        return ["project", "local"] if self.enabled else ["local"]

    @property
    def requires_factorio_control(self) -> bool:
        return self.all_skills or "factorio-control" in self.skills


class AgentProfileSdkSkills(BridgeModel):
    """Strict profile-file SDK skill value."""

    value: str | list[str] | None = None

    @field_validator("value", mode="before")
    @classmethod
    def _coerce_value(cls, value: Any) -> str | list[str] | None:
        if value is None:
            return None
        if isinstance(value, str):
            return value
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            items = list(value)
            if all(isinstance(item, str) for item in items):
                return CommaSeparatedItems.from_value(items).to_list()
        raise ValueError("expected string or list of strings")

    @classmethod
    def from_value(cls, value: Any) -> "AgentProfileSdkSkills":
        if isinstance(value, cls):
            return value
        return cls(value=value)

    def to_profile_value(self) -> str | list[str] | None:
        return self.value


class RawLuaPolicy(BridgeModel):
    """Typed bridge policy for exposing raw Lua execution to the SDK."""

    allow_raw_lua: bool = False

    ENV_FIELDS: ClassVar[tuple["BridgeRuntimeEnvField", ...]] = ()
    TRUE_VALUES: ClassVar[frozenset[str]] = frozenset({"1", "true", "yes", "on"})
    EXECUTE_LUA_TOOL: ClassVar[str] = "mcp__factorioctl__execute_lua"

    @field_validator("allow_raw_lua", mode="before")
    @classmethod
    def _coerce_allow_raw_lua(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return str(value or "").strip().lower() in cls.TRUE_VALUES

    @classmethod
    def from_env(cls, env: Any) -> "RawLuaPolicy":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple["BridgeRuntimeEnvField", ...]:
        # Initialized after BridgeRuntimeEnvField is defined.
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="raw_lua_env_fields",
        )

    @property
    def disallowed_tools(self) -> list[str]:
        return [] if self.allow_raw_lua else [self.EXECUTE_LUA_TOOL]
