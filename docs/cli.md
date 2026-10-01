# CLI

See the [quickstart](../README.md#quick-start) for installation.

## Commands

Local reports, exports, and uploads refresh sources automatically.

| Command | Behavior |
| --- | --- |
| `token-tracker` | Show the full all-time usage report. |
| `token-tracker day`, `week`, or `month` | Report the current UTC calendar period. Weeks start Monday. |
| `token-tracker summary` | Show total, input, output, and cache tokens plus total cost. |
| `token-tracker [day\|week\|month] --server [<server-url>] [--auth-file <path>]` | Fetch combined server totals for the period, or all time if omitted. |
| `token-tracker doctor` | Check configuration, storage paths, source access, and import issues without changing local data. Exit unsuccessfully if issues are found. |
| `token-tracker export <path> [--force]` | Export usage to SQLite. |
| `token-tracker upload [<server-url>] [--auth-file <path>]` | Upload usage to a server. |
| `token-tracker --help` or `-h` | Show command usage. |

Add `--server [<server-url>]` to the default report, `day`, `week`, `month`, or
`summary` to fetch combined server totals without accessing local usage.
Server reports show totals rather than the local report's breakdowns.
`summary` is all-time; use `day`, `week`, or `month` for a period.

## Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--agent <id>` | Reports | Filter by source: `codex`, `claude`, `pi`, or `hermes`. Repeatable. |
| `--provider <name>` | Reports | Filter by stored provider name. Repeatable. |
| `--model <name>` | Reports | Filter by stored model name. Repeatable. |
| `--server [<server-url>]` | Reports | Fetch server totals; omitted URL uses configuration. |
| `--auth-file <path>` | Server reports, upload | Read the bearer token from this file; overrides configuration. |
| `--force` | Export | Replace an existing Token Tracker export; unrelated files are never overwritten. |
| `--` | Export | Treat following arguments as paths, including names beginning with `-`. |

The export's parent directory must already exist. Filters apply to reports only;
exports and uploads always include all retained usage.

## Filters

Matching is exact and case-sensitive. Repeat a flag to match any of its values;
different flags combine with AND. Unknown values return no matching usage.
Provider/model names are not a fixed list; use names shown in the local report's
breakdown. Usage without a provider or model does not match that filter.

```sh
token-tracker --agent codex --agent pi
token-tracker week --provider openai --model gpt-5.6-sol
token-tracker summary --server --agent claude
```

Filters affect totals and local breakdowns. Filtered session counts include only
sessions containing matching events. Import warnings still cover all sources.

## Configuration

Local reports, exports, and uploads create `~/.config/token-tracker/config.toml`
if missing. An absolute `XDG_CONFIG_HOME` replaces `~/.config`. The generated
file has empty values, which mean unset.

| Key | Meaning |
| --- | --- |
| `machine_name` | Optional label included in exports and uploads; not the machine identity. |
| `server_url` | Default URL for upload and server reports. |
| `auth_file` | Path to the bearer-token file. |

For example, edit the file to use your server and an absolute token-file path:

```toml
machine_name = "laptop"
server_url = "https://tracker.example.com"
auth_file = "/home/alice/.config/token-tracker/auth.token"
```

Paths in TOML do not expand `~` or environment variables. Command-line URL and
auth-file values override saved defaults. Both must be supplied through flags or
configuration for server requests. Saved server settings apply to uploads and
reports using `--server`; other reports remain local.

Server requests require HTTPS, except on loopback. The token must be in a regular
file accessible only by its owner (`chmod 600`). See
[client setup](deployment.md#connect-clients) to obtain it.

## Sources and local data

| Source | Session location and overrides |
| --- | --- |
| Pi | `~/.pi/agent/sessions`. `PI_CODING_AGENT_SESSION_DIR` takes precedence; otherwise `sessionDir` in project `.pi/settings.json`, then agent `settings.json`, can override it. `PI_CODING_AGENT_DIR` changes the agent directory. |
| Codex | `~/.codex/sessions` and `~/.codex/archived_sessions`; `CODEX_HOME` changes the base directory. |
| Claude Code | `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`. |
| Hermes | `~/.hermes/state.db` and `~/.hermes/profiles/*/state.db`, or `$HERMES_HOME/state.db` when set. |

Usage is stored at `~/.local/share/token-tracker/usage.db`; an absolute
`XDG_DATA_HOME` replaces `~/.local/share`. Only usage and session metadata are
stored. Imported usage remains after source sessions or databases are deleted.

`machine-state.db` beside the usage database stores the export/upload identity
and revision. Preserve it when retaining the same machine's upload identity;
`machine_name` is only a label.

Costs use recorded values where available and API-price estimates otherwise.
For subscription plans, these are API-equivalent costs, not your subscription bill.
