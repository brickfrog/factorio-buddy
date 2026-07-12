fn main() {
    // Keep the build side-effect free. The previous Python runtime consumed a
    // generated tool manifest; the Rust MCP server owns its tool definitions
    // directly and needs no generated companion artifacts.
}
