# Factorioctl Contributor Notes

## Lua/RCON Architecture

Normal game traffic must go through the Factorio mod's `/claude` dispatcher:
one JSON console command shaped like `{"fn":"remote_name","args":[...],"n":3}`.
The dispatcher and `remote.add_interface("claude_interface", api)` share the
same `api` table, so Rust wrappers and mod remotes stay under one contract.

`src/client/lua.rs` is legacy in name only. Its normal request builders should
emit `/claude` envelopes, not inline Lua, `remote.call(...)` snippets, or
argument-escaped Lua strings. `execute_lua()` / CLI `exec` are debug hatches for
trusted operator use only and must stay off the normal tool path.

The Rust buddy runtime owns model lifecycle and chat ingress. It must not
duplicate gameplay logic from the MCP server or speak a parallel game protocol.
Inbound chat remains the mod-written `script-output` JSONL file.

Do not add new Factorio gameplay logic as inline Rust string literals. If code
scans entities, resources, tiles, inventories, recipes, technologies, belts,
fluid boxes, electric networks, prototypes, or entity statuses, put that logic in
`mod/claude-interface/control.lua` behind a `claude_interface` remote
function and call it from Rust through the small wrapper helper.

When changing mod behavior, verify the Lua itself, not only Rust compilation:

- `find mod/claude-interface -name '*.lua' -print0 | xargs -0 luac -p`
- `cargo test --test lua_golden`
- live Factorio/RCON smoke for any changed remote that touches Factorio state

Avoid adding generated Lua snippets to production paths. If raw Lua is truly
needed for debugging or disposable smoke setup, require the explicit raw-Lua
operator opt-in and keep it out of agent/MCP automation.
