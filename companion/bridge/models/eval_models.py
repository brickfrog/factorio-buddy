from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'rcon_models')

class EvalProductionSnapshot(BridgeModel):
    produced: dict[str, float] = Field(default_factory=dict)
    rate_per_min: dict[str, float] = Field(default_factory=dict)

    @staticmethod
    def as_float_map(value: Any) -> dict[str, float]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, float] = {}
        for key, raw in value.items():
            amount = _coerce_float(raw)
            if amount:
                result[str(key)] = amount
        return result

    @classmethod
    def coerce(cls, value: Any) -> "EvalProductionSnapshot":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls()
        return cls(
            produced=cls.as_float_map(value.get("produced")),
            rate_per_min=cls.as_float_map(value.get("rate_per_min")),
        )

    @classmethod
    def from_rcon_text(cls, value: Any) -> "EvalProductionSnapshot":
        try:
            payload = RconJsonResponse.parse_value(value)
        except BridgeValidationError:
            return cls()
        return cls.coerce(payload)

    def to_dict(self) -> dict[str, dict[str, float]]:
        return {
            "produced": dict(self.produced),
            "rate_per_min": dict(self.rate_per_min),
        }

    def any_produced(self, items: tuple[str, ...]) -> bool:
        return any(_coerce_float(self.produced.get(item)) > 0 for item in items)

    def rate_at_least(self, item: str, threshold: float) -> bool:
        return _coerce_float(self.rate_per_min.get(item)) >= _coerce_float(threshold)

    def score_source(self) -> dict[str, float]:
        return dict(self.rate_per_min or self.produced)

    def production_score(self, values: dict[str, float]) -> float:
        source = self.score_source()
        return sum(_coerce_float(source.get(item)) * value for item, value in values.items())


class EvalMilestoneKind(str, Enum):
    ANY_PRODUCED = "any_produced"
    RATE_AT_LEAST = "rate_at_least"


