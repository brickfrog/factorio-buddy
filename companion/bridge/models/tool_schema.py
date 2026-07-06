from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'tool_result', 'bridge_log')

def _mapping(value: Any, field_path: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BridgeValidationError(field_path, "expected object")
    return value


def _required_str(data: dict[str, Any], key: str) -> str:
    value = data.get(key)
    if not isinstance(value, str) or not value:
        raise BridgeValidationError(key, "expected non-empty string")
    return value


def _optional_str(data: dict[str, Any], key: str) -> str | None:
    value = data.get(key)
    if value is None:
        return None
    if not isinstance(value, str):
        raise BridgeValidationError(key, "expected string")
    return value


def _required_any_str(data: dict[str, Any], key: str) -> str:
    value = data.get(key)
    if not isinstance(value, str):
        raise BridgeValidationError(key, "expected string")
    return value


def _optional_int(data: dict[str, Any], key: str) -> int | None:
    value = data.get(key)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise BridgeValidationError(key, "expected integer")
    if value <= 0:
        raise BridgeValidationError(key, "expected positive integer")
    return value


def _optional_bool(data: dict[str, Any], key: str) -> bool | None:
    value = data.get(key)
    if value is None:
        return None
    if not isinstance(value, bool):
        raise BridgeValidationError(key, "expected boolean")
    return value


def _required_str_list(data: dict[str, Any], key: str) -> list[str]:
    value = data.get(key)
    if not isinstance(value, list):
        raise BridgeValidationError(key, "expected list of strings")
    result = []
    for index, item in enumerate(value):
        if not isinstance(item, str):
            raise BridgeValidationError(f"{key}[{index}]", "expected string")
        if item:
            result.append(item)
    return result


def _coerce_str_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)]


def _coerce_str_or_list(value: Any, *, max_items: int | None = None) -> list[str]:
    if isinstance(value, str):
        result = [value.strip()] if value.strip() else []
    elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
        result = [item.strip() for item in value if isinstance(item, str) and item.strip()]
    else:
        result = []
    if max_items is not None:
        return result[:max_items]
    return result


def _optional_str_list(data: dict[str, Any], key: str) -> list[str]:
    value = data.get(key, [])
    if not isinstance(value, list):
        raise BridgeValidationError(key, "expected list of strings")
    result = []
    for index, item in enumerate(value):
        if not isinstance(item, str):
            raise BridgeValidationError(f"{key}[{index}]", "expected string")
        if item:
            result.append(item)
    return result


def _optional_response_format(data: dict[str, Any]) -> "AgentResponseFormat | None":
    value = data.get("response_format")
    if value is None:
        return None
    try:
        return AgentResponseFormat.coerce(value)
    except BridgeValidationError:
        raise
    except (TypeError, ValueError, ValidationError) as exc:
        raise BridgeValidationError("response_format", "expected object") from exc


def _optional_sdk_skills(data: dict[str, Any]) -> str | list[str] | None:
    value = data.get("sdk_skills")
    if value is None:
        return None
    if isinstance(value, str):
        return value
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return list(value)
    raise BridgeValidationError("sdk_skills", "expected string or list of strings")


def _matches_tool_param_type(value: Any, expected_type: str) -> bool:
    if expected_type == TOOL_PARAM_STRING:
        return isinstance(value, str)
    if expected_type == TOOL_PARAM_NUMBER:
        return (isinstance(value, int | float) and not isinstance(value, bool))
    if expected_type == TOOL_PARAM_INTEGER:
        return isinstance(value, int) and not isinstance(value, bool)
    if expected_type == TOOL_PARAM_BOOLEAN:
        return isinstance(value, bool)
    if expected_type == TOOL_PARAM_OBJECT:
        return isinstance(value, dict)
    if expected_type == TOOL_PARAM_LIST:
        return isinstance(value, list)
    raise BridgeValidationError("<schema>", f"unknown parameter type {expected_type!r}")


