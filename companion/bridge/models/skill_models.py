from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'journal_models')

class SkillDefinitionDraft(BridgeModel):
    """Typed intermediate shape parsed from a legacy <skill> trailer body."""

    name: str = ""
    params: list[str] = Field(default_factory=list)
    steps: list[str] = Field(default_factory=list)
    outcome: str = ""

    @field_validator("name", "outcome", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @field_validator("params", "steps", mode="before")
    @classmethod
    def _coerce_string_list(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @classmethod
    def from_body(cls, body: Any) -> "SkillDefinitionDraft":
        if isinstance(body, SkillDefinition):
            return cls(
                name=body.name,
                params=list(body.params),
                steps=list(body.steps),
                outcome=body.outcome,
            )
        if isinstance(body, cls):
            return body
        data: dict[str, Any] = {
            "name": "",
            "params": [],
            "steps": [],
            "outcome": "",
        }
        active_key: str | None = None
        for line in HiddenTrailerBodyLine.iter_body(body):
            if line.key_is("name"):
                data["name"] = line.value
                active_key = None
            elif line.key_is("params"):
                active_key = "params"
                data["params"].extend(cls._parse_inline_items(line.value))
            elif line.key_is("steps"):
                active_key = "steps"
            elif line.key_is("outcome"):
                data["outcome"] = line.value
                active_key = None
            elif active_key in {"params", "steps"} and line.is_bullet:
                data[active_key].append(line.bullet)
        return cls.model_validate(data)

    @staticmethod
    def _parse_inline_items(value: Any) -> list[str]:
        return CommaSeparatedItems.from_value(value).to_list()

    def to_skill(self) -> "SkillDefinition | None":
        return SkillDefinition.coerce(self.to_dict())

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "params": list(self.params),
            "steps": list(self.steps),
            "outcome": self.outcome,
        }


class SkillDefinition(SkillDefinitionDraft):
    name: str

    @field_validator("name", mode="before")
    @classmethod
    def _validate_name(cls, value: Any) -> str:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("skill name is required")
        return value.strip()

    @classmethod
    def from_trailer_text(cls, text: Any) -> "SkillDefinition | None":
        if isinstance(text, cls):
            return text
        if isinstance(text, SkillDefinitionDraft):
            return text.to_skill()
        block = HiddenTrailerBlock.first_from_text(text, "skill")
        if not block:
            return None
        return SkillDefinitionDraft.from_body(block.body).to_skill()

    @classmethod
    def strip_trailer_text(cls, text: Any) -> str:
        return HiddenTrailerBlock.strip_from_text(text, ["skill"])

    @classmethod
    def coerce(cls, value: Any) -> "SkillDefinition | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "params": list(self.params),
            "steps": list(self.steps),
            "outcome": self.outcome,
        }

    def to_sparse_dict(self) -> dict[str, Any]:
        return {
            key: value
            for key, value in self.to_dict().items()
            if key == "name" or value not in ("", [])
        }

    def signature(self) -> str:
        return f"{self.name}({', '.join(self.params)})"

    def prompt_summary_line(self) -> str:
        return f"- {self.signature()} — {self.outcome}"


class SkillDefinitionCollection(BridgeModel):
    """Typed collection boundary for reusable skill definitions."""

    skills: tuple[SkillDefinition, ...] = ()

    @field_validator("skills", mode="before")
    @classmethod
    def _coerce_skills(cls, value: Any) -> tuple[SkillDefinition, ...]:
        if value is None:
            items: list[Any] = []
        elif isinstance(value, SkillDefinition):
            items = [value]
        elif isinstance(value, SkillDefinitionDraft):
            items = [value]
        elif isinstance(value, dict):
            items = value.get("skills", [value])
        elif isinstance(getattr(value, "skills", None), Iterable) and not isinstance(
            getattr(value, "skills", None),
            (str, bytes, dict),
        ):
            items = list(value.skills)
        elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            items = list(value)
        else:
            items = []

        skills: list[SkillDefinition] = []
        for item in items:
            if isinstance(item, SkillDefinition):
                skill = item
            elif isinstance(item, SkillDefinitionDraft):
                skill = item.to_skill()
            else:
                skill = SkillDefinition.coerce(item)
            if skill:
                skills.append(skill)
        return tuple(skills)

    @classmethod
    def from_value(cls, value: Any) -> "SkillDefinitionCollection":
        if isinstance(value, cls):
            return value
        return cls(skills=value)

    def to_list(self) -> list[SkillDefinition]:
        return list(self.skills)


class SkillLibrary(BridgeModel):
    skills: list[SkillDefinition] = Field(default_factory=list)

    @classmethod
    def from_file_text(cls, value: str, *, max_skills: int = 50) -> "SkillLibrary":
        if isinstance(value, cls):
            return cls.coerce(value, max_skills=max_skills)
        data = _json_object_from_text(value, "skills")
        return cls.coerce(data, max_skills=max_skills)

    @classmethod
    def coerce(cls, value: Any, *, max_skills: int = 50) -> "SkillLibrary":
        raw_skills = SkillDefinitionCollection.from_value(value).to_list()
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        skills: list[SkillDefinition] = []
        seen: set[str] = set()
        for skill in raw_skills:
            if skill.name in seen:
                skills = [existing for existing in skills if existing.name != skill.name]
            seen.add(skill.name)
            skills.append(skill)
            if limit and len(skills) >= limit:
                break
        return cls(skills=skills)

    @classmethod
    def normalized(cls, value: Any, *, max_skills: int = 50) -> "SkillLibrary":
        return cls.coerce(value, max_skills=max_skills)

    def merged_with(self, library: "SkillLibrary", *, max_skills: int = 50) -> "SkillLibrary":
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        merged = list(self.skills)
        positions = {skill.name: index for index, skill in enumerate(merged)}
        for skill in library.skills:
            if skill.name in positions:
                merged[positions[skill.name]] = skill
            else:
                positions[skill.name] = len(merged)
                merged.append(skill)
            if limit and len(merged) >= limit:
                break
        return SkillLibrary(skills=merged[:limit] if limit else [])

    def replace_or_append(
        self,
        skill: SkillDefinition,
        *,
        max_skills: int = 50,
        move_to_end: bool = True,
    ) -> "SkillLibrary":
        skills = [existing for existing in self.skills if existing.name != skill.name]
        if move_to_end:
            skills.append(skill)
        else:
            inserted = False
            for index, existing in enumerate(self.skills):
                if existing.name == skill.name:
                    skills.insert(index, skill)
                    inserted = True
                    break
            if not inserted:
                skills.append(skill)
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        return SkillLibrary(skills=skills[-limit:] if limit else [])

    def get(self, name: Any) -> SkillDefinition | None:
        if not isinstance(name, str):
            return None
        for skill in self.skills:
            if skill.name == name:
                return skill
        return None

    def render_prompt(self) -> str:
        if not self.skills:
            return ""
        lines = ["Available skills (reuse these recipes; follow the steps with your tools):"]
        lines.extend(skill.prompt_summary_line() for skill in self.skills)
        return "\n".join(lines)

    def to_dict(self) -> dict[str, Any]:
        return {"skills": [skill.to_dict() for skill in self.skills]}

    def to_json_line(self) -> str:
        return self.model_dump_json() + "\n"
