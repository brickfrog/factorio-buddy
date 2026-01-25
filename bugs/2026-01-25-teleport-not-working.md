# Bug: Character teleport command does not move character

**STATUS: FIXED**

## Root Cause
The `teleport_character` Lua command only checked `global.factorioctl_character` (the spawned character) but not `game.connected_players` (human player's character).

## Fix
Updated `src/client/lua.rs` to check connected players first, matching the pattern used in other character commands.

## Command
```bash
./target/release/factorioctl --host localhost --port 27016 --password test_password character teleport "76,-26"
```

## Expected Behavior
Character should be moved to position (76, -26).

## Actual Behavior
Command reports "Teleported to (76, -26)" but character remains at previous position (-13.9, -8.6).

## Error Output
```
Teleported to (76, -26)
```
No error shown, but subsequent `character status` shows character still at old position.

## Context
- Game state: Character was at (-13.9, -8.6) near crashed spaceship
- Attempting to teleport to coal drill area

## Workaround
Use `walk-to` with pathfinding instead of teleport. Walk in stages if distance is large.
