use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    version,
    about = "Show all-time usage, or select a command",
    args_conflicts_with_subcommands = true,
    after_help = "Reports and exports refresh sources automatically.\n\
        Config: ~/.config/token-tracker/config.toml\n\n\
        Examples:\n    token-tracker\n    token-tracker week --agent codex"
)]
pub(super) struct Cli {
    #[command(flatten)]
    pub filters: ReportArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub(super) enum Command {
    #[command(about = "Usage for the current UTC day")]
    Day(ReportArgs),
    #[command(about = "Usage for the current UTC week, starting Monday")]
    Week(ReportArgs),
    #[command(about = "Usage for the current UTC month")]
    Month(ReportArgs),
    #[command(about = "All-time tokens by type and total cost")]
    Summary(ReportArgs),
    #[command(
        about = "Check config, storage, and sources without changing local data",
        after_help = "Exit unsuccessfully if issues are found."
    )]
    Doctor,
    #[command(
        about = "Refresh sources and export all retained usage to SQLite",
        after_help = "The parent directory must exist.\n\
            Use -- before paths starting with '-'; put --force before this.\n\n\
            Example: token-tracker export usage.db"
    )]
    Export {
        path: PathBuf,
        #[arg(long, help = "Replace an existing Token Tracker export")]
        force: bool,
    },
}

#[derive(Args)]
#[command(
    after_help = "Reports refresh sources automatically.\nConfig: ~/.config/token-tracker/config.toml"
)]
pub(super) struct ReportArgs {
    #[arg(long = "agent", value_name = "ID", value_parser = filter_value, help = "Filter: codex, claude, pi, hermes")]
    pub agents: Vec<String>,
    #[arg(long = "provider", value_name = "NAME", value_parser = filter_value, help = "Filter by stored provider name")]
    pub providers: Vec<String>,
    #[arg(long = "model", value_name = "NAME", value_parser = filter_value, help = "Filter by stored model name")]
    pub models: Vec<String>,
}

fn filter_value(value: &str) -> Result<String, &'static str> {
    if value.trim().is_empty() {
        Err("value must not be blank")
    } else {
        Ok(value.to_owned())
    }
}
