const REPORT_OPTIONS: &str = "Report options:
    --agent <id>        Filter: codex, claude, pi, hermes
    --provider <name>   Filter by stored provider name
    --model <name>      Filter by stored model name
    --server [<url>]    Fetch combined server totals without reading local usage
    --auth-file <path>  Bearer-token file for server requests
";

const FILTERS: &str = "Filters are exact and case-sensitive.
Repeat a filter to match any value; different filters combine with AND.
Filters apply only to reports.
";

const CONFIG: &str = "Config: ~/.config/token-tracker/config.toml
An absolute XDG_CONFIG_HOME replaces ~/.config.
Set the server URL and token-file path through arguments or config.
Arguments override config. HTTPS is required except on loopback.
";

pub(super) fn for_command(command: &str) -> Option<String> {
    let help = match command {
        "--help" | "-h" => format!(
            "Usage: token-tracker [command] [options]

Commands:
    (none)              All-time usage report
    day | week | month  Current UTC period; weeks start Monday
    summary             All-time token totals by type and total cost
    doctor              Check config, storage, and sources without changing data
    export <path>       Export all retained usage to SQLite
    upload [<url>]      Upload all retained usage to a server

Local reports, exports, and uploads refresh sources automatically.

{REPORT_OPTIONS}
{FILTERS}
Other options:
    --force            Replace an existing Token Tracker export (export only)
    --                 Allow export paths starting with '-'
    -h, --help          Show help; also works after a command

{CONFIG}
Examples:
    token-tracker week --agent codex
    token-tracker summary --server
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

{REPORT_OPTIONS}    -h, --help          Show help

{FILTERS}
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
