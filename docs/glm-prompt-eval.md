# GLM Prompt Eval Loop

This document describes how to use the same GLM provider that powers Doug's
Claude Code runs as a direct API model for offline prompt testing.

The goal is not to replace the live agent loop. The goal is to test prompt and
tool-choice behavior cheaply before spending time, tokens, and Factorio runtime
on a full `just resume` run.

## Current State

The bridge already has an offline prompt regression harness:

```bash
cd companion/bridge
uv run python prompt_eval.py
uv run python prompt_eval.py examples
uv run python prompt_eval.py mine-logs ../logs/bridge-2026-07-02_154159.log
uv run python prompt_eval.py extract-transcript ../logs/bridge-2026-07-02_210229.log \
  --since '2026-07-02 21:02:45' \
  --until '2026-07-02 21:02:46'
```

It can:

- score frozen scenarios against an observed tool/text transcript
- mine candidate scenarios from bridge logs
- extract transcript slices from `.log` or loguru `.jsonl`
- export DSPy-ready examples without depending on DSPy

The missing piece is a model-backed runner that asks GLM, directly, what the
next tool decision should be for a scenario.

## Why Direct GLM

Claude Code is currently used as an agentic runtime:

- system prompt
- Claude Code session
- MCP tools
- skill calls
- tool hooks
- max-turn loop
- ledger and journal continuity

That is useful for playing Factorio, but it is too heavy for prompt regression
testing. For prompt tests, use GLM as a plain request/response model:

```text
scenario + prompt surface + available tool policy -> JSON transcript
```

Then feed that transcript into the deterministic `prompt_eval.py` scorer.

This gives a fast local loop:

```text
prompt candidate -> direct GLM response -> deterministic score -> inspect failures
```

No Factorio server, no MCP subprocess, no Claude Code session, no autonomy loop.

## Provider Wiring

The bridge already points Claude Code at GLM through Anthropic-compatible
environment variables in `companion/bridge/.env`:

```env
ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic
ANTHROPIC_AUTH_TOKEN=...
ANTHROPIC_DEFAULT_HAIKU_MODEL=glm-5-turbo
ANTHROPIC_DEFAULT_SONNET_MODEL=glm-5.2
ANTHROPIC_DEFAULT_OPUS_MODEL=glm-5.2
API_TIMEOUT_MS=3000000
```

Do not put secrets in code, docs, scenario files, or test fixtures. The direct
runner should read the same env var names at runtime.

Recommended default roles:

| Role | Model | Why |
| --- | --- | --- |
| executor simulation | `glm-5-turbo` | cheap, matches Doug's frequent execution tier |
| candidate review / cleanup | `glm-5.2` | stronger model for judging mined scenarios or prompt diffs |
| final acceptance | deterministic scorer + tests | prevents model-as-judge circularity |

## Proposed Commands

Add a new optional script:

```text
companion/bridge/prompt_eval_llm.py
```

Initial commands:

```bash
uv run python prompt_eval_llm.py run-scenario \
  --model glm-5-turbo \
  --scenario first_inserter_deadlock_uses_bounded_bootstrap
```

```bash
uv run python prompt_eval_llm.py run-scenarios \
  --model glm-5-turbo \
  --scenarios prompt_scenarios.json \
  --output /tmp/glm-transcripts.json
```

```bash
uv run python prompt_eval_llm.py refine-candidates \
  --model glm-5.2 \
  --candidates /tmp/mined_candidates.json \
  --output /tmp/refined_candidates.json
```

Later:

```bash
uv run python prompt_eval_llm.py compare-prompts \
  --model glm-5-turbo \
  --scenarios prompt_scenarios.json \
  --baseline-prompt planner.py:EXECUTION_PROMPT \
  --candidate-prompt /tmp/execution_prompt_candidate.txt
```

## Request Shape

The direct runner should ask for a strict JSON transcript:

```json
{
  "tool_calls": ["bootstrap_smelting_once"],
  "text": "Use bounded bootstrap exactly once, then build durable fuel automation."
}
```

The prompt should include:

- scenario name
- prompt surface (`planner`, `execution`, or `autonomy`)
- scenario input text
- relevant prompt text or candidate prompt text
- available tool names
- bridge policy constraints
- instruction to emit JSON only

The response should be validated with `PromptEvalTranscript.coerce(...)`, then
scored with `evaluate_prompt_scenario_model(...)`.

## Minimal Anthropic-Compatible Call

The first implementation can use raw HTTP to avoid adding another SDK:

