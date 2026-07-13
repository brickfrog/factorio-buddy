# Recovery

Repair the existing dependency graph before replacing it.

- Audit first and identify the earliest failed edge: source, transport, fuel,
  power, processing, output, or consumer.
- Inspect partial work and remove or correct invalid pieces before expanding.
- A manual transfer may diagnose or briefly restart a machine, but it does not
  close the dependency and must not be repeated as the repair.
- Prefer restoring continuity over creating another disconnected producer.
- After repair, discard the contaminated observation and run a new clean
  sustained verification window.
