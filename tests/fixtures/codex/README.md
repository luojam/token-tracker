# Codex format evidence and fixtures (C01)

C01 complete, 2026-09-06. The supported accounting contract is settled below and
in [matched producer evidence](SOURCES.md). This slice supplies fixtures and
expected results; parser and retained-import implementation remain C03–C06.

All records were authored with invented IDs, timestamps, paths, and counters.
None was copied or sanitized from local sessions. Optional conversation/tool/
instruction fields are omitted. Empty compaction fields are synthetic placeholders
whose contents must be ignored. Human-readable fixture IDs stand for original
producer-issued IDs. Client versions identify observed shapes, not format versions
or blanket compatibility ranges.
Client versions are evidence labels, not parser mode switches: select accounting
from the records and their verified progression, without a version registry.

## Evidence and supported rules

The initial read-only 2026-09-06 scan found 201 local rollouts: 182 legacy-only,
14 response-plus-mirror, and 5 without usage; the archive directory was absent.
Files were live and grew between follow-up passes. Only field shapes, metadata,
counters, ordering, and ID equality were examined. No credentials, config,
history file, internal database, or personal text was output or retained.

Observed versions span `0.126.0-alpha.8` through `0.153.4`; the inspected legacy
fork is `0.140.0-alpha.2`, the three compaction records are in `0.128.0` and
`0.137.0-alpha.4` files, and all initial response-format files report `0.153.4`.
Matched release source for `0.128.0`, `0.140.0-alpha.2`, and `0.153.4` establishes
the rules that local samples alone could not prove. Exact commits, functions,
archive hashes, and source links are in [SOURCES.md](SOURCES.md).

### Legacy identity and epochs

Use `legacy-turn-v1:<original turn_id>` in namespace `codex`. A legacy turn has
one normalized aggregate, updated as its source prefix grows. First subtract
successive raw cumulative vectors, validate every delta, then normalize and sum
into that turn. Do not validate only the cumulative endpoints. Non-null explicit
zero request/delta vectors preserve a zero aggregate; null info and repeated
nonzero last-usage snapshots add nothing. The last vector is a check, never a
second additive stream.

This resolves the rejected completion-ordinal proposal: changing a completion's
counters to zero, inserting repeats, or correcting how many increments a turn
contains cannot shift its key or a later turn's key. Forks preserve original turn
IDs but rewrite outer timestamps. Do not use those timestamps, child IDs, paths,
response-item IDs assigned during copying, or token hashes as legacy identity.
Use the first task-start timestamp as observation time, never as part of the key.

Start from zero only for the evidenced complete header/start/context history
whose first accounting total equals last usage (including explicit zero), with
no earlier detached restore/checkpoint. Continue cumulative state across resumed
headers and fully copied fork history. An unchanged total with a last context-size
estimate contributes nothing. Reject detached initial baselines, cumulative
resets/decreases, invalid deltas, unknown accounting checkpoints, overlapping
legacy turns, or reuse of a closed turn ID. A final open turn may still contribute
its complete observed usage prefix. Missing lifecycle identity is unsupported.

Legacy events always have aggregate/unknown request granularity. Missing cache
write means incomplete detail, even though its unreported subdivision is represented
as zero to preserve known tokens. Do not price aggregate requests using a context
band inferred from a turn total. Later parser errors retain previous imports.

### Responses, upgrades, and mirrors

Use `response-v1:<response_id>` for each response's `usage`. Identical repeats
are no-ops; a last complete correction updates that response under the same key,
keeping its original thread/turn context. Conflicting identities reject. Cumulative
response and legacy vectors are validation data, not extra usage.
The correction fixture follows its original verified mirror; the correction does
not create a second pending mirror or alter that historical validation trace.

A supported upgrade finishes legacy turns, then starts a new turn with a response
record before its cumulative mirror. The matched producer persists the response
first and delays the mirror until pending tools finish. It separately restores
legacy totals and response totals. If no earlier response record exists, the
latter starts at zero. Preserve the fixed pre-upgrade raw legacy offset `L`:

```text
response.thread_token_usage = accumulated response usage R
mirror.total_token_usage = L + R
mirror.last_token_usage = pending response.usage
mirror.total_token_usage - previous confirmed mirror = pending response.usage
```

Use checked per-counter arithmetic, original thread/turn, and one pending response
to verify a mirror. At a new turn, the response turn accumulator starts over;
the thread accumulator continues. A pending response counts immediately at EOF.
An eventual mirror never adds an event or changes a key. Repeated unchanged totals
are no-ops. Reject unmatched progression/offsets, ambiguous multiple pending
responses, a legacy-first mirror, or an upgrade inside an already-accounted legacy
turn. Arbitrary rewrites that erase old identities are not supported conversions.
The source trace explains why synthetic equality by itself would not suffice.

### Owning thread, lineage, and pricing context