def _coerce_tool_param_type_map(value: Any, field_path: str) -> dict[str, str]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise BridgeValidationError(field_path, "expected object")
    result: dict[str, str] = {}
    for raw_key, raw_type in value.items():
        if not isinstance(raw_key, str) or not raw_key.strip():
            raise BridgeValidationError(field_path, "expected non-empty string keys")
        key = raw_key.strip()
        if not isinstance(raw_type, str):
            raise BridgeValidationError(f"{field_path}.{key}", "expected parameter type string")
        expected_type = raw_type.strip()
        if expected_type not in TOOL_PARAM_TYPES:
            raise BridgeValidationError(
                f"{field_path}.{key}",
                f"unknown parameter type {expected_type!r}",
            )
        result[key] = expected_type
    return result


class ToolParamSchema(BridgeModel):
    required: dict[str, str] = Field(default_factory=dict)
    optional: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "ToolParamSchema":
        if value is None:
            return cls()
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            raise BridgeValidationError("tool_param_schema", "expected object")
        return cls(
            required=_coerce_tool_param_type_map(value.get("required"), "required"),
            optional=_coerce_tool_param_type_map(value.get("optional"), "optional"),
        )

    def validate_request(self, request: "ToolCallRequest") -> None:
        request.validate_params(required=self.required, optional=self.optional)


class ToolParamSchemaRegistry(BridgeModel):
    """Typed registry of Factorio MCP parameter schemas."""

    schemas: dict[str, ToolParamSchema] = Field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "ToolParamSchemaRegistry":
        if isinstance(value, cls):
            return value
        if value is None:
            return cls()
        if not isinstance(value, dict):
            raise BridgeValidationError("tool_param_schema_registry", "expected object")
        schemas: dict[str, ToolParamSchema] = {}
        for raw_name, raw_schema in value.items():
            if not isinstance(raw_name, str) or not raw_name.strip():
                raise BridgeValidationError(
                    "tool_param_schema_registry",
                    "expected non-empty string keys",
                )
            name = raw_name.strip()
            try:
                schemas[name] = ToolParamSchema.from_mapping(raw_schema)
            except BridgeValidationError as exc:
                raise BridgeValidationError(f"{name}: {exc.field_path}", exc.message) from exc
        return cls(schemas=schemas)

    def get(self, tool_name: Any) -> ToolParamSchema | None:
        return self.schemas.get(str(tool_name or ""))

    def validate_request(self, request: "ToolCallRequest") -> None:
        if not request.is_factorio_mcp_tool:
            return
        schema = self.get(request.short_name)
        if schema:
            schema.validate_request(request)


