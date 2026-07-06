#[path = "src/tool_metadata_data.rs"]
mod tool_metadata_data;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tool_metadata_data::FACTORIO_MCP_TOOLS;

fn main() {
    println!("cargo:rerun-if-changed=src/tool_metadata_data.rs");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target = manifest_dir
        .join("companion")
        .join("bridge")
        .join("generated")
        .join("factorio_tool_metadata.json");
    write_if_changed(&target, &render_tool_metadata()).expect("write tool metadata");
}

fn render_tool_metadata() -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str("  \"schema_version\": 1,\n");
    out.push_str("  \"source\": \"src/tool_metadata_data.rs\",\n");
    out.push_str("  \"tools\": [\n");
    for (index, tool) in FACTORIO_MCP_TOOLS.iter().enumerate() {
        let comma = if index + 1 == FACTORIO_MCP_TOOLS.len() {
            ""
        } else {
            ","
        };
        out.push_str(&format!(
            "    {{\"name\": \"{}\", \"mutating\": {}, \"read_only\": {}, \"dry_run_safe\": {}}}{}\n",
            tool.name, tool.mutating, tool.read_only, tool.dry_run_safe, comma
        ));
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn write_if_changed(path: &Path, contents: &str) -> io::Result<()> {
    if matches!(fs::read_to_string(path), Ok(existing) if existing == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}