The first owning `session_meta.id` identifies the normalized session. A child can
have a different `session_meta.session_id` identifying the shared root session;
response `thread_id` matches the child, while response `session_id` matches that
root. Map explicit `parent_thread_id` or verified `forked_from_id` to a parent
session ID. The observed fork contains inherited parent headers after its own
header; keep original owner/start and distinguish those headers from malformed
identity changes. Reject conflicting parent references. `root_turn_id` relates
turns and must never become a parent session reference.

Provider comes from explicit owning metadata and model from the original turn
context. New settings events carry their thread ID. Matching old source establishes
that an unscoped event is that session's full snapshot; apply it only when the
original owner is unambiguous. Missing/null tier clears old fast evidence. Bind
requested settings at a subsequent matching turn boundary, never backfill them
or borrow the parent's setting for a child. Mid-turn changes leave affected tier
attribution unknown. No served tier or monetary charge was observed. Interleaving
multiple threads in a single file is not established by the separate child sample.

C09 conservatively leaves copied legacy ownership unknown until a repeated owning
header establishes local scope. Neither a completed/aborted inherited turn nor the
next task start proves that copying has ended: the partial-fork fixture has no
owner-transition header. Such observations retain tokens with unknown provider/tier;
a repeated child header before its turn context permits child model attribution,
but cannot retroactively bind settings at the preceding task start.

## Expected normalized facts

Vectors are `(ordinary input, cache read, cache write, output)`; totals sum all
four disjoint categories. Output includes reasoning. All events have kind `Other`
and recorded cost `None`. Fixture provider/model are explicitly
`openai / gpt-6-astra` where the owning metadata and turn context apply.

The following table is the parser oracle, not a claim that the parser exists.
`L(a)` means `legacy-turn-v1:turn-legacy-a`, `L(b)` means
`legacy-turn-v1:turn-legacy-b`, and `L(f)` means `legacy-turn-v1:turn-fork`.
`R(a)`, `R(b)`, `R(c)`, and `R(d)` mean `response-v1:response-a`,
`response-v1:response-b`, `response-v1:response-child`, and
`response-v1:response-distinct`. [expectations.json](expectations.json) contains
expanded keys and vectors for reuse by parser tests.

### Observed shapes with invented values

| Fixture | Expected events | Total | Context |
| --- | --- | ---: | --- |
| `legacy-fresh.jsonl` | `L(a)` = `(120,100,0,30)` | 250 | `0.128.0`; two increasing notifications plus repeat/null info; incomplete cache detail, unknown tier, aggregate granularity. |
| `legacy-resume-compaction.jsonl` | `L(a)` = `(120,100,0,30)`, `L(b)` = `(60,40,0,10)` | 360 | Same-ID resume; unchanged compaction total contributes nothing despite last context estimate 50. |
| `legacy-parent.jsonl` | `L(a)` = `(120,100,0,30)` | 250 | `0.140.0-alpha.2`; owner `thread-parent`. |
| `legacy-fork.jsonl` | `L(a)` = `(120,100,0,30)`, `L(f)` = `(60,40,0,10)` | 360 | Owner `thread-fork`, parent `thread-parent`; copied payloads retain original turn IDs with rewritten outer timestamps. |
| `response-mirrors.jsonl` | `R(a)` = `(60,40,0,10)`, `R(b)` = `(60,60,0,20)` | 250 | `0.153.4`; A's tier unknown, B requested priority/fast at its turn boundary; complete cache detail, exact requests. |
| `response-pending-mirror.jsonl` | Same as response-mirrors | 250 | Exact byte prefix through B, before its mirror. |
| `response-partial-tail.jsonl` | `R(a)` = `(60,40,0,10)` | 110 | Deliberately EOF-truncated final JSON without newline; `IncompleteFinalLine`. |
| `response-subagent.jsonl` | `R(c)` = `(60,40,0,10)` | 110 | Owner `thread-child`, parent/root session `thread-main`, own turn `turn-child`, shared root turn `turn-main`; unknown tier. |

### Matched producer sequences, constructed with invented values

| Fixture | Expected events | Total | Evidence |
| --- | --- | ---: | --- |
| `upgrade-response-first.jsonl` | `L(a)` = `(120,100,0,30)`, `R(a)` = `(60,40,0,10)` | 360 | Legacy `0.128.0` prefix, then new `0.153.4` turn; raw legacy offset `(220 input,100 read,0 write,30 output)` remains in the mirror but not the response accumulator. Not seen as a local upgrade. |
| `legacy-partial-fork.jsonl` | `L(a)` = `(60,40,0,10)`, `L(f)` = `(60,40,0,10)` | 220 | Interrupted inherited turn closes before new child work. Supported by the matched fork code; not observed as this exact local sequence. |

### Adversarial policy cases

These are accounting/correction contracts, not claims the producer emits corrections.

