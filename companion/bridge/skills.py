"""Shared reusable build recipes for bridge procedural memory."""

import json
import os
import re
from pathlib import Path


SKILL_RE = re.compile(r"<skill>(.*?)</skill>", re.DOTALL | re.IGNORECASE)
MAX_SKILLS = 50

STARTER_SKILLS = [
    {
        "name": "build_burner_mining_setup",
        "params": ["resource_name", "output_pos"],
        "steps": [
            "find_nearest_resource for resource_name",
            "check_placement for a burner mining drill on the resource patch",
            "place_entity burner-mining-drill facing output_pos",
            "check_placement for a chest or belt at the drill output",
            "place_entity chest or route_belt from the drill output toward output_pos",
            "insert_items coal into the burner mining drill",
            "verify_production for the mined resource",
        ],
        "outcome": "burner drill produces the requested resource onto the output",
    },
    {
        "name": "lay_smelting_line",
        "params": ["ore_belt_pos", "furnace_count"],
        "steps": [
            "check_placement for furnace_count stone furnaces beside ore_belt_pos",
            "place_entity stone-furnace in a straight line",
            "get_machine_belt_positions for the furnaces",
            "route_belt ore past the input side of the furnaces",
            "place inserters with correct facing from the ore belt into each furnace",
            "route_belt plates past the output side of the furnaces",
            "place inserters with correct facing from each furnace to the plates belt",
            "insert_items coal into each stone furnace or route fuel belt if available",
            "verify_production for iron-plate or copper-plate",
        ],
        "outcome": "iron or copper plates move on the output belt",
    },
    {
        "name": "feed_lab",
        "params": ["lab_pos", "science_belt_pos"],
        "steps": [
            "check_placement for lab_pos and a belt-adjacent inserter",
            "place_entity lab at lab_pos",
            "route_belt automation-science-pack to science_belt_pos",
            "place inserters with correct facing from science_belt_pos into the lab",
            "insert_items automation-science-pack when belt supply is not ready",
            "verify_production for research progress",
        ],
        "outcome": "lab consumes science packs and advances research",
    },
    {
        "name": "build_steam_power",
        "params": ["water_pos", "target_pos"],
        "steps": [
            "get_recipes_for_item for offshore-pump, boiler, and steam-engine before guessing recipe names; if enabled=false, unlock the listed technology before crafting",
            "craft offshore-pump, boiler, steam-engine, small-electric-pole, and pipe as needed",
            "find_entity_placements for offshore-pump near water_pos and choose an allowed candidate",
            "derive boiler, steam-engine, and any needed pipe positions from nearby placement checks; do not assume a fixed orientation or hardcoded layout",
            "place one component at a time, then verify the chain has water reaching the boiler and steam reaching the engine before extending poles",
            "connect small-electric-pole coverage from the steam engine to target_pos",
            "insert_items fuel into the boiler",
            "verify_production for powered labs or power network status",
        ],
        "outcome": "steam engine produces electricity and powers the target",
    },
]


def _skills_file() -> Path:
    return Path(__file__).resolve().parent / ".skills.json"


def _str_list(value) -> list:
    if not isinstance(value, list):
        return []
    return [str(item).strip() for item in value if str(item).strip()]


def _normalize_skill(value) -> dict | None:
    if not isinstance(value, dict):
        return None
    name = value.get("name", "")
    if not isinstance(name, str) or not name.strip():
        return None
    skill = {
        "name": name.strip(),
        "params": _str_list(value.get("params", [])),
        "steps": _str_list(value.get("steps", [])),
        "outcome": value.get("outcome", "") if isinstance(value.get("outcome", ""), str) else "",
    }
    skill["outcome"] = skill["outcome"].strip()
    return skill


def _normalize_library(value) -> dict:
    if not isinstance(value, dict):
        return {"skills": []}
    normalized = []
    seen = set()
    items = value.get("skills", [])
    if not isinstance(items, list):
        return {"skills": []}
    for item in items:
        skill = _normalize_skill(item)
        if not skill:
            continue
        name = skill["name"]
        if name in seen:
            normalized = [existing for existing in normalized if existing["name"] != name]
        seen.add(name)
        normalized.append(skill)
        if len(normalized) >= MAX_SKILLS:
            break
    return {"skills": normalized}


