# Development Notes

## Rules for LLM Players

### Proximity Enforcement
- The player character must be near entities to interact with them
- `place`: player must be within 10 tiles of target position
- `insert`: player must be within 5 tiles of target entity
- `set-recipe`: player must be within 5 tiles of target entity
- Use `walk-to x,y` to move the player before interacting

### No Cheating
- Do NOT use `exec` to spawn entities, items, or power sources
- Do NOT use `exec` to teleport the player
- All entities must be placed from inventory using proper commands
- All items must be crafted or mined, not spawned
- Power must come from in-game sources (steam engines, solar, etc.)
- Research should be done via labs with science packs (not direct `tech.researched = true`)

### Bootstrapping Exception
- For initial development, bootstrapping with `exec` may be needed
- Document any cheats used and plan to remove them
- Current bootstrap: electronic circuits were spawned to build first assembler

## Manual Commands That Should Be CLI Features

### Research/Technology
- `get research status` - List researched techs and available ones
- `get research available` - Show techs that can be researched next
- `research start <tech>` - Start researching a technology
- `get research current` - Show current research progress

### Recipes
- `get recipes craftable` - List all recipes player can hand-craft
- `get recipes --category <cat>` - Already exists but could be improved

### Inventory Transfer
- `collect --from <unit_number> --item <name>` - Take items from entity to character
- `collect-all --area <area> --item <name>` - Collect item from all entities in area

### Entity Queries
- `get entities --type <type>` - Filter by entity type (furnace, mining-drill, etc.)
- `get entity-types --area <area>` - Summary of entity types in area

### Crash Site / Exploration
- `get containers --area <area>` - List containers with contents

## Factorio 2.0 Early Game Notes

The early game in Factorio 2.0 is different:
- Transport belts require "pressing" category (needs assembling-machine-1)
- Electronic circuits require "electronics" category (needs assembling-machine-1)
- Assembling-machine-1 requires electronic circuits
- This creates a dependency cycle that must be broken by:
  - Starting items from crash site
  - Research progression
  - Some early-game machine I haven't found yet

Currently researched: steam-power
Next available: electronics (needs lab + science packs)

Lab requires: iron-gear-wheel x10, electronic-circuit x10, transport-belt x4
- Cannot craft electronic-circuit or transport-belt yet

## Current Automation Setup

### Power Generation (REAL, no cheats)
- Steam power at (-57, 16.5):
  - Offshore pump at (-56.5, 20.5) facing south
  - Boiler at (-57, 16.5) facing east
  - Steam engine at (-53.5, 16.5) facing east
  - Power line running from (-50, 16) to (52, -25) with 25 poles

### Iron Smelting
Iron smelting line at (48,-24) to (48,-30):
- 4 burner-mining-drills facing east
- 4 stone-furnaces receiving ore from drills
- Blueprint saved as "iron-smelter-cell" (8 entities)

### Assembly
Assembling machine at (52.5, -25.5):
- Set to produce transport-belt
- Powered by real steam power via pole network
- Producing 2 belts per craft cycle

### Coal Mining
Coal miners at (78, -22) and (78, -24):
- 2 burner-mining-drills facing west
- Belt line partially laid from coal to smelters
- TODO: Complete belt line and add inserters

### Next Steps
1. Complete coal delivery belt line to iron smelters
2. Add inserters to distribute coal to furnaces/drills
3. Scale up production
4. Research more technologies

## A* Pathfinding

The CLI now supports A* pathfinding for both walking and belt routing:

### Belt Routing with A*
```bash
# Route a belt line avoiding obstacles
factorioctl belt line --from 0,0 --to 20,0 --search-radius 10

# Dry run to preview the path
factorioctl belt line --from 0,0 --to 20,0 --dry-run

# Multi-segment route through waypoints
factorioctl belt route --waypoints "0,0;10,5;20,0"
```

### Walking with A*
```bash
# Walk with pathfinding to avoid obstacles
factorioctl walk-to 50,50 --pathfind

# Adjust search radius for pathfinding
factorioctl walk-to 50,50 --pathfind --search-radius 30
```

The pathfinder:
- Builds a collision map from tiles and entities
- Uses A* with turn cost penalties for smoother paths
- Skips resources (can place belts over ore)
- Simplifies walk paths to reduce waypoints

## Bugs Fixed This Session

- `create_blueprint` returns array of entity indices in Factorio 2.0, not count
- Fixed with `#entities` to get count
