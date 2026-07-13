# Logistics

Reason from producer to consumer, not from isolated machines.

- Start with the milestone item's one-minute produced and consumed rates. A
  zero rate is an outcome to diagnose, not permission to place random machines.
- Read the target-flow producers, consumers, buffers, complete paths, and
  nearest disconnected pair. Prefer repairing or extending those exact nodes.
- For each producer, close its first `item_supply_dependencies` or fuel
  dependency before working downstream. Repeat recursively until extraction is
  machine-driven.
- Prefer direct insertion for adjacent stages and continuous transport for
  repeated movement over distance.
- Treat belts, inserters, pipes, and logistics systems as graph edges whose
  pickup, direction, drop, power, and capacity must all be valid.
- Use chests to buffer an already automated inbound and outbound flow. A chest
  with no outbound edge is not delivery when another process needs the item.
- Reuse and extend working routes when practical; avoid parallel islands that
  require the character to bridge them.
- After connecting a path, confirm both directed topology and nonzero rate.
  Verify the downstream consumer, not merely the upstream producer.
- Diagnose connectivity before capacity. Add parallel machines only after one
  complete path is sustained and the rates show a real throughput bottleneck.
