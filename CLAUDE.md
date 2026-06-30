# Factorioctl Contributor Notes

## Lua/RCON Architecture

Treat Lua executed through RCON as a real runtime surface. The Rust compiler does
not validate Factorio Lua strings, and `execute_lua()` line-joins generated Lua
before sending it to RCON.

`src/client/lua.rs` should stay thin. It may emit transport glue such as guarded
`remote.call("claude_interface", ...)` wrappers, argument escaping, and compact
JSON fallback errors for an out-of-date synced mod.

Do not add new Factorio gameplay logic as inline Rust string literals. If code
scans entities, resources, tiles, inventories, recipes, technologies, belts,
fluid boxes, electric networks, prototypes, or entity statuses, put that logic in
`companion/mod/claude-interface/control.lua` behind a `claude_interface` remote
function and call it from Rust through the small wrapper helper.

When changing Lua behavior, verify the Lua itself, not only Rust compilation:

- `find companion/mod/claude-interface -name '*.lua' -print0 | xargs -0 luac -p`
- `cargo test --test lua_golden`
- live Factorio/RCON smoke for any changed remote that touches Factorio state

Avoid same-line trailing comments in generated Lua snippets. Comment-only lines
are stripped before RCON execution, but `code -- comment` can break after the
line-join step.
