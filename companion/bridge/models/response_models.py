from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'tool_schema', 'input_models', 'settings_models')

class ResponseFormatSection(RemotePayloadModel):
    label: str
    color: str = "0.5,0.7,0.5"
    description: str = ""

    @field_validator("label", mode="before")
    @classmethod
    def _coerce_label(cls, value: Any) -> str:
        text = _input_text(value)
        if not text:
            raise ValueError("section label is required")
        return text

    @field_validator("color", mode="before")
    @classmethod
    def _coerce_color(cls, value: Any) -> str:
        return _input_text(value, "0.5,0.7,0.5")

    @field_validator("description", mode="before")
    @classmethod
    def _coerce_description(cls, value: Any) -> str:
        return _input_text(value)

    @classmethod
    def coerce(cls, value: Any) -> "ResponseFormatSection | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result["label"] = self.label
        result["color"] = self.color
        if self.description:
            result["description"] = self.description
        return result


class ResponseFormatSectionCollection(BridgeModel):
    """Typed collection of response-format sections."""

    items: tuple[ResponseFormatSection, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[ResponseFormatSection, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        sections: list[ResponseFormatSection] = []
        for item in value:
            section = (
                item
                if isinstance(item, ResponseFormatSection)
                else ResponseFormatSection.coerce(item)
            )
            if section:
                sections.append(section)
        return tuple(sections)

    @classmethod
    def from_value(cls, value: Any) -> "ResponseFormatSectionCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[ResponseFormatSection]:
        return list(self.items)


class AgentResponseFormat(RemotePayloadModel):
    header_label: str = "STATUS"
    header_color: str = "1,0.8,0.2"
    action_label: str = "ACTIONS"
    action_color: str = "0.6,0.8,1"
    footer_label: str | None = None
    footer_color: str = "0.4,0.6,0.4"
    sections: list[ResponseFormatSection] = Field(default_factory=list)

    @field_validator(
        "header_label",
        "header_color",
        "action_label",
        "action_color",
        "footer_color",
        mode="before",
    )
    @classmethod
    def _coerce_required_text(cls, value: Any, info) -> str:
        defaults = {
            "header_label": "STATUS",
            "header_color": "1,0.8,0.2",
            "action_label": "ACTIONS",
            "action_color": "0.6,0.8,1",
            "footer_color": "0.4,0.6,0.4",
        }
        return _input_text(value, defaults[info.field_name])

    @field_validator("footer_label", mode="before")
    @classmethod
    def _coerce_footer_label(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @field_validator("sections", mode="before")
    @classmethod
    def _coerce_sections(cls, value: Any) -> list[ResponseFormatSection]:
        return ResponseFormatSectionCollection.from_value(value).to_list()

    @classmethod
    def coerce(cls, value: Any) -> "AgentResponseFormat | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "header_label": self.header_label,
            "header_color": self.header_color,
            "action_label": self.action_label,
            "action_color": self.action_color,
        })
        if self.footer_label is not None:
            result["footer_label"] = self.footer_label
        result["footer_color"] = self.footer_color
        if self.sections:
            result["sections"] = [section.to_dict() for section in self.sections]
        return result


class ParsedResponseSection(RemotePayloadModel):
    label: str = ""
    color: str = ""
    text: str = ""

    @field_validator("label", "color", "text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return _input_text(value)

    @classmethod
    def coerce(cls, value: Any) -> "ParsedResponseSection | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "label": self.label,
            "color": self.color,
            "text": self.text,
        })
        return result