class ToolCallRequest(BridgeModel):
    tool_name: str
    tool_input: dict[str, Any] = Field(default_factory=dict)

    @field_validator("tool_name", mode="before")
    @classmethod
    def _coerce_tool_name(cls, value: Any) -> str:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("expected non-empty string")
        return value.strip()

    @field_validator("tool_input", mode="before")
    @classmethod
    def _coerce_tool_input(cls, value: Any) -> dict[str, Any]:
        if value is None:
            return {}
        if not isinstance(value, dict):
            raise ValueError("expected object")
        return dict(value)

    @classmethod
    def from_hook_input(cls, value: Any) -> "ToolCallRequest":
        if isinstance(value, cls):
            return value
        if isinstance(value, dict):
            data = value
        elif hasattr(value, "tool_name"):
            data = {
                "tool_name": getattr(value, "tool_name", None),
                "tool_input": getattr(value, "tool_input", {}),
            }
        else:
            raise BridgeValidationError("tool_call", "expected object")
        tool_input = data.get("tool_input", {})
        if tool_input is not None and not isinstance(tool_input, dict):
            raise BridgeValidationError("tool_input", "expected object")
        try:
            return cls(tool_name=data.get("tool_name"), tool_input=tool_input)
        except ValidationError as exc:
            for error in exc.errors():
                location = error.get("loc", ())
                if location == ("tool_name",):
                    raise BridgeValidationError(
                        "tool_name",
                        "expected non-empty string",
                    ) from exc
                if location == ("tool_input",):
                    raise BridgeValidationError("tool_input", "expected object") from exc
            raise BridgeValidationError("tool_call", "expected object") from exc

    @staticmethod
    def short_factorio_tool_name(tool_name: Any) -> str:
        name = str(tool_name or "")
        if name.startswith(FACTORIO_MCP_TOOL_PREFIX):
            return name[len(FACTORIO_MCP_TOOL_PREFIX):]
        return name

    @staticmethod
    def is_factorio_mcp_tool_name(tool_name: Any) -> bool:
        return str(tool_name or "").startswith(FACTORIO_MCP_TOOL_PREFIX)

    @staticmethod
    def is_mutating_factorio_tool_name(tool_name: Any) -> bool:
        return (
            ToolCallRequest.short_factorio_tool_name(tool_name)
            in FACTORIO_MUTATING_TOOLS
        )

    @staticmethod
    def is_read_only_factorio_tool_name(tool_name: Any) -> bool:
        return (
            ToolCallRequest.short_factorio_tool_name(tool_name)
            in FACTORIO_READ_ONLY_TOOLS
        )

    @property
    def short_name(self) -> str:
        return self.short_factorio_tool_name(self.tool_name)

    @property
    def is_factorio_mcp_tool(self) -> bool:
        return self.is_factorio_mcp_tool_name(self.tool_name)

    @property
    def is_mutating_factorio_tool(self) -> bool:
        return self.is_mutating_factorio_tool_name(self.tool_name)

    @property
    def is_read_only_factorio_tool(self) -> bool:
        return self.is_read_only_factorio_tool_name(self.tool_name)

    @property
    def is_read_only_dry_run(self) -> bool:
        if self.short_name != "feed_lab_from_inventory":
            return (
                self.short_name in FACTORIO_DRY_RUN_SAFE_MUTATING_TOOLS
                and self.tool_input.get("dry_run") is True
            )
        return self.tool_input.get("dry_run", True) is not False

    @property
    def is_manual_fuel_transfer(self) -> bool:
        if self.short_name not in {"hand_feed_furnace", "insert_items"}:
            return False
        item = str(self.tool_input.get("item") or "").strip().lower()
        inventory_type = str(self.tool_input.get("inventory_type") or "").strip().lower()
        return inventory_type == "fuel" or item in {
            "coal",
            "wood",
            "solid-fuel",
            "rocket-fuel",
            "nuclear-fuel",
        }

    @property
    def is_manual_science_transfer(self) -> bool:
        item = str(self.tool_input.get("item") or "").strip().lower()
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        science_pack = str(self.tool_input.get("science_pack") or "").strip().lower()
        if self.short_name == "feed_lab_from_inventory":
            return self.tool_input.get("dry_run", True) is False
        if self.short_name == "craft":
            return recipe.endswith("-science-pack") or recipe == "automation-science-pack"
        if self.short_name in {"extract_items", "insert_items"}:
            return (
                item.endswith("-science-pack")
                or science_pack.endswith("-science-pack")
                or item == "automation-science-pack"
                or science_pack == "automation-science-pack"
            )
        return False

    @property
    def is_manual_material_transfer(self) -> bool:
        item = str(self.tool_input.get("item") or "").strip().lower()
        inventory_type = str(self.tool_input.get("inventory_type") or "").strip().lower()
        if self.short_name == "hand_feed_furnace":
            return True
        if self.short_name == "insert_items":
            return inventory_type == "furnace_source" and item in {
                "iron-ore",
                "copper-ore",
                "stone",
            }
        if self.short_name == "extract_items":
            return inventory_type == "furnace_result" and item in {
                "iron-plate",
                "copper-plate",
                "steel-plate",
            }
        return False

    @property
    def is_manual_component_craft(self) -> bool:
        if self.short_name != "craft":
            return False
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        return recipe in {
            "iron-gear-wheel",
            "copper-cable",
            "electronic-circuit",
        }

    @property
    def is_bootstrap_infrastructure_craft(self) -> bool:
        if self.short_name != "craft":
            return False
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        return recipe in {
            "assembling-machine-1",
            "burner-inserter",
            "copper-cable",
            "electronic-circuit",
            "inserter",
            "iron-gear-wheel",
            "small-electric-pole",
            "transport-belt",
        }

    def validate_params(
        self,
        *,
        schema: ToolParamSchema | dict[str, Any] | None = None,
        required: dict[str, str] | None = None,
        optional: dict[str, str] | None = None,
    ) -> None:
        if schema is not None:
            param_schema = ToolParamSchema.from_mapping(schema)
            required = param_schema.required
            optional = param_schema.optional
        required = required or {}
        optional = optional or {}
        for key, expected_type in required.items():
            if key not in self.tool_input:
                raise BridgeValidationError(f"tool_input.{key}", "missing required field")
            value = self.tool_input.get(key)
            if not _matches_tool_param_type(value, expected_type):
                raise BridgeValidationError(f"tool_input.{key}", f"expected {expected_type}")
        for key, expected_type in optional.items():
            if key not in self.tool_input or self.tool_input.get(key) is None:
                continue
            value = self.tool_input.get(key)
            if not _matches_tool_param_type(value, expected_type):
                raise BridgeValidationError(f"tool_input.{key}", f"expected {expected_type}")

    def to_dict(self) -> dict[str, Any]:
        return {
            "tool_name": self.tool_name,
            "tool_input": dict(self.tool_input),
        }