| Fixture | Expected result |
| --- | --- |
| `legacy-corrected.jsonl` | `L(a)` = `(150,110,0,40)`, total 300; corrected first request and an inserted repeat keep one key. |
| `legacy-zero-first-request.jsonl` | `L(a)` = `(60,60,0,20)`, total 140; first completion corrected to zero cannot shift any key. |
| `legacy-zero-correction.jsonl` | `L(a)` = `(0,0,0,0)`, total 0; explicit zero replaces the same turn observation. |
| `legacy-resume-corrected.jsonl` | `L(a)` = `(150,110,0,40)`, `L(b)` = `(60,40,0,10)`, total 410; a correction to A changes neither B's key nor its delta. |
| `response-repeat-correction.jsonl` | Only `R(a)` = `(100,50,0,20)`, total 170; retain original unknown tier after later default settings. |
| `response-equal-counts.jsonl` | `R(a)` and `R(d)` each `(60,40,0,10)`, total 220; distinct responses sharing a root turn/counts both count. |
| `response-cache-write.jsonl` | `R(a)` = `(50,40,10,10)`, total 110; cache write is disjoint, reasoning 2 already included in output 10. |
| `response-cleared-tier.jsonl` | Same keys/vectors as response-mirrors, total 250; a full snapshot omitting tier clears priority before B's turn, leaving both tiers unknown. |

Every `reject-*.jsonl` must reject the **whole file**, preserving any previous
successful import. No imported events/totals are promised for rejected files.

| Fixture | Reason |
| --- | --- |
| `reject-response-identity.jsonl` | A response ID is reused with a conflicting original turn. |
| `reject-legacy-first-mirror.jsonl` | Legacy notification precedes an equal response; cannot safely replace an imported legacy key or prove a mirror. |
| `reject-mid-turn-upgrade.jsonl` | Response format starts inside an already-accounted legacy turn. |
| `reject-mirror-offset.jsonl` | Mirror progression disagrees with the fixed pre-upgrade baseline and pending response. |
| `reject-ambiguous-mirrors.jsonl` | A second response arrives before the first mirror; the supported one-pending-response ordering no longer proves pairing. |
| `reject-initial-baseline.jsonl` | First total exceeds last request; no complete fresh epoch. |
| `reject-checkpoint-baseline.jsonl` | A compaction checkpoint precedes the first counter; even total equal to last usage does not establish a complete original epoch. |
| `reject-counter-decrease.jsonl` | Cumulative usage decreases after prior usage; no supported reset epoch. |
| `reject-context-reset.jsonl` | Producer's full-context fallback overwrites cumulative accounting with a context-size estimate. |
| `reject-overlapping-delta.jsonl` | Valid cumulative endpoints imply delta input 10 and cache read 20; normalization must fail. |
| `reject-legacy-correction.jsonl` | Post-completion change has no active turn and delta disagrees with last usage. |
| `reject-reused-turn.jsonl` | New work reuses a closed original turn ID. |
| `reject-overlapping-turns.jsonl` | Another legacy turn starts before the prior turn closes. |

## Prefix and retained-import expectations

| Fixture prefix (complete lines) | Expected facts |
| --- | --- |
| `legacy-fresh`: 4 / 5 / 6 / 7 / 8 | None / `L(a)` total 110 / unchanged / same key total 250 / unchanged. |
| `response-mirrors`: 3 / 4 / 5 / 9 / 10 / 11 | None / `R(a)` total 110 / unchanged / unchanged / `R(a)+R(b)` total 250 / unchanged. |
| `upgrade-response-first`: 8 / 11 / 12 / 13 | `L(a)` total 250 / unchanged / `L(a)+R(a)` total 360 / unchanged. |
| `reject-legacy-first-mirror`: 4 / 5 | Prefix has `legacy-turn-v1:turn-main` = `(60,40,0,10)`, total 110; the response on line 5 rejects the extension and retains that import. |

C06 must import those prefixes through the retained store and reimport each
extension, including partial-tail completion. It must check same-key corrections
(including explicit zero), both parent/child import orders, and whole-file rejection
retaining prior observations. Source evidence releases the contract; these fixture
checks do not replace those later parser/storage integration tests.

The legacy-first prefix is indistinguishable from legacy usage until its response
arrives. Do not reject it merely because the header says `0.153.4`, and do not
replace its imported turn key with a response key on extension. Whole-file
rejection preserves the valid earlier prefix without adding a second charge.

Reuse the same bytes at different paths for copy/rename/archive tests. With both
legacy-parent and legacy-fork, canonical total is 360 over two keys, not 610.
With legacy-parent and legacy-partial-fork it is also 360: the ancestor's fuller
`L(a)` wins, and `L(f)` remains separate. Response-mirrors plus response-subagent
have three keys and total 360 despite equal A/child counts.

Final complete JSON without a newline is valid. Only response-partial-tail has
invalid JSON. C03 also needs malformed complete JSON, truncated UTF-8, and incomplete
header tests. Discovery, parser implementation, pricing/schema, and registration
are outside C01 and remain unchanged.
