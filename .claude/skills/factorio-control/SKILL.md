---
name: factorio-control
description: Control an autonomous Factorio teammate through the Factorio MCP. Use for every gameplay turn to maintain one persistent factory milestone, eliminate character-mediated logistics, close material-flow dependencies, and prove sustained automation from live game evidence.
---

# Factorio Factory Control

Treat the factory as a material-flow graph. The character may construct the
graph, but must not remain an edge in its steady-state operation.

## Operating loop

1. Read `get_factory_milestone`. If none exists, set one concrete milestone
   with a target item and observable completion condition.
2. Call `audit_automation`. Read `production_flow` as the outcome and
   `material_graph.target_flow` as the causal producer-to-consumer path.
3. Close the first reported dependency. Work backward through its item/fuel
   supply evidence and reuse the named producers, consumers, buffers, and belt
   networks before placing anything new.
4. Make one coherent graph change, then audit again. If throughput remains
   zero, diagnose the existing statuses and path before adding capacity.
5. Call `verify_factory_milestone` to start or finish a sustained observation
   window. Select a new milestone only after verification reports `complete`.

## Automation contract

- A completed chain continuously gathers, transports, processes, and delivers
  its required resources without character inventory transfers.
- Manual mining, crafting, insertion, extraction, and lab feeding are bounded
  bootstrap or recovery actions. Replace their material dependency before
  claiming completion.
- A chest is a buffer. It is a valid endpoint only for an explicit stockpile
  milestone; otherwise it needs an automated outbound path to the consumer.
- Working once is not proof. Require positive progress across the verification
  window with zero manual material events.
- A placed machine is not progress by itself. Count it only when upstream
  producers reach its inputs and its output reaches the milestone endpoint.
- Global rates answer whether the factory works; the directed material graph
  answers where it fails. Use both, never one as a substitute for the other.
- Derive geometry from live state. These rules constrain flow and evidence,
  not layouts or coordinates.

## Mutation discipline

- Do not place a speculative duplicate while the audit identifies an existing
  producer, consumer, or nearest disconnected pair.
- Use `analyze_item_flow` on the reported producer/consumer pair when a path is
  disconnected, then repair its first break.
- Treat composite builders and placement planners as execution primitives, not
  strategy. The milestone graph decides what needs to be connected.
- Expand capacity only after one complete path has positive production and
  consumption flow. Connectivity comes before scale.

## Strategy references

Load only the relevant topic with `load_factory_playbook`:

- `bootstrap`: escape initial hand work without making it permanent.
- `logistics`: connect producers, buffers, and consumers into continuous flow.
- `power`: make generation, fuel, distribution, and demand sustainable.
- `smelting`: turn raw extraction into expandable plate flow.
- `science`: automate intermediates through lab consumption.
- `recovery`: repair a stalled or malformed chain without building duplicates.

Keep in-game replies operational and concise.