class PreToolUsePermissionDecision(str, Enum):
    ALLOW = "allow"
    DENY = "deny"


class PreToolUseGuardKind(str, Enum):
    PARALLEL_MUTATION = "parallel_mutation"
    READ_ONLY_TURN = "read_only_turn"
    PARAM_SCHEMA = "param_schema"
    SKILL_REQUIRED = "skill_required"
    MANUAL_AUTOMATION = "manual_automation"


class PreToolUseDecision(BridgeModel):
    """Typed Claude SDK PreToolUse hook response payload."""

    permission_decision: PreToolUsePermissionDecision
    reason: str = ""
    hook_event_name: str = "PreToolUse"

    @field_validator("permission_decision", mode="before")
    @classmethod
    def _coerce_permission_decision(
        cls,
        value: Any,
    ) -> PreToolUsePermissionDecision:
        if isinstance(value, PreToolUsePermissionDecision):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower()
            for decision in PreToolUsePermissionDecision:
                if normalized == decision.value:
                    return decision
        return PreToolUsePermissionDecision.DENY

    @field_validator("reason", "hook_event_name", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def allow(cls) -> "PreToolUseDecision":
        return cls(permission_decision=PreToolUsePermissionDecision.ALLOW)

    @classmethod
    def deny(cls, reason: Any) -> "PreToolUseDecision":
        return cls(
            permission_decision=PreToolUsePermissionDecision.DENY,
            reason=reason,
        )

    @property
    def is_denied(self) -> bool:
        return self.permission_decision == PreToolUsePermissionDecision.DENY

    def to_dict(self) -> dict[str, Any]:
        hook_output: dict[str, Any] = {
            "hookEventName": self.hook_event_name or "PreToolUse",
            "permissionDecision": self.permission_decision.value,
        }
        if self.reason:
            hook_output["permissionDecisionReason"] = self.reason
        result = {"hookSpecificOutput": hook_output}
        if self.is_denied:
            result["decision"] = "block"
            result["reason"] = self.reason
        return result


class PreToolUseHookResponse(BridgeModel):
    """Typed SDK PreToolUse hook response, including no-op pass-through."""

    decision: PreToolUseDecision | None = None

    @field_validator("decision", mode="before")
    @classmethod
    def _coerce_decision(cls, value: Any) -> PreToolUseDecision | None:
        if value is None or isinstance(value, PreToolUseDecision):
            return value
        if isinstance(value, PreToolUseGuardBlock):
            return value.to_decision()
        if isinstance(value, dict):
            hook_output = value.get("hookSpecificOutput")
            if isinstance(hook_output, dict):
                return PreToolUseDecision(
                    permission_decision=hook_output.get("permissionDecision"),
                    reason=(
                        value.get("reason")
                        or hook_output.get("permissionDecisionReason")
                        or ""
                    ),
                    hook_event_name=hook_output.get("hookEventName", "PreToolUse"),
                )
        return None

    @classmethod
    def noop(cls) -> "PreToolUseHookResponse":
        return cls()

    @classmethod
    def allow(cls) -> "PreToolUseHookResponse":
        return cls(decision=PreToolUseDecision.allow())

    @classmethod
    def block(cls, block: "PreToolUseGuardBlock") -> "PreToolUseHookResponse":
        return cls(decision=block.to_decision())

    @property
    def is_noop(self) -> bool:
        return self.decision is None

    def to_dict(self) -> dict[str, Any]:
        if self.decision is None:
            return {}
        return self.decision.to_dict()


class PreToolUseGuardBlock(BridgeModel):
    """Typed pre-tool-use guard block with operator-safe reason text."""

    kind: PreToolUseGuardKind
    tool_name: str = ""
    previous_tool_name: str = ""
    detail: str = ""
    elapsed_s: float = 0.0

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> PreToolUseGuardKind:
        if isinstance(value, PreToolUseGuardKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in PreToolUseGuardKind:
                if normalized == kind.value:
                    return kind
        return PreToolUseGuardKind.PARAM_SCHEMA

    @field_validator("tool_name", "previous_tool_name", "detail", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("elapsed_s", mode="before")
    @classmethod
    def _coerce_elapsed(cls, value: Any) -> float:
        try:
            return max(0.0, float(value))
        except (TypeError, ValueError):
            return 0.0

    @classmethod
    def parallel_mutation(
        cls,
        *,
        tool_name: Any,
        previous_tool_name: Any = "",
        elapsed_s: Any = 0.0,
    ) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.PARALLEL_MUTATION,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
            previous_tool_name=ToolCallRequest.short_factorio_tool_name(previous_tool_name),
            elapsed_s=elapsed_s,
        )

    @classmethod
    def read_only_turn(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.READ_ONLY_TURN,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @classmethod
    def param_schema(
        cls,
        *,
        detail: Any,
        tool_name: Any = "",
    ) -> "PreToolUseGuardBlock":
        short_name = ToolCallRequest.short_factorio_tool_name(tool_name)
        normalized_detail = str(detail or "").strip()
        if short_name and not normalized_detail.startswith(f"{short_name}:"):
            normalized_detail = f"{short_name}: {normalized_detail}"
        return cls(
            kind=PreToolUseGuardKind.PARAM_SCHEMA,
            tool_name=short_name,
            detail=normalized_detail,
        )

    @classmethod
    def skill_required(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.SKILL_REQUIRED,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @classmethod
    def manual_automation(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.MANUAL_AUTOMATION,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @property
    def reason(self) -> str:
        if self.kind == PreToolUseGuardKind.PARALLEL_MUTATION:
            return (
                f"{BRIDGE_PARALLEL_MUTATION_GUARD_PREFIX} {self.tool_name}. "
                "Wait for the previous mutating tool result before issuing "
                "another world/inventory-changing command."
            )
        if self.kind == PreToolUseGuardKind.READ_ONLY_TURN:
            return (
                f"{BRIDGE_READ_ONLY_TURN_GUARD_PREFIX} {self.tool_name}. "
                "This turn may only use read-only diagnostics; emit a ledger-only "
                "plan or reflection and stop."
            )
        if self.kind == PreToolUseGuardKind.SKILL_REQUIRED:
            return (
                f"{BRIDGE_SKILL_REQUIRED_GUARD_PREFIX} {self.tool_name}. "
                "Call Skill(factorio-control) before using Factorio MCP tools."
            )
        if self.kind == PreToolUseGuardKind.MANUAL_AUTOMATION:
            return (
                f"{BRIDGE_MANUAL_AUTOMATION_GUARD_PREFIX} {self.tool_name}. "
                "The active ledger plan is stale because it relies on manual "
                "transfer loops. Replace it with durable automation controllers "
                "such as bootstrap_smelting_once for first-inserter deadlocks, "
                "repair_fuel_sustainability, build_fuel_supply, execute_direct_smelter, "
                "plan_recipe_assembler_cell, build_recipe_assembler_cell, "
                "build_automation_science, build_assembler_feed, "
                "plan_machine_output, build_assembler_output for machine/furnace output belts, "
                "or build_lab_feed."
            )
        return f"{BRIDGE_PARAM_SCHEMA_GUARD_PREFIX} {self.detail}"

    @property
    def debug_message(self) -> str:
        if self.kind == PreToolUseGuardKind.PARALLEL_MUTATION:
            return (
                "blocked parallel mutating tool: "
                f"{self.tool_name} after {self.previous_tool_name} "
                f"in {self.elapsed_s:.3f}s"
            )
        if self.kind == PreToolUseGuardKind.READ_ONLY_TURN:
            return (
                "blocked non-read-only tool during planner/reflection turn: "
                f"{self.tool_name}"
            )
        if self.kind == PreToolUseGuardKind.SKILL_REQUIRED:
            return f"blocked Factorio MCP tool before skill: {self.tool_name}"
        if self.kind == PreToolUseGuardKind.MANUAL_AUTOMATION:
            return f"blocked stale manual automation tool: {self.tool_name}"
        if self.tool_name:
            return f"blocked invalid {self.tool_name} params: {self.detail}"
        return f"blocked malformed tool call hook input: {self.detail}"

    def to_decision(self) -> PreToolUseDecision:
        return PreToolUseDecision.deny(self.reason)

    def to_dict(self) -> dict[str, Any]:
        return self.to_decision().to_dict()


class WatchdogToolObservation(BridgeModel):
    """Typed view of one tool result as consumed by the tick watchdog."""

    tool_use_id: str = ""
    tool_name: str = ""
    classification: ToolResultClassification = ToolResultClassification.SDK_FAILURE
    text: str = ""
    indicates_progress: bool | None = None

    @field_validator("tool_use_id", "tool_name", "text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("classification", mode="before")
    @classmethod
    def _coerce_classification(cls, value: Any) -> ToolResultClassification:
        if isinstance(value, ToolResultClassification):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower()
            for classification in ToolResultClassification:
                if normalized == classification.value:
                    return classification
        return ToolResultClassification.SDK_FAILURE

    @classmethod
    def from_result(
        cls,
        *,
        tool_use_id: Any = None,
        tool_name: Any = None,
        classification: ToolResultClassification | str,
        text: Any = "",
        indicates_progress: bool | None = None,
    ) -> "WatchdogToolObservation":
        return cls(
            tool_use_id=tool_use_id,
            tool_name=tool_name,
            classification=classification,
            text=text,
            indicates_progress=indicates_progress,
        )

    @property
    def short_tool_name(self) -> str:
        return ToolCallRequest.short_factorio_tool_name(self.tool_name)

    @property
    def is_ok(self) -> bool:
        return self.classification == ToolResultClassification.OK

    @property
    def is_expected_miss(self) -> bool:
        return self.classification == ToolResultClassification.EXPECTED_MISS

    @property
    def is_game_rejected(self) -> bool:
        return self.classification == ToolResultClassification.GAME_REJECTED

    @property
    def is_mutating_tool(self) -> bool:
        return ToolCallRequest.is_mutating_factorio_tool_name(self.tool_name)

    def indicates_mutating_progress(
        self,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        if self.indicates_progress is not None:
            return self.indicates_progress
        return ToolResultOutcome.text_indicates_progress(
            self.text,
            text_is_error=text_is_error,
        )

    def failure_signature(self, *, limit: int = 300) -> str:
        return "|".join([
            self.short_tool_name,
            self.classification.value,
            " ".join(self.text.split())[:limit],
        ])
