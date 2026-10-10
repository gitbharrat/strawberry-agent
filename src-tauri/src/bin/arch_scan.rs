//! Dev utility: rebuild a workspace's architecture map from the terminal.
//!
//! Static skeleton only — files, functions, graph, quality. Model
//! annotations are added by the app, which owns the API key.
//!
//! Usage: cargo run --bin arch_scan -- /path/to/workspace

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: arch_scan <workspace-path>")?;
    let root = std::path::PathBuf::from(path);
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()).into());
    }

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let doc = rt.block_on(strawberry_agent_lib::analysis::rescan_static(&root))?;

    println!("{} files · {} functions · {} edges", doc.file_count, doc.fn_count, doc.edges.len());
    println!(
        "written to {}/{}/{{architecture.json, architecture.md}}",
        root.display(),
        strawberry_agent_lib::analysis::DIR
    );
    Ok(())
}
