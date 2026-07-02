"""Shared reusable build recipes for bridge procedural memory."""

import os
from pathlib import Path

from models import (
    BridgeValidationError,
    SkillDefinition,
    SkillDefinitionDraft,
    SkillLibrary,
)


MAX_SKILLS = 50

STARTER_SKILLS = (
    SkillDefinition(
        name="build_burner_mining_setup",
        params=["resource_name", "output_pos"],
        steps=[
            "find_nearest_resource for resource_name",
            "call build_edge_miner for resource_name near the patch center; prefer its returned clear output placement steps over hand-authored drill coordinates",
            "call build_direct_smelter using the returned drill output tile before hand-authoring belt/inserter/furnace geometry",
            "check_placement for a burner mining drill on the resource patch",
            "place_entity burner-mining-drill facing output_pos",
            "check_placement for a chest or belt at the drill output",
            "place_entity chest or route_belt from the drill output toward output_pos",
            "insert_items coal into the burner mining drill",
            "verify_production for the mined resource",
        ],
        outcome="burner drill produces the requested resource onto the output",
    ),
    SkillDefinition(
        name="lay_smelting_line",
        params=["ore_belt_pos", "furnace_count"],
        steps=[
            "call build_direct_smelter when starting from a single drill output tile; execute its returned steps before inventing inserter directions",
            "check_placement for furnace_count stone furnaces beside ore_belt_pos",
            "place_entity stone-furnace in a straight line",
            "get_machine_belt_positions for the furnaces",
            "route_belt ore past the input side of the furnaces",
            "place burner-inserters with correct facing from the ore belt into each furnace until electric inserter is unlocked",
            "route_belt plates past the output side of the furnaces",
            "place burner-inserters with correct facing from each furnace to the plates belt until electric inserter is unlocked",
            "insert_items coal into each stone furnace or route fuel belt if available",
            "verify_production for iron-plate or copper-plate",
        ],
        outcome="iron or copper plates move on the output belt",
    ),
    SkillDefinition(
        name="feed_lab",
        params=["lab_pos", "science_belt_pos"],
        steps=[
            "check_placement for lab_pos and a belt-adjacent inserter; use burner-inserter if electric inserter is still locked",
            "place_entity lab at lab_pos",
            "route_belt automation-science-pack to science_belt_pos",
            "place an available inserter with correct facing from science_belt_pos into the lab",
            "call feed_lab_from_inventory with dry_run=true when belt supply is not ready; execute its returned guarded feed step before manual insert attempts",
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
            "insert_items fuel into the returned fuel_target boiler",
            "call extend_power_to for long pole runs from existing power to target_pos; place returned pole steps instead of hand-authored coordinates",
            "call diagnose_steam_power and get_power_status before moving or rebuilding any power components",
            "if diagnose_steam_power reports an existing plant issue, call repair_steam_power and execute its repair_steps before attempting a rebuild",
        ],
        outcome="steam engine produces electricity and powers the target",
    ),
)


def _skills_file() -> Path:
    return Path(__file__).resolve().parent / ".skills.json"


def _starter_library_model() -> SkillLibrary:
    return SkillLibrary(skills=list(STARTER_SKILLS))


def default_library() -> dict:
    return _starter_library_model().to_dict()


def load_library_model() -> SkillLibrary:
    starters = _starter_library_model()
    try:
        saved = SkillLibrary.from_file_text(
            _skills_file().read_text(),
            max_skills=MAX_SKILLS,
        )
    except (BridgeValidationError, OSError):
        return starters
    if not saved.skills:
        return starters
    return starters.merged_with(saved, max_skills=MAX_SKILLS)


def load_library() -> dict:
    return load_library_model().to_dict()


def save_library_model(library: dict | SkillLibrary) -> None:
    path = _skills_file()
    tmp = path.with_name(path.name + ".tmp")
    payload = SkillLibrary.normalized(library, max_skills=MAX_SKILLS).to_json_line()
    try:
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[skills] WARNING: failed to persist skill library: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def save_library(library: dict | SkillLibrary) -> None:
    return save_library_model(library)


def parse_skill_trailer_model(
    source: str | SkillDefinition | SkillDefinitionDraft,
) -> SkillDefinition | None:
    return SkillDefinition.from_trailer_text(source)


def parse_skill_trailer(source: str | SkillDefinition | SkillDefinitionDraft) -> dict | None:
    skill = parse_skill_trailer_model(source)
    return skill.to_sparse_dict() if skill else None


def apply_skill_update_model(source: str | SkillDefinition | SkillDefinitionDraft) -> SkillLibrary:
    skill = parse_skill_trailer_model(source)
    current = load_library_model()
    if not skill:
        return current

    library = current.replace_or_append(skill, max_skills=MAX_SKILLS)
    save_library_model(library)
    return library


def apply_skill_update(source: str | SkillDefinition | SkillDefinitionDraft) -> dict:
    return apply_skill_update_model(source).to_dict()


def strip_skill_trailer(text: str) -> str:
    return SkillDefinition.strip_trailer_text(text)


def render_skills(library: dict | SkillLibrary) -> str:
    return SkillLibrary.normalized(library, max_skills=MAX_SKILLS).render_prompt()


def get_skill_model(library: dict | SkillLibrary, name: str) -> SkillDefinition | None:
    return SkillLibrary.normalized(library, max_skills=MAX_SKILLS).get(name)


def get_skill(library: dict | SkillLibrary, name: str) -> dict | None:
    skill = get_skill_model(library, name)
    return skill.to_dict() if skill else None