class AnomalyEvidence(BridgeModel):
    """Typed interpretation of an agent ANOMALY section."""

    kind: AnomalyEvidenceKind = AnomalyEvidenceKind.EMPTY
    raw_text: str = ""
    normalized_text: str = ""

    NOMINAL_VALUES: ClassVar[frozenset[str]] = frozenset({
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
    })

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> AnomalyEvidenceKind:
        if isinstance(value, AnomalyEvidenceKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in AnomalyEvidenceKind:
                if normalized == kind.value:
                    return kind
        return AnomalyEvidenceKind.EMPTY

    @field_validator("raw_text", "normalized_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_meaningful(self) -> bool:
        return self.kind == AnomalyEvidenceKind.MEANINGFUL

    @classmethod
    def from_text(cls, value: Any) -> "AnomalyEvidence":
        raw = str(value or "").strip()
        normalized = cls.normalize_text(raw)
        if not normalized:
            kind = AnomalyEvidenceKind.EMPTY
        elif cls.is_nominal_normalized_text(normalized):
            kind = AnomalyEvidenceKind.NOMINAL
        else:
            kind = AnomalyEvidenceKind.MEANINGFUL
        return cls(kind=kind, raw_text=raw, normalized_text=normalized)

    @staticmethod
    def normalize_text(value: Any) -> str:
        normalized = re.sub(r"[^a-z0-9 ]+", "", str(value or "").strip().lower())
        return re.sub(r"\s+", " ", normalized)

    @classmethod
    def is_nominal_normalized_text(cls, normalized: str) -> bool:
        return (
            normalized in cls.NOMINAL_VALUES
            or normalized.startswith(("no anomaly", "no anomalies", "none ", "nominal"))
        )


class ParsedAgentResponse(BridgeModel):
    header: ParsedResponseSection | None = None
    body: str = ""
    actions: list[str] = Field(default_factory=list)
    footer: ParsedResponseSection | None = None
    data: dict[str, ParsedResponseSection] = Field(default_factory=dict)

    @field_validator("body", mode="before")
    @classmethod
    def _coerce_body(cls, value: Any) -> str:
        return _input_text(value)

    @field_validator("actions", mode="before")
    @classmethod
    def _coerce_actions(cls, value: Any) -> list[str]:
        return _coerce_str_list(value)

    @field_validator("header", "footer", mode="before")
    @classmethod
    def _coerce_section(cls, value: Any) -> ParsedResponseSection | None:
        return ParsedResponseSection.coerce(value)

    @field_validator("data", mode="before")
    @classmethod
    def _coerce_data(cls, value: Any) -> dict[str, ParsedResponseSection]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, ParsedResponseSection] = {}
        for key, item in value.items():
            section = ParsedResponseSection.coerce(item)
            if section:
                label = _input_text(key, section.label)
                if label:
                    if not section.label:
                        section = section.model_copy(update={"label": label})
                    result[label] = section
        return result

    @classmethod
    def body_only(cls, value: Any) -> "ParsedAgentResponse":
        return cls(body=str(value) if value is not None else "")

    @classmethod
    def from_text(cls, value: Any) -> "ParsedAgentResponse":
        text = str(value if value is not None else "")
        matches = list(_AGENT_RESPONSE_SECTION_RE.finditer(text))
        if not matches:
            return cls.body_only(text)

        header: ParsedResponseSection | None = None
        body = ""
        actions: list[str] = []
        footer: ParsedResponseSection | None = None
        data: dict[str, ParsedResponseSection] = {}

        for index, match in enumerate(matches):
            color = match.group(1)
            label = match.group(2).strip()
            content_start = match.end()
            content_end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
            content = text[content_start:content_end].strip()

            if index == 0:
                header_body = TextMarkerSplit.from_text(content, "\n\n")
                header = ParsedResponseSection(
                    label=label,
                    color=color,
                    text=header_body.before.strip(),
                )
                if header_body.matched and header_body.after.strip():
                    body = header_body.after.strip()
            elif "ACTION" in label.upper():
                for line in BridgeTextLines.from_text(content, keep_blank=False).lines:
                    item = line.strip().lstrip("- ").strip()
                    if item:
                        actions.append(item)
            elif label.upper() in {"FILED", "CLASSIFIED", "END"}:
                footer = ParsedResponseSection(label=label, color=color, text=content)
            else:
                data[label] = ParsedResponseSection(label=label, color=color, text=content)

        if not body:
            body = header.text if header else text

        return cls(
            header=header,
            body=body,
            actions=actions,
            footer=footer,
            data=data,
        )

    @staticmethod
    def sanitize_text(value: Any) -> str:
        """Remove markdown artifacts while preserving Factorio rich text tags."""
        text = str(value if value is not None else "")
        text = re.sub(r"\*\*(.+?)\*\*", r"\1", text)
        text = re.sub(r"^#{1,3}\s+", "", text, flags=re.MULTILINE)
        text = re.sub(r"```\w*\n?", "", text)
        return text.strip()

    @classmethod
    def from_mapping(cls, value: Any) -> "ParsedAgentResponse":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls.body_only(value)
        try:
            return cls.model_validate(value)
        except ValidationError:
            return cls.body_only(value)

    def anomaly_text(self) -> str:
        section = self.data.get("ANOMALY")
        return section.text if section else ""

    def anomaly_evidence(self) -> AnomalyEvidence:
        return AnomalyEvidence.from_text(self.anomaly_text())

    @staticmethod
    def normalized_anomaly_text(value: Any) -> str:
        return AnomalyEvidence.normalize_text(value)

    @classmethod
    def is_meaningful_anomaly_text(cls, value: Any) -> bool:
        return AnomalyEvidence.from_text(value).is_meaningful

    def meaningful_anomaly_text(self) -> str:
        evidence = self.anomaly_evidence()
        return evidence.raw_text if evidence.is_meaningful else ""

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {}
        if self.header:
            result["header"] = self.header.to_dict()
        if self.body or not any((self.header, self.actions, self.footer, self.data)):
            result["body"] = self.body
        if self.actions:
            result["actions"] = list(self.actions)
        if self.footer:
            result["footer"] = self.footer.to_dict()
        if self.data:
            result["data"] = {
                label: section.to_dict()
                for label, section in self.data.items()
            }
        return result


class AgentProfile(BridgeModel):
    name: str
    system_prompt: str
    model: str | None = None
    planner_model: str | None = None
    max_turns: int | None = None
    telemetry_name: str | None = None
    planet: str | None = None
    group: str | None = None
    heartbeat_interval: int | None = None
    planner_interval: int | None = None
    reflect_interval: int | None = None
    autonomy_requires_player: bool | None = None
    sdk_skills: str | list[str] | None = None
    response_format: AgentResponseFormat | None = None
    extra: dict[str, Any] = Field(default_factory=dict)

    @field_validator("sdk_skills", mode="before")
    @classmethod
    def _coerce_sdk_skills(cls, value: Any) -> str | list[str] | None:
        return AgentProfileSdkSkills.from_value(value).to_profile_value()

    @classmethod
    def from_mapping(cls, value: Any) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        data = _mapping(value, "agent")
        known = {
            "name",
            "system_prompt",
            "model",
            "planner_model",
            "max_turns",
            "telemetry_name",
            "planet",
            "group",
            "heartbeat_interval",
            "planner_interval",
            "reflect_interval",
            "autonomy_requires_player",
            "sdk_skills",
            "response_format",
        }
        extra = {key: item for key, item in data.items() if key not in known}
        try:
            return cls(
                name=_required_str(data, "name"),
                system_prompt=_required_str(data, "system_prompt"),
                model=_optional_str(data, "model"),
                planner_model=_optional_str(data, "planner_model"),
                max_turns=_optional_int(data, "max_turns"),
                telemetry_name=_optional_str(data, "telemetry_name"),
                planet=_optional_str(data, "planet"),
                group=_optional_str(data, "group"),
                heartbeat_interval=_optional_int(data, "heartbeat_interval"),
                planner_interval=_optional_int(data, "planner_interval"),
                reflect_interval=_optional_int(data, "reflect_interval"),
                autonomy_requires_player=_optional_bool(data, "autonomy_requires_player"),
                sdk_skills=data.get("sdk_skills"),
                response_format=_optional_response_format(data),
                extra=extra,
            )
        except ValidationError as exc:
            for error in exc.errors():
                loc = error.get("loc", ())
                if loc and loc[0] == "sdk_skills":
                    raise BridgeValidationError(
                        "sdk_skills",
                        "expected string or list of strings",
                    ) from exc
            raise

    @classmethod
    def coerce(cls, value: Any) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        return cls.from_mapping(value)

    @classmethod
    def from_file_text(cls, value: str) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "agent")
        return cls.from_mapping(data)

    @property
    def planet_name(self) -> str:
        return self.planet or "nauvis"

    @property
    def registration_label(self) -> str:
        return (self.planet or self.name).capitalize()

    def sort_key(self, planet_order: dict[str, int]) -> tuple[int, str]:
        return (planet_order.get(self.planet_name, 99), self.name)

    def with_system_prompt(self, system_prompt: str) -> "AgentProfile":
        return AgentProfile(
            name=self.name,
            system_prompt=system_prompt,
            model=self.model,
            planner_model=self.planner_model,
            max_turns=self.max_turns,
            telemetry_name=self.telemetry_name,
            planet=self.planet,
            group=self.group,
            heartbeat_interval=self.heartbeat_interval,
            planner_interval=self.planner_interval,
            reflect_interval=self.reflect_interval,
            autonomy_requires_player=self.autonomy_requires_player,
            sdk_skills=self.sdk_skills,
            response_format=self.response_format,
            extra=dict(self.extra),
        )

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.extra)
        result["name"] = self.name
        result["system_prompt"] = self.system_prompt
        optional = {
            "model": self.model,
            "planner_model": self.planner_model,
            "max_turns": self.max_turns,
            "telemetry_name": self.telemetry_name,
            "planet": self.planet,
            "group": self.group,
            "heartbeat_interval": self.heartbeat_interval,
            "planner_interval": self.planner_interval,
            "reflect_interval": self.reflect_interval,
            "autonomy_requires_player": self.autonomy_requires_player,
            "sdk_skills": self.sdk_skills,
            "response_format": self.response_format,
        }
        for key, value in optional.items():
            if value is not None:
                if key == "response_format":
                    result[key] = value.to_dict()
                else:
                    result[key] = value
        return result