class EvalMilestoneSpec(BridgeModel):
    """Typed milestone rule for the production eval harness."""

    name: str
    kind: EvalMilestoneKind
    items: tuple[str, ...] = ()
    item: str = ""
    threshold: float = 0.0

    @field_validator("name", "item", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> EvalMilestoneKind:
        if isinstance(value, EvalMilestoneKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in EvalMilestoneKind:
                if normalized == kind.value:
                    return kind
        return EvalMilestoneKind.ANY_PRODUCED

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            items = [value]
        elif isinstance(value, (list, tuple)):
            items = list(value)
        else:
            items = []
        return tuple(str(item).strip() for item in items if str(item).strip())

    @field_validator("threshold", mode="before")
    @classmethod
    def _coerce_threshold(cls, value: Any) -> float:
        return _coerce_float(value)

    @classmethod
    def any_produced(cls, name: str, items: tuple[str, ...]) -> "EvalMilestoneSpec":
        return cls(name=name, kind=EvalMilestoneKind.ANY_PRODUCED, items=items)

    @classmethod
    def rate_at_least(cls, name: str, item: str, threshold: float) -> "EvalMilestoneSpec":
        return cls(
            name=name,
            kind=EvalMilestoneKind.RATE_AT_LEAST,
            item=item,
            threshold=threshold,
        )

    def reached(self, snapshot: Any) -> bool:
        typed_snapshot = EvalProductionSnapshot.coerce(snapshot)
        if self.kind == EvalMilestoneKind.RATE_AT_LEAST:
            return typed_snapshot.rate_at_least(self.item, self.threshold)
        return typed_snapshot.any_produced(self.items)


class EvalResult(BridgeModel):
    production_score: float = 0.0
    milestones: dict[str, bool] = Field(default_factory=dict)
    milestones_reached: int = 0

    @classmethod
    def create(cls, *, production_score: Any, milestones: dict[str, Any]) -> "EvalResult":
        normalized = {
            str(name): bool(reached)
            for name, reached in (milestones if isinstance(milestones, dict) else {}).items()
        }
        return cls(
            production_score=_coerce_float(production_score),
            milestones=normalized,
            milestones_reached=sum(1 for reached in normalized.values() if reached),
        )

    @classmethod
    def coerce(
        cls,
        value: Any,
        *,
        milestone_names: list[str] | tuple[str, ...] = (),
    ) -> "EvalResult":
        if isinstance(value, cls):
            return value
        data = value if isinstance(value, dict) else {}
        raw_milestones = data.get("milestones", {})
        milestones = {
            str(name): bool(reached)
            for name, reached in (
                raw_milestones if isinstance(raw_milestones, dict) else {}
            ).items()
        }
        for name in milestone_names:
            milestones.setdefault(str(name), False)
        return cls.create(
            production_score=data.get("production_score", 0.0),
            milestones=milestones,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "production_score": self.production_score,
            "milestones": dict(self.milestones),
            "milestones_reached": self.milestones_reached,
        }

    def is_better_than(self, other: "EvalResult | None") -> bool:
        return other is None or self.production_score > other.production_score


class PromptEvalScenario(BridgeModel):
    """Frozen prompt/tool-choice scenario for offline prompt regression tests."""

    name: str
    prompt_surface: str = "autonomy"
    input_text: str = ""
    expected_tools: tuple[str, ...] = ()
    expected_tool_prefix: tuple[str, ...] = ()
    forbidden_tools: tuple[str, ...] = ()
    required_text: tuple[str, ...] = ()
    forbidden_text: tuple[str, ...] = ()
    notes: str = ""

    @field_validator("name", "prompt_surface", "input_text", "notes", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator(
        "expected_tools",
        "expected_tool_prefix",
        "forbidden_tools",
        "required_text",
        "forbidden_text",
        mode="before",
    )
    @classmethod
    def _coerce_text_tuple(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            raw_items = [value]
        elif isinstance(value, (list, tuple)):
            raw_items = list(value)
        else:
            raw_items = []
        return tuple(str(item).strip() for item in raw_items if str(item).strip())

    @classmethod
    def coerce(cls, value: Any) -> "PromptEvalScenario":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls(name="")
        return cls.model_validate(value)

    def expected_behavior(self) -> str:
        parts: list[str] = []
        if self.expected_tool_prefix:
            parts.append(
                "first tool calls: " + " -> ".join(self.expected_tool_prefix)
            )
        if self.expected_tools:
            parts.append("must call: " + ", ".join(self.expected_tools))
        if self.forbidden_tools:
            parts.append("must not call: " + ", ".join(self.forbidden_tools))
        if self.required_text:
            parts.append("must mention: " + "; ".join(self.required_text))
        if self.forbidden_text:
            parts.append("must not mention: " + "; ".join(self.forbidden_text))
        return " | ".join(parts)

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "prompt_surface": self.prompt_surface,
            "input_text": self.input_text,
            "expected_tools": list(self.expected_tools),
            "expected_tool_prefix": list(self.expected_tool_prefix),
            "forbidden_tools": list(self.forbidden_tools),
            "required_text": list(self.required_text),
            "forbidden_text": list(self.forbidden_text),
            "notes": self.notes,
        }


class PromptEvalTranscript(BridgeModel):
    """Observed tool calls and text emitted by a candidate prompt/program."""

    tool_calls: tuple[str, ...] = ()
    text: str = ""

    @field_validator("tool_calls", mode="before")
    @classmethod
    def _coerce_tool_calls(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            raw_items = [value]
        elif isinstance(value, (list, tuple)):
            raw_items = list(value)
        else:
            raw_items = []
        return tuple(str(item).strip() for item in raw_items if str(item).strip())

    @field_validator("text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def coerce(cls, value: Any) -> "PromptEvalTranscript":
        if isinstance(value, cls):
            return value
        if isinstance(value, dict):
            return cls.model_validate(value)
        if isinstance(value, (list, tuple)):
            return cls(tool_calls=value)
        return cls(text=str(value or ""))

    def to_dict(self) -> dict[str, Any]:
        return {
            "tool_calls": list(self.tool_calls),
            "text": self.text,
        }


class PromptEvalScenarioResult(BridgeModel):
    scenario_name: str
    score: float = 0.0
    passed: bool = False
    missing_expected_tools: tuple[str, ...] = ()
    prefix_mismatches: tuple[str, ...] = ()
    forbidden_tools_seen: tuple[str, ...] = ()
    missing_required_text: tuple[str, ...] = ()
    forbidden_text_seen: tuple[str, ...] = ()

    @classmethod
    def create(
        cls,
        *,
        scenario_name: Any,
        checks: dict[str, bool],
        missing_expected_tools: list[str],
        prefix_mismatches: list[str],
        forbidden_tools_seen: list[str],
        missing_required_text: list[str],
        forbidden_text_seen: list[str],
    ) -> "PromptEvalScenarioResult":
        total = max(len(checks), 1)
        score = sum(1 for passed in checks.values() if passed) / total
        return cls(
            scenario_name=str(scenario_name or "").strip(),
            score=round(score, 6),
            passed=all(checks.values()) if checks else True,
            missing_expected_tools=tuple(missing_expected_tools),
            prefix_mismatches=tuple(prefix_mismatches),
            forbidden_tools_seen=tuple(forbidden_tools_seen),
            missing_required_text=tuple(missing_required_text),
            forbidden_text_seen=tuple(forbidden_text_seen),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "scenario_name": self.scenario_name,
            "score": self.score,
            "passed": self.passed,
            "missing_expected_tools": list(self.missing_expected_tools),
            "prefix_mismatches": list(self.prefix_mismatches),
            "forbidden_tools_seen": list(self.forbidden_tools_seen),
            "missing_required_text": list(self.missing_required_text),
            "forbidden_text_seen": list(self.forbidden_text_seen),
        }


class PromptEvalSuiteResult(BridgeModel):
    results: tuple[PromptEvalScenarioResult, ...] = ()
    score: float = 0.0
    passed: bool = True

    @classmethod
    def from_results(cls, results: list[PromptEvalScenarioResult]) -> "PromptEvalSuiteResult":
        score = (
            sum(result.score for result in results) / len(results)
            if results
            else 0.0
        )
        return cls(
            results=tuple(results),
            score=round(score, 6),
            passed=all(result.passed for result in results),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "score": self.score,
            "passed": self.passed,
            "results": [result.to_dict() for result in self.results],
        }