```python
import json
import os
import urllib.request


def call_glm_messages(*, model: str, system: str, user: str) -> str:
    base_url = os.environ["ANTHROPIC_BASE_URL"].rstrip("/")
    token = os.environ["ANTHROPIC_AUTH_TOKEN"]
    payload = {
        "model": model,
        "max_tokens": 1000,
        "temperature": 0,
        "system": system,
        "messages": [{"role": "user", "content": user}],
    }
    req = urllib.request.Request(
        f"{base_url}/v1/messages",
        data=json.dumps(payload).encode(),
        headers={
            "content-type": "application/json",
            "x-api-key": token,
            "anthropic-version": "2023-06-01",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=120) as response:
        data = json.loads(response.read().decode())
    return "".join(
        block.get("text", "")
        for block in data.get("content", [])
        if isinstance(block, dict)
    )
```

If z.ai's Anthropic-compatible endpoint expects a different path or header, keep
that compatibility shim isolated in `prompt_eval_llm.py`; do not thread it into
the bridge runtime.

## Scoring Flow

For one scenario:

```text
PromptEvalScenario
  -> build user prompt
  -> call GLM
  -> parse JSON as PromptEvalTranscript
  -> evaluate_prompt_scenario_model(...)
  -> PromptEvalScenarioResult
```

For a full corpus:

```text
prompt_scenarios.json
  -> run every scenario
  -> write transcripts
  -> score suite
  -> print failures first
```

Example output:

```json
{
  "score": 0.75,
  "passed": false,
  "results": [
    {
      "scenario_name": "fuel_supply_missing_materials_does_not_execute_build",
      "passed": false,
      "missing_expected_tools": ["bootstrap_smelting_once"],
      "forbidden_tools_seen": ["build_fuel_supply"]
    }
  ]
}
```

## Candidate Refinement

`mine-logs` intentionally emits candidates, not truth. A direct GLM refinement
pass can help clean those up.

Input:

- mined candidate scenario
- source log snippet
- known tool catalog
- bridge policy summary

Output:

```json
{
  "decision": "accept",
  "scenario": {
    "name": "first_inserter_deadlock_uses_bounded_bootstrap",
    "prompt_surface": "execution",
    "input_text": "...",
    "expected_tools": ["bootstrap_smelting_once"],
    "expected_tool_prefix": ["bootstrap_smelting_once"],
    "forbidden_tools": ["insert_items", "extract_items", "hand_feed_furnace"],
    "required_text": ["exactly once", "durable automation"],
    "forbidden_text": [],
    "notes": "..."
  },
  "reason": "The log shows a first-inserter circular dependency and no sanctioned bounded bootstrap call."
}
```

Keep this as a review helper. Do not auto-promote every refined candidate into
the corpus without human inspection.

## Guardrails

- Do not put secrets in generated examples, scenario files, transcripts, or logs.
- Do not let GLM be both the only executor and the only judge.
- Keep deterministic scoring as the acceptance gate.
- Keep live Factorio smoke tests separate from offline prompt evals.
- Treat `bootstrap_smelting_once` as a bounded escape hatch, not successful
  automation by itself.
- Prefer small prompt edits that improve held-out scenarios over giant
  generated rulebooks.

## Implementation Checklist

1. Add `companion/bridge/prompt_eval_llm.py`.
2. Load `companion/bridge/.env` into process env when running directly.
3. Implement one isolated Anthropic-compatible `messages` call helper.
4. Implement `run-scenario` for one built-in or corpus scenario.
5. Validate model output as `PromptEvalTranscript`.
6. Score via `evaluate_prompt_scenario_model`.
7. Add tests with a fake HTTP/client function; do not hit the network in unit tests.
8. Add `run-scenarios` for corpus-wide scoring.
9. Add `refine-candidates` once the runner is stable.
10. Only then consider DSPy/GEPA around the same scenario corpus.

## First Useful Test Cases

Start with the existing cases:

- `first_inserter_deadlock_uses_bounded_bootstrap`
- `fuel_supply_missing_materials_does_not_execute_build`
- `plan_ready_stall_executes_instead_of_replanning`
- `coal_babysitting_prefers_durable_fuel_controller`

Then add mined candidates from logs when they survive review.

## Why This Is Worth It

Doug's expensive failures are mostly local policy mistakes:

- keeps planning when it should execute
- hand-feeds coal forever
- retries impossible placements
- executes automation builders after dry-run already reported missing materials
- misses the bounded bootstrap path for first-inserter deadlocks

Those are exactly the failures an offline prompt-eval loop can catch quickly.
The live game should be reserved for validating that the improved policy still
works against real Factorio geometry and production state.
