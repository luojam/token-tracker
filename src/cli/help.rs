const REPORT_OPTIONS: &str = "Options:
    --agent <id>        Filter: codex, claude, pi, hermes
    --provider <name>   Filter by stored provider name
    --model <name>      Filter by stored model name
    --server [<url>]    Fetch combined server totals (requires an auth file)
    --auth-file <path>  Bearer-token file for server requests
";

const CONFIG: &str = "Config: ~/.config/token-tracker/config.toml
Command-line values take precedence over config settings.
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
    upload [<url>] [--auth-file <path>]  Upload all retained usage to a server
    export <path>                        Export all retained usage to SQLite

{REPORT_OPTIONS}    -h, --help          Show help; also works after a command

{CONFIG}
Local reports, exports, and uploads refresh sources automatically.
With --server, reports use combined usage data stored on the server.

Examples:
    token-tracker
    token-tracker week --agent codex
    token-tracker --server https://tracker.example.com --auth-file auth.token
    token-tracker upload https://tracker.example.com --auth-file auth.token
"
        ),
        "day" | "week" | "month" | "summary" | "--server" | "--agent" | "--provider"
        | "--model" => {
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
Local reports refresh sources automatically.
With --server, reports use combined usage data stored on the server.

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
        "upload" => format!(
            "Usage: token-tracker upload [<url>] [--auth-file <path>]

Refresh sources and upload all retained usage to a server.

Options:
    --auth-file <path>  Bearer-token file
    -h, --help          Show help

{CONFIG}"
        ),
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