def default_library() -> dict:
    return {
        "skills": [{
            "name": skill["name"],
            "params": list(skill["params"]),
            "steps": list(skill["steps"]),
            "outcome": skill["outcome"],
        } for skill in STARTER_SKILLS]
    }


def load_library() -> dict:
    starters = _normalize_library(default_library())["skills"]
    try:
        data = json.loads(_skills_file().read_text())
    except (ValueError, OSError):
        return {"skills": starters}
    saved = _normalize_library(data)["skills"]
    if not saved:
        return {"skills": starters}

    merged = list(starters)
    positions = {skill["name"]: i for i, skill in enumerate(merged)}
    for skill in saved:
        name = skill["name"]
        if name in positions:
            merged[positions[name]] = skill
        else:
            positions[name] = len(merged)
            merged.append(skill)
        if len(merged) >= MAX_SKILLS:
            break
    return {"skills": merged[:MAX_SKILLS]}


def save_library(library: dict) -> None:
    path = _skills_file()
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = json.dumps(_normalize_library(library)) + "\n"
    except TypeError as e:
        print(f"[skills] WARNING: refusing to save unserializable skill library: {e}")
        return None
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


def _parse_params(value: str) -> list:
    if not isinstance(value, str):
        return []
    return [part.strip() for part in value.split(",") if part.strip()]


def parse_skill_trailer(text: str) -> dict | None:
    if not isinstance(text, str):
        return None
    match = SKILL_RE.search(text)
    if not match:
        return None

    parsed = {}
    active_key = None
    params = []
    steps = []
    for raw_line in match.group(1).splitlines():
        line = raw_line.strip()
        if not line:
            continue
        key, sep, value = line.partition(":")
        key_lower = key.strip().lower()
        if sep and key_lower == "name":
            parsed["name"] = value.strip()
            active_key = None
        elif sep and key_lower == "params":
            active_key = "params"
            params.extend(_parse_params(value.strip()))
        elif sep and key_lower == "steps":
            active_key = "steps"
        elif sep and key_lower == "outcome":
            parsed["outcome"] = value.strip()
            active_key = None
        elif active_key == "params" and line.startswith("- "):
            item = line[2:].strip()
            if item:
                params.append(item)
        elif active_key == "steps" and line.startswith("- "):
            item = line[2:].strip()
            if item:
                steps.append(item)

    if params:
        parsed["params"] = params
    if steps:
        parsed["steps"] = steps
    if not parsed.get("name"):
        return None
    return parsed


def apply_skill_update(text: str) -> dict:
    parsed = parse_skill_trailer(text)
    current = load_library()
    skill = _normalize_skill(parsed)
    if not skill:
        return current

    skills = [existing for existing in current.get("skills", []) if existing.get("name") != skill["name"]]
    skills.append(skill)
    library = _normalize_library({"skills": skills[-MAX_SKILLS:]})
    save_library(library)
    return library


def strip_skill_trailer(text: str) -> str:
    if not isinstance(text, str):
        return ""
    if not SKILL_RE.search(text):
        return text
    stripped = SKILL_RE.sub("", text)
    stripped = re.sub(r"\n{3,}", "\n\n", stripped)
    return stripped.strip()


def render_skills(library: dict) -> str:
    if not isinstance(library, dict):
        return ""
    skills = _normalize_library(library).get("skills", [])
    if not skills:
        return ""

    lines = ["Available skills (reuse these recipes; follow the steps with your tools):"]
    for skill in skills:
        params = ", ".join(skill.get("params", []))
        outcome = skill.get("outcome", "")
        lines.append(f"- {skill['name']}({params}) — {outcome}")
    return "\n".join(lines)


def get_skill(library: dict, name: str) -> dict | None:
    if not isinstance(name, str):
        return None
    for skill in _normalize_library(library).get("skills", []):
        if skill.get("name") == name:
            return skill
    return None
