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
            "call execute_edge_miner with dry_run=true for resource_name near the patch center; execute it with dry_run=false when success/ready are true",
            "call execute_direct_smelter with dry_run=true using the returned drill output tile before hand-authoring belt/inserter/furnace geometry; execute it when the plan is ready",
            "route coal to the burner mining drill with belt/inserter/chest fuel supply when coal is nearby; use temporary fuel inserts only for bootstrap recovery",
            "call diagnose_fuel_sustainability near the drill after bootstrap fuel; execute build_fuel_supply with returned build_fuel_supply_args for the ranked repair",
            "verify_production for the mined resource",
        ],
        outcome="burner drill produces the requested resource onto the output",
    ),
    SkillDefinition(
        name="lay_smelting_line",
        params=["ore_belt_pos", "furnace_count"],
        steps=[
            "call execute_direct_smelter when starting from a single drill output tile; use dry_run=true for planning and dry_run=false to build the cell",
            "check_placement for furnace_count stone furnaces beside ore_belt_pos",
            "place_entity stone-furnace in a straight line",
            "get_machine_belt_positions for the furnaces",
            "route_belt ore past the input side of the furnaces",
            "place burner-inserters with correct facing from the ore belt into each furnace until electric inserter is unlocked",
            "route_belt plates past the output side of the furnaces",
            "place burner-inserters with correct facing from each furnace to the plates belt until electric inserter is unlocked",
            "route a coal fuel belt or chest-fed inserter to furnaces when coal production exists; use temporary fuel inserts only to restart a stalled bootstrap line",
            "call diagnose_fuel_sustainability around the smelting line; execute build_fuel_supply with returned build_fuel_supply_args before treating the line as automated",
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
            "call build_assembler_output with dry_run=true for automation-science-pack from the assembler output inserter drop tile toward science_belt_pos; execute it when route.materials_sufficient is true",
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
            "call diagnose_fuel_sustainability around the boiler; execute build_fuel_supply with returned build_fuel_supply_args before treating power as complete",
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
