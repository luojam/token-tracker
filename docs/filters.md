# Report filters

Use these with the default report, `day`, `week`, `month`, or `summary`,
locally or with `--server`:

| Filter | Value |
| --- | --- |
| `--agent <id>` | `codex`, `claude`, `pi`, or `hermes` for bundled sources. |
| `--provider <name>` | Provider as stored in usage, such as `openai` or `anthropic`. |
| `--model <name>` | Exact model name as stored in usage. |

Matching is case-sensitive. Repeat a flag to match any of its values; different
flags combine with AND. Unknown values return no matching usage. Provider/model
names are not a fixed list; use names shown in the local report's breakdown.
Events without attribution do not match provider or model filters.

```sh
token-tracker --agent codex --agent pi
token-tracker week --provider openai --model gpt-5.6-sol
token-tracker summary --server --agent claude
```

Filters affect totals and local breakdowns. Filtered session counts include only
sessions containing matching events. Import warnings still cover all sources.
Server reports show totals from uploaded usage and require an updated server.

The server API accepts repeated query parameters with the same rules:
`/summary?period=week&agent=codex&agent=pi&provider=openai`.
