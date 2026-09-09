# Claude format contract and fixtures (CL01)

Authored 2026-09-09 for [the implementation plan](../../../docs/claude-plan.md).
These are invented, minimal records, not copied or sanitized private transcripts.
Conversation and tool text is omitted; content blocks retain only shape markers.
The fixtures require no Claude executable, reference directory, network, or database.
Producer observations and API documentation are separated in [SOURCES.md](SOURCES.md).
No Claude parser or pricing implementation is supplied by this slice.

## Supported records

Read `type: "assistant"`, with response fields under `message` and counters under
`message.usage`. A nonempty `message.stop_reason` plus valid required counters
marks a complete snapshot. Null/absent/empty stop reason is unfinished. This is a
persisted transcript rule; live SDK placeholder guidance is not a transcript parser.
The observed final reasons are `tool_use` and `end_turn`; ordinary final refusals
also count. Client `version` is evidence, not a compatibility gate or format version.

Group `message.id` across the entire file. Emit one `Assistant` event in namespace
`claude`, keyed `response-v1:<message.id>`. Select the last complete snapshot and
retain the first complete snapshot's timestamp. Neither provisional snapshots nor
component-wise maxima may replace it. Identical repeats across content blocks add
nothing; different response IDs with equal counts both count. Request IDs, when
present, must agree; model disagreements for the same response also reject the
whole file. Missing model information retains unattributed usage. Outer UUIDs,
timestamps, session IDs, and paths are not usage keys.

All four counters (`input_tokens`, `output_tokens`, `cache_read_input_tokens`,
`cache_creation_input_tokens`) must be nonnegative integers representable in the
core counter type. Missing/malformed required counters or arithmetic overflow
reject an accounting record, including a complete snapshot; never coerce them.
Unfinished records with valid counters produce no usage. Count notices by distinct
unfinished response IDs, not by rows. A final for that ID clears its omission.

Synthetic suppression requires the explicit `message.model: "<synthetic>"` shape
and zero usage, not merely a non-`msg_` identifier or zero token counts. Ignore
`cost-state`, user/tool-result usage summaries, and compaction boundaries/context
sizes. They are not additional response usage. Validate JSON even on ignored rows.
Unknown metadata can precede useful records. A malformed complete JSON line rejects
the file, even if it contains only ignored fields. Only an EOF-truncated final JSON
value is tolerated, with `IncompleteFinalLine` and one truncated-tail notice.

## Session metadata and source paths

The fixture filenames describe scenarios. Use each expectation's absolute
`source_path` as `ParseContext.source_path` (or materialize that layout under a
temporary root for discovery tests), rather than parsing the descriptive filename
as a session ID. Supported layouts are `<project>/<session-id>.jsonl` and
`<project>/<session-id>/subagents/**/agent-<agent-id>.jsonl`.

Camel-case `sessionId` must agree with the owning main-session filename/directory.
Subagents also require their own `agentId`, matching the filename, and normalize to
`subagent-v1:<session-id>:<agent-id>` with `ParentSession::SessionId` referring to
that main session. Components must be nonempty and exclude separators/colons so
this encoding cannot collide; these fixtures use UUID sessions and hex agent IDs.
Both children deliberately share the main `sessionId` and differ in `agentId`.
The parent need not be present. Neither `parentUuid` nor snake-case `session_id`
establishes session identity or ancestry. Copied main history implies no parent.

Use the earliest available record timestamp and working directory, even if metadata
appears later in file order. Do not use filesystem times or reconstruct cwd from a
project slug. `snapshots` places its earlier metadata after an assistant row.
All expected `format_version` and names are null. A metadata-only source creates a
zero-event session; missing required identity/timestamp rejects through the normal
file error path. Old working directories do not need to exist.

## Token and pricing facts

Expected vectors use `(input, cache_read, cache_write, output)`, matching the Codex
fixture convention. These categories are disjoint; thinking is already in output.
Every event has `recorded_cost: null`. Canonical `claude-*` IDs use `anthropic`;
opaque IDs use `unknown` and remain unpriced. Do not guess deployment aliases.

