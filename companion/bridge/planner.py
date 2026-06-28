"""Pure autonomy planner/execution prompt assembly."""


PLANNER_PROMPT = (
    "(planner autonomy tick - no new messages from the player) "
    "You're playing Factorio on your own initiative right now. Prioritize "
    "continuity: review the committed objective, recent progress, and live "
    "state before changing direction. Call situation_report once to assess "
    "your current situation instead of render_map + get_inventory + "
    "get_resources, then produce or refresh a concrete 3-6 step plan toward "
    "the committed objective. If no objective is committed, pick one yourself "
    "from the situation_report and make the plan serve it. Finish before you "
    "switch: choose a new objective only when the current one is genuinely "
    "finished or impossible. Narrate briefly with broadcast_thought so the "
    "stream stays lively. Emit exactly one ledger block at the end of your "
    "reply in this format, setting the objective and plan:\n"
    "<ledger>\n"
    "objective: your current objective\n"
    "plan:\n"
    "- next concrete step\n"
    "- following concrete step\n"
    "progress: one short note about what changed\n"
    "</ledger>"
)


EXECUTION_PROMPT = (
    "(execution autonomy tick - no new messages from the player) "
    "You're playing Factorio on your own initiative right now. Prioritize "
    "continuity: continue the committed objective and committed plan, then "
    "execute the NEXT incomplete plan step with your tools right now. Do not "
    "re-plan, do not re-scan areas already seen, do not restart the plan, and "
    "do not switch objectives during this cheap execution tick. If you "
    "genuinely need to check your surroundings, call situation_report once "
    "instead of render_map + get_inventory + get_resources. Actually do "
    "things, don't just describe them. Narrate briefly with broadcast_thought "
    "so the stream stays lively. If the plan is finished or clearly wrong, say "
    "so briefly; the next planner tick will refresh it. After meaningful "
    "progress, emit exactly one short ledger block at the end of your reply "
    "with just a progress note, leaving objective and plan unchanged:\n"
    "<ledger>\n"
    "progress: one short note about what changed\n"
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
