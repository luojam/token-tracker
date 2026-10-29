use std::{
    fmt::Write as _,
    io::{self, Write as _},
};

use super::{CliError, args::ReportArgs, config::Config, reporting::*};
use token_tracker::{
    AGENT_LABELS, TokenTracker, TokenTrackerConfig,
    domain::{ReportFilters, ReportingPeriod},
    remote::{HttpRemote, RemoteError, SnapshotCache, combine_snapshots},
};

fn remote(config: &Config) -> Result<HttpRemote, CliError> {
    let url = config.remote_url.as_deref().ok_or_else(|| {
        CliError::Remote(RemoteError::Invalid("set remote_url in config.toml".into()))
    })?;
    let token = Config::remote_token()?;

    HttpRemote::new(url, &token).map_err(CliError::Remote)
}

pub(super) fn sync(config: &Config) -> Result<(), CliError> {
    let remote = remote(config)?;
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        machine_name: config.machine_name.clone(),
        ..Default::default()
    })
    .map_err(CliError::Open)?;

    let (snapshot, imported) = tracker.sync_remote(&remote).map_err(CliError::Remote)?;
    for warning in imported.warnings {
        eprintln!("Warning: {}", escape_control_characters(&warning.message));
    }

    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| CliError::Remote(error.into()))?
        .len();
    writeln!(
        io::stdout().lock(),
        "Synced {}: {} daily rows, {} bytes",
        snapshot.machine_id,
        snapshot.rows.len(),
        bytes
    )
    .map_err(CliError::Output)
}

pub(super) fn report(
    config: &Config,
    args: &ReportArgs,
    period: ReportingPeriod,
    summary: bool,
) -> Result<(), CliError> {
    let remote = remote(config)?;
    let data = token_tracker::storage::default_database_path().map_err(CliError::Open)?;
    let cache = SnapshotCache::new(&data.with_file_name("remote-cache"), remote.endpoint());
    let snapshots = cache.refresh(&remote).map_err(CliError::Remote)?;

    let report = combine_snapshots(
        &snapshots,
        period.current_range(),
        &ReportFilters {
            agents: args.agents.clone(),
            providers: args.providers.clone(),
            models: args.models.clone(),
        },
    )
    .map_err(CliError::Remote)?;

    let mut output = format!(
        "Token Tracker — Remote — {}\n\n{}",
        period.label(),
        render_summary(&report)
    );

    let width = super::terminal_width();
    if !summary {
        render_usage_breakdown(&mut output, &report, AGENT_LABELS, width, false);
    }

    render_estimate_diagnostics(&mut output, &report.totals.estimates);

    writeln!(output, "\nUploaded snapshots ({}):", snapshots.len()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    for snapshot in snapshots {
        let timestamp =
            chrono::DateTime::from_timestamp_millis(snapshot.generated_at_unix_ms).unwrap();
        let age = now.saturating_sub(timestamp.timestamp());
        let age = if age < 0 {
            "clock is ahead".into()
        } else {
            format!("{}h {}m old", age / 3600, age % 3600 / 60)
        };

        let name = snapshot
            .machine_name
            .as_deref()
            .unwrap_or(&snapshot.machine_id);
        writeln!(
            output,
            "- {} [{}]: {} ({age})",
            escape_control_characters(name),
            snapshot.machine_id,
            timestamp.to_rfc3339()
        )
        .unwrap();
    }

    io::stdout()
        .lock()
        .write_all(wrap_report(output, width).as_bytes())
        .map_err(CliError::Output)
}
