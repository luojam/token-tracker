# CLI

See the [quickstart](../README.md#quick-start) for installation.

## Commands

Reports and exports refresh sources automatically.

| Command | Behavior |
| --- | --- |
| `token-tracker` | Show the full all-time usage report. |
| `token-tracker day`, `week`, or `month` | Report the current UTC calendar period. Weeks start Monday. |
| `token-tracker summary` | Show total, input, output, and cache tokens plus total cost. |
| `token-tracker doctor` | Check configuration, storage paths, source access, and import issues without changing local data. Exit unsuccessfully if issues are found. |
| `token-tracker export <path> [--force]` | Export usage to SQLite. |
| `token-tracker --help` or `-h` | Show command usage. |

Use `<command> --help` (or `-h`) for help with a specific command.

`summary` is all-time; use `day`, `week`, or `month` for a period.

## Options

| Option | Applies to | Meaning |
| --- | --- | --- |
| `--agent <id>` | Reports | Filter by source: `codex`, `claude`, `pi`, or `hermes`. Repeatable. |
| `--provider <name>` | Reports | Filter by stored provider name. Repeatable. |
| `--model <name>` | Reports | Filter by stored model name. Repeatable. |
| `--force` | Export | Replace an existing Token Tracker export; unrelated files are never overwritten. |
| `--` | Export | Treat following arguments as paths, including names beginning with `-`. |

The export's parent directory must already exist. Filters apply to reports only;
exports always include all retained usage.

## Filters

Matching is exact and case-sensitive. Repeat a flag to match any of its values;
different flags combine with AND. Unknown values return no matching usage.
Provider/model names are not a fixed list; use names shown in the local report's
breakdown. Usage without a provider or model does not match that filter.

```sh
token-tracker --agent codex --agent pi
token-tracker week --provider openai --model gpt-5.6-sol
token-tracker summary --agent claude
```

Filters affect totals and local breakdowns. Filtered session counts include only
sessions containing matching events. Import warnings still cover all sources.

## Configuration

Reports and exports create `~/.config/token-tracker/config.toml`
if missing. An absolute `XDG_CONFIG_HOME` replaces `~/.config`. The generated
file has empty values, which mean unset.

| Key | Meaning |
| --- | --- |
| `machine_name` | Optional label included in exports; not the machine identity. |

For example, set a label for this machine:

```toml
machine_name = "laptop"
```

## Sources and local data

| Source | Session location and overrides |
| --- | --- |
| Pi | `~/.pi/agent/sessions`. `PI_CODING_AGENT_SESSION_DIR` takes precedence; otherwise `sessionDir` in project `.pi/settings.json`, then agent `settings.json`, can override it. `PI_CODING_AGENT_DIR` changes the agent directory. |
| Codex | `~/.codex/sessions` and `~/.codex/archived_sessions`; `CODEX_HOME` changes the base directory. |
| Claude Code | `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`. |
| Hermes | `~/.hermes/state.db` and `~/.hermes/profiles/*/state.db`, or `$HERMES_HOME/state.db` when set. |

Codex imports both `.jsonl` and compressed `.jsonl.zst` rollouts. If both exist,
the plain file takes precedence.

Usage is stored at `~/.local/share/token-tracker/usage.db`; an absolute
`XDG_DATA_HOME` replaces `~/.local/share`. Only usage and session metadata are
stored. Imported usage remains after source sessions or databases are deleted.

`machine-state.db` beside the usage database stores the export identity
and revision. Preserve it when retaining the same machine's export identity;
`machine_name` is only a label.

Costs use recorded values where available and API-price estimates otherwise.
For subscription plans, these are API-equivalent costs, not your subscription bill.
