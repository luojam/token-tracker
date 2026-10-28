const REPORT_OPTIONS: &str = "Options:
    --agent <id>        Filter: codex, claude, pi, hermes
    --provider <name>   Filter by stored provider name
    --model <name>      Filter by stored model name
";

const CONFIG: &str = "Config: ~/.config/token-tracker/config.toml
";

pub(super) fn for_command(command: &str) -> Option<String> {
    let help = match command {
        "--help" | "-h" => format!(
            "Usage: token-tracker [command] [options]

Commands:
    (none) [options]                     All-time usage report
    day | week | month [options]         Current UTC period; weeks start Monday
    summary [options]                    All-time tokens by type and total cost
    doctor                               Check config, storage, and sources
    export <path>                        Export all retained usage to SQLite

{REPORT_OPTIONS}    -h, --help          Show help; also works after a command

{CONFIG}
Reports and exports refresh sources automatically.

Examples:
    token-tracker
    token-tracker week --agent codex
"
        ),
        "day" | "week" | "month" | "summary" | "--agent" | "--provider" | "--model" => {
            let (command, description) = match command {
                "summary" => (" summary", "All-time token totals by type and total cost."),
                "day" => (" day", "Usage for the current UTC day."),
                "week" => (" week", "Usage for the current UTC week, starting Monday."),
                "month" => (" month", "Usage for the current UTC month."),
                _ => ("", "All-time usage report."),
            };
            format!(
                "Usage: token-tracker{command} [options]

{description}
Reports refresh sources automatically.

{REPORT_OPTIONS}    -h, --help          Show help

{CONFIG}"
            )
        }
        "export" => "Usage: token-tracker export <path> [--force]

Refresh sources and export all retained usage to SQLite.
The parent directory must exist.

Options:
    --force     Replace an existing Token Tracker export
    --          Allow paths starting with '-'; put --force before this
    -h, --help  Show help

Example: token-tracker export usage.db
"
        .to_owned(),
        "doctor" => "Usage: token-tracker doctor

Check config, storage, and sources without changing local data.
Exit unsuccessfully if issues are found.

    -h, --help  Show help
"
        .to_owned(),
        _ => return None,
    };
    Some(help)
}
