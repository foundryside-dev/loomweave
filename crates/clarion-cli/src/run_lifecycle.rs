use anyhow::{Context, Result};
use clarion_storage::{Writer, commands::WriterCmd};

pub(crate) async fn begin_run(
    writer: &Writer,
    run_id: &str,
    analyze_config_json: &str,
    started_at: &str,
) -> Result<()> {
    writer
        .send_wait(|ack| WriterCmd::BeginRun {
            run_id: run_id.to_owned(),
            config_json: analyze_config_json.to_owned(),
            started_at: started_at.to_owned(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("BeginRun")
}
