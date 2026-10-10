//! End-to-end smoke test: runs one real agent turn and prints the event stream.

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let prompt = args.next().unwrap_or_else(|| {
        "Create hello.py that prints the first 10 Fibonacci numbers, then run it and show the output."
            .into()
    });
    let workdir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("strawberry-smoke"));
    let model = std::env::var("SMOKE_MODEL").ok();

    if let Err(e) = strawberry_agent_lib::smoke::run(&prompt, workdir, model).await {
        eprintln!("smoke failed: {e}");
        std::process::exit(1);
    }
}