`usage.iterations` absent/null uses the top-level counters once. A nonempty array
uses its components once, including compaction, without adding top-level counts.
Support `message`/`compaction`, with model omitted or matching the response model.
Validate top-level input/output against the sum of non-compaction iterations, as
documented. For this initial supported shape, also require the reported top-level
cache counts to agree with non-compaction components; that extension is an adapter
validation policy supported by the observed single-message records. Empty arrays,
unknown types, and mixed-model iterations omit that response with an unsupported
accounting notice. A known-shape counter disagreement rejects the file.

`cache_creation.ephemeral_5m_input_tokens` and `ephemeral_1h_input_tokens` describe
cache writes, not additional tokens. When supplied, both must be valid counters
and sum to that component's cache writes. A mismatch rejects the file. Missing/null
duration detail retains tokens and is represented as null; positive writes without
it make the whole estimate unavailable. Zero writes do not require a guessed TTL.
Keep per-iteration detail; the compaction fixture's top-level duration breakdown
belongs to its message component, not the response aggregate.

Preserve `usage.speed` and `usage.service_tier` independently. The fixture oracle
explicitly distinguishes missing, null, and string values. Only supported served
values can be priced; standard tier alone cannot establish standard speed.
Missing/unsupported pricing facts produce no parse omission notice because their
tokens are retained. `pricing_facts` describes the CL04 interface requirements, not
an already implemented Rust or SQLite encoding.

## Fixture oracle

[expectations.json](expectations.json) is hand-authored accounting truth, independent
of the future parser/calculator. It specifies every session, event, timestamp,
vector, pricing component, notice, total, and completion/error result. Array order
is only presentation: compare events by key. Component order follows iterations.
Notice/error names are semantic labels for CL03/CL05, not prescribed Rust variants;
optional notice line references are deliberately not prescribed. Error line numbers
are one-based. Rejected files return no session/events/notices and no total; their
prior successful imports must survive. `invalid_json_lines` lists the only intended
JSON syntax failures; all other JSON must parse, including semantically bad records.

| Fixture | Main purpose | Tokens / outcome |
| --- | --- | --- |
| `snapshots` | Nonadjacent block repeats, equal-count distinct ID, placeholder/final/correction/trailing placeholder; first-final timestamp | 333, two events |
| `shared-history` | Same response copied under changed outer UUID/time/session; one new response | 168, two events |
| `child-a1b2c3d`, `child-b2c3d4e` | Distinct children with shared parent, including nested path | 5 each |
| `partial-ignored` | Two unfinished IDs, synthetic entry, cumulative/tool/context exclusions, ordinary refusal | 5; two incomplete-response notices counted together |
| `cache-iterations` | Mixed/5m/1h/missing durations, fast speed, compaction, null/missing/unknown pricing evidence | 410, eleven events |
| `unsupported` | Unknown iteration type, mixed model, empty iterations alongside valid usage | 5; three unsupported responses |
| `metadata-only` | Metadata after ignored row, no assistant usage | 0, one session |
| `truncated-tail` | Good response followed by truncated JSON without newline | 5; incomplete final line and one tail notice |
| `reject-malformed` | Invalid syntax in a complete ignored record | Reject line 3 |
| `reject-model`, `reject-request` | Reused response with conflicting identity | Reject line 3 |
| `reject-duration` | Cache subdivision sums to 3, aggregate writes 4 | Reject line 3 |
| `reject-counter` | Negative required output counter | Reject line 3 |
| `reject-iteration-total` | Known message input disagrees with top-level input | Reject line 3 |

The `prefixes` oracle checks snapshots at 3/4/6/7/8 complete lines: zero usage with
one omission, first final, repeats/equal ID, correction, unchanged after trailing
placeholder. Prefixes use the same session/source metadata and complete syntax.
The four-file `combined_import` oracle expects four sessions, five unique events,
and 348 tokens regardless of import order. Shared complete observations agree on
usage; selecting canonical ownership still uses existing generic precedence.
These are CL05/CL06 acceptance inputs, not claims of passing adapter tests today.

The independent price oracle is Opus 5 standard speed/standard tier:

```text
input 10 + output 20 + reads 100 + writes (30 five-minute + 10 one-hour) = 170
(10 × $5 + 20 × $25 + 100 × $0.50 + 30 × $6.25 + 10 × $10) / 1,000,000
= $887.50 / 1,000,000 = $0.0008875 = 887,500,000 picodollars
```

The rates are the plan's 2026-09-09 snapshot, not a fresh CL07 pricing verification.
A later pricing change must not silently rewrite this dated numeric oracle.
