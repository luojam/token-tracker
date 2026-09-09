# Claude fixture evidence

Recorded 2026-09-09. The [plan](../../../docs/claude-plan.md#evidence) contains the
initial reference inspection and rate snapshot. No private transcript or text was
copied into fixtures. Values, IDs, paths, timestamps, and minimal block markers
were invented. Public docs describe API fields, not a stable Claude Code JSONL ABI.

## Local observations

The plan reports 111 files (107 main, four subagent), client versions 2.1.227 through
2.1.263, 2,640 real response IDs, 83 IDs shared across files, 73 IDs with changing
snapshots, five without any final snapshot, nine files without assistant usage,
and 70 files with cumulative `cost-state`. These are sample facts, not guarantees.

A CL01 read-only scan of assistant metadata/counter shapes found:

- 5,457 real assistant rows: 5,352 with nonempty stop reasons (`tool_use` or
  `end_turn`) and 105 with null stop reasons. All 5,352 finals had the four valid
  nonnegative integer counters, served speed `standard`, and tier `standard`.
- Each real final had one `message` iteration whose four counters matched the
  top-level counters. Iterations contained `type`, the four counters, and
  `cache_creation`; no iteration model field or compaction iteration was observed.
- Six additional rows used model `<synthetic>` and stop reason `stop_sequence`.
  Their zero usage is recorded in the plan. Their response identifiers were not
  ordinary API response IDs; do not generalize that into an ID-prefix filter.
- Cache duration fields, output thinking detail, camel-case session identity,
  separate snake-case session identity, and child agent identity support the
  minimal shapes used here. Finalization depends on structure, not client version.

This verifies the selected complete-snapshot shape. It does not prove every
historical transcript follows it. Full corrections and malformed/conflicting rows
in the fixtures are deliberate policy tests, not claims about producer behavior.

## Public API and storage evidence

Primary sources opened during CL01:

- [Session storage](https://code.claude.com/docs/en/sessions#where-transcripts-are-stored)
  describes project JSONL storage and the internal nature of the transcript format.
- [SDK cost tracking](https://code.claude.com/docs/en/agent-sdk/cost-tracking)
  explains response-ID deduplication across emitted content blocks. SDK stream
  placeholders are a separate surface; local persisted rows establish the fixture
  finalization rule.
- [Compaction usage](https://platform.claude.com/docs/en/build-with-claude/compaction#understanding-usage)
  says to sum iteration usage, including compaction, and states that top-level
  input/output sum only non-compaction iterations. The `msg_compaction` fixture
  combines that documented behavior with invented cache values. Replayed summary
  blocks alone are not evidence of another compaction request.
- [Messages API response fields](https://platform.claude.com/docs/en/api/beta/messages/create)
  gives cache-duration field names, optional iteration models, output thinking
  details, and distinct usage speed/service-tier fields. Generated API examples
  are shape evidence, not arithmetic oracles: some example counters/models are
  internally inconsistent. Our supported arithmetic is explicit in README.
- [Prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)
  documents separate cache writes/reads and cache lifetimes. Cache subdivision
  counters must not be added to aggregate writes again.

The plan additionally cites
[subagent transcript storage](https://code.claude.com/docs/en/sub-agents#resume-subagents),
[the changelog](https://code.claude.com/docs/en/changelog), and
[pricing](https://platform.claude.com/docs/en/about-claude/pricing).
Those prior references were not independently reverified in CL01. CL07 must recheck
rates and model IDs; the price oracle here uses the supplied dated plan snapshot.

## Adapter policy versus producer evidence

Response key encoding, first-final timestamp stability, last-final correction,
strict request/model consistency, skipping unfinished IDs, retained notices, and
whole-file rejection are deliberate adapter policies from the shared contract.
So are collision-free child IDs, missing-price handling, and exclusions of
cumulative/background summaries. None is presented as a producer guarantee.

Compaction and same-model iteration fields are API-backed extensions beyond the
local sample. Requiring matching non-compaction cache totals extends the observed
single-message equality conservatively; the compaction guide explicitly specifies
only input/output equality. Unknown/mixed/empty iteration shapes are unsupported
with notices; malformed counters and known-shape sum conflicts are errors. These
choices keep omitted accounting visible without inventing fallback totals.
