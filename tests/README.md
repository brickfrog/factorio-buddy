# Testing factorioctl

This directory contains test infrastructure for the factorioctl CLI.

## Quick Start

```bash
# Build, start a disposable isolated server, exercise the Rust/Lua/RCON path,
# and clean it up automatically.
just test-live
```

## Test Files

- `setup.sh` - Builds the Rust binaries, creates an isolated map, and starts Factorio
- `run_tests.sh` - Runs the test suite against the running server
- `live_regressions.sh` - Verifies high-risk mod and MCP contracts in Factorio
- `buddy_runtime.sh` - Verifies Buddy's managed-server security and lifecycle
- `autonomous_eval.sh` - Unattended real-model (Claude Opus) evaluation matrix
- `../scripts/smoke_agent_binding.sh` - Proves independent NPC character binding
- `cleanup.sh` - Stops server and cleans up

The gameplay server's save, write-data, mods, and script output all live under
one temporary directory created by `just test-live`. The live regressions use
raw Lua only to create disposable fixtures through the
explicitly enabled trusted-operator path. Behavior under test goes through the
shipped `/claude` mod dispatcher or model-facing MCP server: research triggers,
reach, item conservation, surface scoping, entity lookup, production
verification, route reuse, protocol errors, and RCON connection reuse.

## Server Ports

- RCON: `127.0.0.1:27016` (test server)
- Game: `34198` (for spectating)
- Buddy lifecycle RCON: `127.0.0.1:27217` (override with `BUDDY_TEST_RCON_PORT`)
- Buddy lifecycle game: `34399` (override with `BUDDY_TEST_GAME_PORT`)

`buddy_runtime.sh` exercises clean shutdown, unexpected server death, and
unclean resume with a temporary HOME, write-data directory, and save. It proves
that managed RCON is loopback-only with private generated credentials, a
same-agent controller cannot take the active lease, unrelated autosaves cannot
contaminate resume, startup lifecycle calls reuse one RCON connection,
unexpected owned-server death terminates Buddy, and clean shutdown leaves no
Factorio process behind.

## Unattended model evaluation

```bash
tests/autonomous_eval.sh --arm opus|deterministic|jev-shadow|jev \
    --scenario open-play|fuel-repair --seed <u32> --repeat <n> --output <new-dir>
```

This runs a real model: authenticated Claude Code with `claude-opus-5-5`
access, release `buddy`/`mcp`/`factorioctl` binaries built from this tree,
Factorio, `jq`, `ss` and `bd` are required. The `jev-shadow` and `jev` arms also
need `TYPESAFE_API_KEY`. Preflight writes `<output>/preflight.json` and stops
before any trial if a prerequisite is missing, the output directory already
exists, RCON `27217`/game `34399` (override with `AUTONOMOUS_EVAL_RCON_PORT` /
`AUTONOMOUS_EVAL_GAME_PORT`) are busy, or one minimal no-tools Opus request
fails. A busy port is never freed by killing its owner.

Repetitions run serially. Each gets its own directory with a private HOME, a
new save, write-data, a mod copy, a disposable Beads tracker (so `file_issue`
cannot reach the repository tracker), and a working directory. Buddy runs with
`--effort medium`, `--heartbeat-seconds 5`, `--turn-timeout-seconds 300`, game
speed 1, and an autonomy budget of 30 turns or 15 minutes. A `claude` wrapper
on PATH restores the real HOME only for authentication and tees the unmodified
stream-JSON into `claude-stream.jsonl`.

`fuel-repair` is a lab-only scenario whose world is prepared with trusted raw
Lua before Buddy starts. `--fixture` picks one of eight cases (default
`missing-feed`): `missing-feed`, `already-sustainable`,
`insufficient-materials`, `blocked-route`, `mixed-source-belt`,
`stale-consumer`, `failed-unchanged-repair` and `competing-consumers`.
`--layout heldout` rotates the same case a quarter turn, moves it and changes
its obstacle geometry; do not tune a policy on held-out runs. Before the save
is kept, the runner checks each case's premise through the shipped
`diagnose_fuel_sustainability` remote and one `repair_fuel_sustainability`
dry-run through the MCP binary, and checks that the dry-run changed nothing;
a violated premise fails the trial before Buddy starts. `fixture.json`,
`fixture-dryrun.json`, `fixture-validation.json` and the manifest record the
case, layout and per-case expectation (for example `repair_expected`).
`stale-consumer` also replaces its target drill (new `unit_number`) once,
right after the first repair preview appears in Buddy's evidence log.
`--fixture-only` builds, saves and validates one fixture, runs case probes
that execute the shipped controller on it (then restores the validated save),
and exits without Buddy or Claude:

```bash
AUTONOMOUS_EVAL_RCON_PORT=27416 AUTONOMOUS_EVAL_GAME_PORT=34598 \
    tests/autonomous_eval.sh --scenario fuel-repair --fixture blocked-route \
    --layout heldout --fixture-only --seed 2590060468 --repeat 1 --output <new-dir>
```

`--planner noop` (fuel-repair only) replaces Opus with a stub that ends every
turn without tool calls. The prepared defect then survives until Buddy's
maintenance check, so the deterministic, Jev-shadow and Jev decision paths
run against a real fixture and a real controller. Results are component
replay (`planner_model: "noop-stub"`) and never count as planner results:

```bash
TYPESAFE_API_KEY=... tests/autonomous_eval.sh --arm jev-shadow --scenario fuel-repair \
    --fixture missing-feed --planner noop --seed 2590060468 --repeat 1 --output <new-dir>
```

`--budget-minutes N` (default 15) sets the autonomy deadline for long runs
toward the rocket. Turns scale to `max(30, 2N)` and the hard wall is the
deadline plus 5 minutes. Compare only trials with equal budgets. Do not
rebuild binaries or edit the runner while a trial is running: Claude spawns a
fresh MCP process every turn and bash reads the script lazily.

`--continue-from SAVE_ZIP` (open-play only) starts each trial from a copy of
an earlier trial's `save.zip`, and Buddy starts without `--fresh`. Chain 60-minute
runs this way so progress toward the rocket builds up across trials. Pass the
seed the save was made with. The manifest records the source path and its
sha256. The milestones still measure only this trial's holdout windows.

When Buddy logs `autonomy_budget_exhausted`, no model or maintenance turn can
start. The runner then samples `evaluation_sample` at the start, middle and
end of two consecutive 3600-tick holdout windows and credits only production
sustained in both windows: plate automation, powered production, automated
science delivery, research progress, and for `fuel-repair` the case-aware
`durable_fuel_recovery` outcome. A fuel target counts only with measured
output in both windows (`products_finished`, or the amount mined from its own
resource patch) and a durable feed confirmed by `diagnose_fuel_sustainability`;
status-only targets make the result `outcome_unknown`. Refusal cases succeed
on truthful behaviour (no teardown, no false success, no repeated unchanged
failure). Model prose and working statuses are not success. Buddy is then stopped with
SIGTERM; the trial fails unless the owned server exits and the save sidecar
records `clean_shutdown: true`. Connected players, tool calls after the
budget, leaked credentials, and forced kills are invariant failures. Every
trial writes `manifest.json`, `summary.json`, logs, samples, the final save and
`evidence.jsonl`; `<output>/matrix.jsonl` collects one summary per trial. The
exit status is 0 only when every trial is free of invariant failures.
