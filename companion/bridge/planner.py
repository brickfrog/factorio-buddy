"""Pure autonomy planner/execution prompt assembly."""

import re


PLANNER_PROMPT = (
    "(planner tick) "
    "Assess, then plan. Call situation_report once. continuity: keep your "
    "committed objective unless it is finished or impossible; if you have none, "
    "pick one. Live state is authoritative: if the ledger says an early step is "
    "unfinished but situation_report shows structures/inventory already exist, "
    "update the objective and plan from the live state instead of redoing old "
    "starter work. If situation_report is far from the objective site or only "
    "proves local absence, inspect the target site or known resource/build "
    "coordinates with read-only tools before concluding existing infrastructure "
    "is missing. Write a 3-6 step plan where every step is one concrete tool "
    "action, not a description. Build on what exists; do not redo finished "
    "work. This is a read-only planning turn: do not call mutating tools such "
    "as walk_to, mine_at, craft, place_entity, remove_entity, insert_items, "
    "extract_items, route_belt, start_research, clear_area, or set_recipe. Do "
    "not execute the plan in this turn; stop after the ledger block. For "
    "repeated extraction or crafting, plan one tool call with a count parameter "
    "instead of many repeated mutating calls. End with one ledger block:\n"
    "<ledger>\n"
    "objective: <goal>\n"
    "plan:\n"
    "- <step>\n"
    "- <step>\n"
    "progress: <what changed>\n"
    "</ledger>"
)


EXECUTION_PROMPT = (
    "(execution tick) "
    "Do the next unfinished step in your plan now: call the tool, do not "
    "describe it. Do not re-plan or re-scan during normal execution. Do not "
    "walk more than ~25 tiles unless a step needs a specific tile. After you "
    "place or change production, call verify_production and fix what is broken. "
    "continuity: keep the committed objective and plan. For repeated extraction "
    "or crafting, use one tool call with a count parameter instead of many "
    "repeated mutating calls. If you must look, call situation_report once. If "
    "live state shows the plan is stale, finished, or clearly wrong, stop before "
    "any mutating tool call, say so in one line, and replace the ledger with the "
    "next objective and 3-6 concrete tool-action steps. For normal execution, "
    "end with one progress-only ledger block:\n"
    "<ledger>\n"
    "progress: <what changed>\n"
    "</ledger>\n"
    "For stale/finished/wrong plans, end with one replacement ledger block:\n"
    "<ledger>\n"
    "objective: <updated goal>\n"
    "plan:\n"
    "- <step>\n"
    "- <step>\n"
    "progress: <why the old plan was stale or complete>\n"
    "</ledger>"
)


def choose_autonomy_mode(ledger: dict, exec_ticks_since_plan: int,
                         planner_interval: int) -> str:
    """Return the autonomy mode for this tick without touching IO/state."""
    objective = str(ledger.get("objective", "")).strip()
    plan_steps = ledger.get("plan_steps", [])
    if not objective or not plan_steps:
        return "plan"
    if exec_ticks_since_plan >= planner_interval:
        return "plan"
    return "execute"


_LIVE_ENTITY_RE = re.compile(r"\b([a-z0-9][a-z0-9-]*)=(\d+)\b", re.IGNORECASE)
_STEAM_BUILD_INTENT_RE = re.compile(
    r"\b(?:build|deploy|place|set up|setup|construct|create|craft|complete)\b"
    r".{0,80}\b(?:steam power|steam-power|offshore pump|offshore-pump|boiler|"
    r"steam engine|steam-engine)\b"
    r"|\b(?:steam power|steam-power)\b.{0,80}\b"
    r"(?:build|deployment|setup|set up|complete)\b",
    re.IGNORECASE | re.DOTALL,
)


def live_state_entity_counts(live_state: str) -> dict[str, int]:
    """Parse the compact live-state entity summary into name -> count."""
    counts: dict[str, int] = {}
    for name, raw_count in _LIVE_ENTITY_RE.findall(str(live_state)):
        try:
            count = int(raw_count)
        except ValueError:
            continue
        key = name.lower()
        counts[key] = counts.get(key, 0) + count
    return counts


def _ledger_objective_text(ledger: dict) -> str:
    parts = [str(ledger.get("objective", ""))]
    parts.extend(str(step) for step in ledger.get("plan_steps", []))
    parts.extend(str(note) for note in ledger.get("progress_notes", []))
    return "\n".join(parts).lower()


def objective_satisfied_by_live_state(ledger: dict, live_state: str) -> str:
    """Return a short reason when live state proves the ledger is stale.

    The rules are intentionally conservative: only well-known early-game
    objectives with direct world evidence trigger an automatic planner tick.
    """
    text = _ledger_objective_text(ledger)
    counts = live_state_entity_counts(live_state)
    if not text or not counts:
        return ""

    def has(name: str) -> bool:
        return counts.get(name, 0) > 0

    has_steam_chain = all(
        has(name) for name in ("offshore-pump", "boiler", "steam-engine")
    )
    has_lab_power_evidence = has("lab") and (
        has_steam_chain or has("small-electric-pole")
    )

    mentions_steam_power = (
        "steam power" in text
        or "steam-power" in text
        or "steam engine" in text
        or "steam-engine" in text
        or "boiler" in text
        or "offshore pump" in text
        or "offshore-pump" in text
    )
    mentions_powered_lab = (
        "power the lab" in text
        or "powered lab" in text
        or "lab near power endpoint" in text
    )
    steam_build_intent = _STEAM_BUILD_INTENT_RE.search(text) is not None
    mentions_automation_research = (
        "automation research" in text
        or "start automation" in text
        or "automation-science-pack" in text
    )
    progress_says_automation_done = (
        "automation research completed" in text
        or "automation+electric-mining-drill research" in text
    )

    if (
        steam_build_intent
        and mentions_steam_power
        and mentions_powered_lab
        and has_steam_chain
        and has_lab_power_evidence
    ):
        return "live state already has steam power and a powered-lab footprint"
    if steam_build_intent and mentions_steam_power and has_steam_chain:
        return "live state already has offshore-pump, boiler, and steam-engine"
    if mentions_powered_lab and has_lab_power_evidence:
        return "live state already has lab plus power-grid evidence"
    if mentions_automation_research and progress_says_automation_done and has("lab"):
        return "ledger progress says automation research completed and live state has a lab"
    return ""


def build_autonomy_prompt(mode: str, ledger_text: str, live_state: str) -> str:
    """Assemble an autonomy prompt from already-loaded pure inputs."""
    prompt = PLANNER_PROMPT if mode == "plan" else EXECUTION_PROMPT
    parts = [ledger_text, live_state, prompt]
    return "\n\n".join(part for part in parts if part)