class AgentNameSelection(BridgeModel):
    """Typed comma/list selector for explicit multi-agent startup."""

    names: list[str] = Field(default_factory=list)

    @field_validator("names", mode="before")
    @classmethod
    def _coerce_names(cls, value: Any) -> list[str]:
        return CommaSeparatedItems.from_value(value).to_list()

    @classmethod
    def from_cli_arg(cls, value: Any) -> "AgentNameSelection":
        return cls(names=value)

    @property
    def filter_or_none(self) -> list[str] | None:
        return list(self.names) if self.names else None


class AgentRuntimeConfig(BridgeModel):
    """Resolved per-agent runtime config after profile, CLI, and env overlays."""

    profile: AgentProfile
    model: str = "haiku"
    planner_model: str = "sonnet"
    max_turns: int = 200
    skill_config: SdkSkillConfig = Field(default_factory=SdkSkillConfig)
    telemetry_name: str
    heartbeat_interval: float = 0.0
    planner_interval: int = 5
    reflect_interval: int = 16
    autonomy_requires_player: bool = True

    @field_validator("model", "planner_model", "telemetry_name", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("max_turns", "planner_interval", "reflect_interval", mode="before")
    @classmethod
    def _coerce_positive_int(cls, value: Any, info) -> int:
        defaults = {
            "max_turns": 200,
            "planner_interval": 5,
            "reflect_interval": 16,
        }
        minimums = {
            "max_turns": 1,
            "planner_interval": 1,
            "reflect_interval": 1,
        }
        default = defaults[info.field_name]
        minimum = minimums[info.field_name]
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @field_validator("heartbeat_interval", mode="before")
    @classmethod
    def _coerce_heartbeat_interval(cls, value: Any) -> float:
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return 0.0
        return parsed if parsed >= 0.0 else 0.0

    @field_validator("autonomy_requires_player", mode="before")
    @classmethod
    def _coerce_bool(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return True

    @classmethod
    def from_sources(
        cls,
        profile: AgentProfile | dict[str, Any],
        *,
        cli_model: Any = None,
        cli_max_turns: Any = None,
        cli_sdk_skills: Any = None,
        default_sdk_skills: Any = None,
        heartbeat_interval: Any = 0.0,
        planner_interval: Any = 5,
        autonomy_requires_player: Any = True,
        runtime_settings: BridgeRuntimeSettings | None = None,
        env: Any = None,
    ) -> "AgentRuntimeConfig":
        resolved_profile = AgentProfile.coerce(profile)
        settings = runtime_settings or BridgeRuntimeSettings.from_env(env or {})
        max_turns_source = (
            cli_max_turns
            if cli_max_turns is not None
            else resolved_profile.max_turns
        )
        max_turns = (
            BridgeRuntimeSettings(max_turns=max_turns_source).max_turns
            if max_turns_source is not None
            else settings.max_turns
        )
        sdk_skill_value = (
            cli_sdk_skills
            if cli_sdk_skills is not None
            else resolved_profile.sdk_skills
        )
        return cls(
            profile=resolved_profile,
            model=cli_model or resolved_profile.model or "haiku",
            planner_model=resolved_profile.planner_model or "sonnet",
            max_turns=max_turns,
            skill_config=SdkSkillConfig.from_env(
                env or {},
                value=sdk_skill_value,
                default=default_sdk_skills,
            ),
            telemetry_name=resolved_profile.telemetry_name or resolved_profile.name,
            heartbeat_interval=(
                resolved_profile.heartbeat_interval
                if resolved_profile.heartbeat_interval is not None
                else heartbeat_interval
            ),
            planner_interval=(
                resolved_profile.planner_interval
                if resolved_profile.planner_interval is not None
                else planner_interval
            ),
            reflect_interval=(
                resolved_profile.reflect_interval
                if resolved_profile.reflect_interval is not None
                else 16
            ),
            autonomy_requires_player=(
                resolved_profile.autonomy_requires_player
                if resolved_profile.autonomy_requires_player is not None
                else autonomy_requires_player
            ),
        )

    @property
    def agent_name(self) -> str:
        return self.profile.name

    @property
    def system_prompt(self) -> str:
        return self.profile.system_prompt

    @property
    def planet_name(self) -> str:
        return self.profile.planet_name

    @property
    def sdk_skills(self) -> list[str] | str:
        return self.skill_config.sdk_value


class AgentInvocationConfig(BridgeModel):
    """Resolved config for one Claude SDK invocation."""

    agent_name: str = "default"
    telemetry_name: str | None = None
    response_to: str | None = None
    system_prompt: str
    session_id: str | None = None
    model: str | None = None
    max_turns: int = 200
    skill_config: SdkSkillConfig = Field(default_factory=SdkSkillConfig)
    read_only_tools: bool = False

    @field_validator(
        "agent_name",
        "telemetry_name",
        "response_to",
        "system_prompt",
        "session_id",
        "model",
        mode="before",
    )
    @classmethod
    def _coerce_optional_text(cls, value: Any, info) -> str | None:
        text = str(value or "").strip()
        if info.field_name in {"telemetry_name", "response_to", "session_id", "model"}:
            return text or None
        return text or "default"

    @field_validator("max_turns", mode="before")
    @classmethod
    def _coerce_max_turns(cls, value: Any) -> int:
        return BridgeRuntimeSettings(max_turns=value).max_turns

    @field_validator("read_only_tools", mode="before")
    @classmethod
    def _coerce_read_only(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return False

    @classmethod
    def from_sources(
        cls,
        *,
        system_prompt: Any,
        agent_name: Any = "default",
        telemetry_name: Any = None,
        response_to: Any = None,
        session_id: Any = None,
        model: Any = None,
        max_turns: Any = None,
        sdk_skills: Any = None,
        read_only_tools: Any = False,
        default_sdk_skills: Any = None,
        runtime_settings: BridgeRuntimeSettings | None = None,
        env: Any = None,
    ) -> "AgentInvocationConfig":
        settings = runtime_settings or BridgeRuntimeSettings.from_env(env or {})
        resolved_max_turns = (
            BridgeRuntimeSettings(max_turns=max_turns).max_turns
            if max_turns is not None
            else settings.max_turns
        )
        return cls(
            agent_name=agent_name,
            telemetry_name=telemetry_name,
            response_to=response_to,
            system_prompt=system_prompt,
            session_id=session_id,
            model=model,
            max_turns=resolved_max_turns,
            skill_config=SdkSkillConfig.from_env(
                env or {},
                value=sdk_skills,
                default=default_sdk_skills,
            ),
            read_only_tools=read_only_tools,
        )

    @property
    def telemetry_label(self) -> str:
        return self.telemetry_name or self.agent_name

    @property
    def rcon_target(self) -> str:
        return self.response_to or self.agent_name

    @property
    def sdk_skills(self) -> list[str] | str:
        return self.skill_config.sdk_value

    @property
    def resume_tag(self) -> str:
        return (
            f" (resume {self.session_id[:8]}...)"
            if self.session_id
            else " (new session)"
        )

    def to_sdk_options_spec(
        self,
        *,
        mcp_servers: Any,
        env: Any,
        project_root: Any,
    ) -> "AgentClaudeOptionsSpec":
        return AgentClaudeOptionsSpec(
            system_prompt=self.system_prompt,
            model=self.model,
            max_turns=self.max_turns,
            mcp_servers=mcp_servers,
            tools=self.skill_config.claude_tools,
            disallowed_tools=RawLuaPolicy.from_env(env).disallowed_tools,
            permission_mode="bypassPermissions",
            resume=self.session_id,
            setting_sources=self.skill_config.setting_sources,
            cwd=project_root,
            skills=self.sdk_skills,
        )


class AgentClaudeOptionsSpec(BridgeModel):
    """Typed bridge-owned spec for constructing ClaudeAgentOptions."""

    system_prompt: str
    model: str | None = None
    max_turns: int = 200
    mcp_servers: Any = Field(default_factory=dict)
    strict_mcp_config: bool = True
    tools: list[str] = Field(default_factory=list)
    disallowed_tools: list[str] = Field(default_factory=list)
    permission_mode: str = "bypassPermissions"
    resume: str | None = None
    setting_sources: list[str] = Field(default_factory=list)
    cwd: str
    skills: list[str] | str | None = None

    @field_validator("system_prompt", "permission_mode", mode="before")
    @classmethod
    def _coerce_required_text(cls, value: Any, info) -> str:
        text = str(value or "").strip()
        if text:
            return text
        return "default" if info.field_name == "system_prompt" else "bypassPermissions"

    @field_validator("model", "resume", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("max_turns", mode="before")
    @classmethod
    def _coerce_max_turns(cls, value: Any) -> int:
        return BridgeRuntimeSettings(max_turns=value).max_turns

    @field_validator("strict_mcp_config", mode="before")
    @classmethod
    def _coerce_strict_mcp_config(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"0", "false", "no", "off"}:
            return False
        return True

    @field_validator("tools", "disallowed_tools", "setting_sources", mode="before")
    @classmethod
    def _coerce_string_list(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("cwd", mode="before")
    @classmethod
    def _coerce_cwd(cls, value: Any) -> str:
        text = str(value or "").strip()
        return text or "."


class AgentMessageResult(BridgeModel):
    """Typed result for a handled agent message."""

    session_id: str | None = None
    reset_session: bool = False

    @field_validator("session_id", mode="before")
    @classmethod
    def _coerce_session_id(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("reset_session", mode="before")
    @classmethod
    def _coerce_reset_session(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return False

    @classmethod
    def keep_session(cls, session_id: Any = None) -> "AgentMessageResult":
        return cls(session_id=session_id, reset_session=False)

    @classmethod
    def reset(cls) -> "AgentMessageResult":
        return cls(session_id=None, reset_session=True)

    def to_legacy_session_value(self, reset_token: str) -> str | None:
        return reset_token if self.reset_session else self.session_id
