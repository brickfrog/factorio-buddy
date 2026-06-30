"""Pure autonomy planner/execution prompt assembly."""


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
    "work. For repeated extraction or crafting, use one tool call with a count "
    "parameter instead of many repeated mutating calls. End with one ledger "
    "block:\n"
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


def build_autonomy_prompt(mode: str, ledger_text: str, live_state: str) -> str:
    """Assemble an autonomy prompt from already-loaded pure inputs."""
    prompt = PLANNER_PROMPT if mode == "plan" else EXECUTION_PROMPT
    parts = [ledger_text, live_state, prompt]
    return "\n\n".join(part for part in parts if part)
