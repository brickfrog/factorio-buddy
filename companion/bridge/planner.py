"""Pure autonomy planner/execution prompt assembly."""

from typing import Any

from models import (
    AutonomyDecision,
    AutonomyDecisionReason,
    AutonomyMode,
    AutonomyPromptInput,
    JournalWindow,
    LedgerState,
    LiveCompletionEvidence,
    LiveState,
    autonomy_mode,
)


PLANNER_STALL_REPEAT_COUNT = 3

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
    "signal: new_objective|plan_ready|plan_done|none\n"
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
    "signal: none|plan_done\n"
    "</ledger>\n"
    "For stale/finished/wrong plans, end with one replacement ledger block:\n"
    "<ledger>\n"
    "objective: <updated goal>\n"
    "plan:\n"
    "- <step>\n"
    "- <step>\n"
    "progress: <why the old plan was stale or complete>\n"
    "signal: new_objective|plan_done|none\n"
    "</ledger>"
)


def choose_autonomy_decision(
    ledger: LedgerState | dict,
    exec_ticks_since_plan: int,
    planner_interval: int,
    *,
    journal_window: Any = None,
    live_completion_evidence: Any = None,
    reflect_due: bool = False,
) -> AutonomyDecision:
    """Return the typed autonomy decision for this tick without IO/state."""
    state = LedgerState.coerce(ledger)
    window = (
        journal_window
        if isinstance(journal_window, JournalWindow)
        else JournalWindow.coerce(journal_window or [])
    )
    completion_evidence = (
        live_completion_evidence
        if isinstance(live_completion_evidence, LiveCompletionEvidence)
        else LiveCompletionEvidence.none()
    )
    live_completion_reason = completion_evidence.reason
    has_plan = bool(state.objective.strip()) and bool(state.plan_steps)
    readiness = state.readiness_evidence()
    ledger_repeated_ready = (
        readiness.has_plan
        and readiness.repeated_ready
        and not readiness.explicit_ready
    )
    actionable_plan = has_plan and (
        window.has_actionable_plan_signal()
        or readiness.is_ready
    )
    repeated_plan_progress = (
        has_plan
        and (
            ledger_repeated_ready
            or window.has_repeated_unsignaled_progress(
                min_count=PLANNER_STALL_REPEAT_COUNT,
            )
            or window.has_repeated_ready_progress(
                min_count=PLANNER_STALL_REPEAT_COUNT,
            )
        )
    )
    if not state.objective.strip() or not state.plan_steps:
        return AutonomyDecision(
            mode=AutonomyMode.PLAN,
            reason=AutonomyDecisionReason.MISSING_PLAN,
        )

    try:
        interval = int(planner_interval)
    except (TypeError, ValueError):
        interval = 0
    try:
        exec_ticks = int(exec_ticks_since_plan)
    except (TypeError, ValueError):
        exec_ticks = 0
    mode = (
        AutonomyMode.PLAN
        if exec_ticks >= max(0, interval)
        else AutonomyMode.EXECUTE
    )
    reason = (
        AutonomyDecisionReason.PLANNER_INTERVAL
        if mode == AutonomyMode.PLAN
        else AutonomyDecisionReason.WITHIN_INTERVAL
    )

    if mode == AutonomyMode.PLAN and repeated_plan_progress:
        mode = AutonomyMode.EXECUTE
        reason = AutonomyDecisionReason.REPEATED_PLAN_PROGRESS
    elif mode == AutonomyMode.PLAN and actionable_plan:
        mode = AutonomyMode.EXECUTE
        reason = AutonomyDecisionReason.ACTIONABLE_PLAN
    if (
        mode == AutonomyMode.EXECUTE
        and window.newest_event_indicates_plan_done()
    ):
        mode = AutonomyMode.PLAN
        reason = AutonomyDecisionReason.PLAN_DONE
    if mode == AutonomyMode.EXECUTE and live_completion_reason:
        mode = AutonomyMode.PLAN
        reason = AutonomyDecisionReason.LIVE_STATE_COMPLETION
    if (
        mode == AutonomyMode.EXECUTE
        and reflect_due
        and not actionable_plan
        and not repeated_plan_progress
    ):
        mode = AutonomyMode.PLAN
        reason = AutonomyDecisionReason.REFLECTION_DUE

    return AutonomyDecision(
        mode=mode,
        reason=reason,
        actionable_plan=actionable_plan or repeated_plan_progress,
    )


def choose_autonomy_mode(ledger: LedgerState | dict, exec_ticks_since_plan: int,
                         planner_interval: int) -> str:
    """Return the autonomy mode for this tick without touching IO/state."""
    return choose_autonomy_decision(
        ledger,
        exec_ticks_since_plan,
        planner_interval,
    ).mode_value


def _live_state_model(live_state: LiveState | str) -> LiveState:
    if isinstance(live_state, LiveState):
        return live_state
    return LiveState.from_line(live_state)


def live_state_entity_counts(live_state: LiveState | str) -> dict[str, int]:
    """Parse the compact live-state entity summary into name -> count."""
    return _live_state_model(live_state).entity_counts


def objective_completion_evidence(
    ledger: LedgerState | dict,
    live_state: LiveState | str,
) -> LiveCompletionEvidence:
    """Return typed evidence that live state has completed the ledger objective."""
    return LedgerState.coerce(ledger).live_state_completion_evidence(live_state)


def build_autonomy_prompt(
    mode: AutonomyMode | str,
    ledger_text: str,
    live_state: LiveState | str,
) -> str:
    """Assemble an autonomy prompt from already-loaded pure inputs."""
    prompt = (
        PLANNER_PROMPT
        if autonomy_mode(mode) == AutonomyMode.PLAN
        else EXECUTION_PROMPT
    )
    live_state_text = live_state.to_line() if isinstance(live_state, LiveState) else live_state
    parts = [ledger_text, live_state_text, prompt]
    return "\n\n".join(part for part in parts if part)


def build_autonomy_prompt_model(source: AutonomyPromptInput | dict) -> str:
    """Assemble an autonomy prompt from typed context, not pre-rendered text."""
    prompt_input = (
        source
        if isinstance(source, AutonomyPromptInput)
        else AutonomyPromptInput.model_validate(source)
    )
    return prompt_input.render(
        planner_prompt=PLANNER_PROMPT,
        execution_prompt=EXECUTION_PROMPT,
    )
