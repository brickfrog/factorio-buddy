"""Shared reusable build recipes for bridge procedural memory."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any, Iterable

from pydantic import Field, ValidationError, field_validator

from runtime_paths import read_candidates, state_file
from models.base import BridgeModel, BridgeValidationError, CommaSeparatedItems, _json_object_from_text
from models.rcon_models import HiddenTrailerBlock
from models.tool_schema import _coerce_str_or_list


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
        from ledger import HiddenTrailerBodyLine

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


MAX_SKILLS = 50

STARTER_SKILLS = (
    SkillDefinition(
        name="build_burner_mining_setup",
        params=["resource_name", "output_pos"],
        steps=[
            "find_nearest_resource for resource_name",
            "call execute_edge_miner with dry_run=true for resource_name near the patch center; execute it with dry_run=false when success/ready are true",
            "call execute_direct_smelter with dry_run=true using the returned drill output tile before hand-authoring belt/inserter/furnace geometry; execute it when the plan is ready",
            "if no inserter exists and first smelting is blocked only by fuel/ore insertion, call bootstrap_smelting_once exactly once to produce first plates or a burner-inserter, then stop using bootstrap and build durable fuel/output automation",
            "route coal to the burner mining drill with belt/inserter/chest fuel supply when coal is nearby; use temporary fuel inserts only for bootstrap recovery",
            "call repair_fuel_sustainability near the drill after bootstrap fuel; if it cannot select a repair, inspect diagnose_fuel_sustainability and execute build_fuel_supply with returned build_fuel_supply_args",
            "verify_production for the mined resource",
        ],
        outcome="burner drill produces the requested resource onto the output",
    ),
    SkillDefinition(
        name="lay_smelting_line",
        params=["ore_belt_pos", "furnace_count"],
        steps=[
            "call execute_direct_smelter when starting from a single drill output tile; use dry_run=true for planning and dry_run=false to build the cell",
            "if missing burner-inserter creates a circular first-plate deadlock, call bootstrap_smelting_once exactly once, optionally with craft_recipe=burner-inserter, then immediately use the new inserter in durable fuel/output automation",
            "check_placement for furnace_count stone furnaces beside ore_belt_pos",
            "place_entity stone-furnace in a straight line",
            "get_machine_belt_positions for the furnaces",
            "route_belt ore past the input side of the furnaces",
            "place burner-inserters with correct facing from the ore belt into each furnace until electric inserter is unlocked",
            "route_belt plates past the output side of the furnaces",
            "place burner-inserters with correct facing from each furnace to the plates belt until electric inserter is unlocked",
            "route a coal fuel belt or chest-fed inserter to furnaces when coal production exists; use temporary fuel inserts only to restart a stalled bootstrap line",
            "call repair_fuel_sustainability around the smelting line; if it cannot select a repair, inspect diagnose_fuel_sustainability and execute build_fuel_supply with returned build_fuel_supply_args before treating the line as automated",
            "call plan_machine_output for furnace unit numbers to derive ready_to_call build_assembler_output args; execute build_assembler_output to move iron-plate/copper-plate output onto belts before building assembler feeds",
            "verify_production for iron-plate or copper-plate",
        ],
        outcome="iron or copper plates move on the output belt",
    ),
    SkillDefinition(
        name="build_automation_science",
        params=["assembler_pos", "gear_belt_pos", "copper_belt_pos", "science_belt_pos"],
        steps=[
            "call execute_entity_placement_near with dry_run=true for assembling-machine-1 near assembler_pos; execute it with dry_run=false and use placed_unit_number",
            "call execute_entity_placement_near with dry_run=true for lab near science_belt_pos when no lab exists; execute it with dry_run=false and use placed_unit_number",
            "if no iron-gear-wheel belt exists, place a gear assembler and call plan_recipe_assembler_cell for recipe=iron-gear-wheel input_item_name=iron-plate output_item_name=iron-gear-wheel; execute build_recipe_assembler_cell with ready_to_call.execute_args",
            "call set_recipe automation-science-pack on the assembler placed_unit_number",
            "call plan_automation_science using the assembler unit, lab unit, gear_belt_pos, and copper_belt_pos; execute build_automation_science with ready_to_call.execute_args when all routes are viable",
            "call build_assembler_feed with dry_run=true for iron-gear-wheel from gear_belt_pos into the assembler; execute it when route.materials_sufficient is true",
            "call build_assembler_feed with dry_run=true for copper-plate from copper_belt_pos into the assembler; execute it when route.materials_sufficient is true",
            "call plan_machine_output for automation-science-pack from the assembler toward science_belt_pos; execute build_assembler_output with ready_to_call.execute_args when route.materials_sufficient is true",
            "call build_lab_feed with dry_run=true from science_belt_pos to the lab pickup tile; execute it when the route is ready",
            "verify_production around the assembler and get_research_status after science reaches the lab",
        ],
        outcome="automation science packs are assembled from belt-fed inputs and delivered to labs",
    ),
    SkillDefinition(
        name="feed_lab",
        params=["lab_pos", "science_belt_pos"],
        steps=[
            "call execute_entity_placement_near with dry_run=true for lab near lab_pos; execute it with dry_run=false and use placed_unit_number",
            "route_belt automation-science-pack to science_belt_pos",
            "call build_lab_feed with dry_run=true for the science belt pickup and lab inserter; execute it when the route is ready",
            "call feed_lab_from_inventory with dry_run=true only when belt supply is not ready; execute its returned guarded feed step as bootstrap, not as research automation",
            "verify_production for research progress",
        ],
        outcome="lab consumes science packs and advances research",
    ),
    SkillDefinition(
        name="build_steam_power",
        params=["water_pos", "target_pos"],
        steps=[
            "get_recipes_for_item for offshore-pump, boiler, and steam-engine before guessing recipe names; if enabled=false, unlock the listed technology before crafting",
            "craft offshore-pump, boiler, steam-engine, small-electric-pole, and pipe as needed",
            "call plan_steam_power with a water bounding box around water_pos and target_pos; do not place pump, boiler, or steam-engine until the returned plan has no placement blockers",
            "place offshore-pump, boiler, steam-engine, pipes, and small-electric-poles using the returned place_args instead of hand-authored coordinates",
            "temporarily fuel the returned fuel_target boiler only to energize bootstrap; then route coal to the boiler before treating power as complete",
            "call repair_fuel_sustainability around the boiler; if it cannot select a repair, inspect diagnose_fuel_sustainability and execute build_fuel_supply with returned build_fuel_supply_args before treating power as complete",
            "call extend_power_to for long pole runs from existing power to target_pos; place returned pole steps instead of hand-authored coordinates",
            "call diagnose_steam_power and get_power_status before moving or rebuilding any power components",
            "if diagnose_steam_power reports an existing plant issue, call repair_steam_power and execute its repair_steps before attempting a rebuild",
        ],
        outcome="steam engine produces electricity and powers the target",
    ),
)


def _skills_file() -> Path:
    return state_file(".skills.json")


def _skills_read_files() -> tuple[Path, ...]:
    primary = _skills_file()
    candidates = [primary]
    candidates.extend(path for path in read_candidates(".skills.json") if path not in candidates)
    return tuple(candidates)


def _starter_library_model() -> SkillLibrary:
    return SkillLibrary(skills=list(STARTER_SKILLS))


def load_library_model() -> SkillLibrary:
    starters = _starter_library_model()
    saved = None
    for path in _skills_read_files():
        try:
            saved = SkillLibrary.from_file_text(
                path.read_text(),
                max_skills=MAX_SKILLS,
            )
            break
        except (BridgeValidationError, OSError):
            continue
    if saved is None:
        return starters
    if not saved.skills:
        return starters
    return starters.merged_with(saved, max_skills=MAX_SKILLS)


def save_library_model(library: dict | SkillLibrary) -> None:
    path = _skills_file()
    tmp = path.with_name(path.name + ".tmp")
    payload = SkillLibrary.normalized(library, max_skills=MAX_SKILLS).to_json_line()
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[skills] WARNING: failed to persist skill library: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def parse_skill_trailer_model(
    source: str | SkillDefinition | SkillDefinitionDraft,
) -> SkillDefinition | None:
    return SkillDefinition.from_trailer_text(source)


def apply_skill_update_model(source: str | SkillDefinition | SkillDefinitionDraft) -> SkillLibrary:
    skill = parse_skill_trailer_model(source)
    current = load_library_model()
    if not skill:
        return current

    library = current.replace_or_append(skill, max_skills=MAX_SKILLS)
    save_library_model(library)
    return library


def strip_skill_trailer(text: str) -> str:
    return SkillDefinition.strip_trailer_text(text)


def render_skills(library: dict | SkillLibrary) -> str:
    return SkillLibrary.normalized(library, max_skills=MAX_SKILLS).render_prompt()


def get_skill_model(library: dict | SkillLibrary, name: str) -> SkillDefinition | None:
    return SkillLibrary.normalized(library, max_skills=MAX_SKILLS).get(name)
