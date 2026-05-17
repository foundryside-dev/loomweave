use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result, anyhow, ensure};
use clarion_storage::ReaderPool;

pub fn run(path: &Path) -> Result<()> {
    let db_path = path.join(".clarion").join("clarion.db");
    ensure!(
        db_path.exists(),
        "Clarion database not found at {}; run `clarion install --path {}` first",
        db_path.display(),
        path.display()
    );

    let _readers = ReaderPool::open(&db_path, 16)
        .map_err(|err| anyhow!("open reader pool for {}: {err}", db_path.display()))?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    clarion_mcp::serve_stdio(&mut reader, &mut writer).context("serve MCP stdio")
}
