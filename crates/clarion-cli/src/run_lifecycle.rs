use anyhow::{Context, Result};
use clarion_storage::{Writer, commands::WriterCmd};

pub(crate) async fn begin_run(
    writer: &Writer,
    run_id: &str,
    analyze_config_json: &str,
    started_at: &str,
    head_commit: Option<&str>,
) -> Result<()> {
    writer
        .send_wait(|ack| WriterCmd::BeginRun {
            run_id: run_id.to_owned(),
            config_json: analyze_config_json.to_owned(),
            started_at: started_at.to_owned(),
            head_commit: head_commit.map(str::to_owned),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("BeginRun")
}

/// Reopen an existing run for `--resume` (REQ-FINDING-05): the writer reuses
/// the prior run's row instead of inserting a fresh one (which would conflict
/// on the run PK). See [`WriterCmd::ResumeRun`].
pub(crate) async fn resume_run(writer: &Writer, run_id: &str) -> Result<()> {
    writer
        .send_wait(|ack| WriterCmd::ResumeRun {
            run_id: run_id.to_owned(),
            ack,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("ResumeRun")
}

/// Open the analyze run: reopen an existing row when `resume` is set, else
/// begin a fresh run. Centralises the begin-vs-resume choice so every
/// run-opening call site (including the discovery-error and no-plugins early
/// exits) honours `--resume` uniformly.
pub(crate) async fn open_run(
    writer: &Writer,
    resume: bool,
    run_id: &str,
    analyze_config_json: &str,
    started_at: &str,
    head_commit: Option<&str>,
) -> Result<()> {
    if resume {
        // Resume reuses the existing run row (and its original
        // `analyzed_at_commit`); the prior-run base must not shift mid-resume.
        resume_run(writer, run_id).await
    } else {
        begin_run(writer, run_id, analyze_config_json, started_at, head_commit).await
    }
}
