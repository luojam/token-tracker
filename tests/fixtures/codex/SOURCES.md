# C01 producer evidence

Inspected 2026-09-06. Source was downloaded read-only from the official release
archives into `/tmp/token-tracker-c01-source`. No producer was run and no model
requests were made. The earlier lookup failed because the implementation lives
under `core/src/session/`, not `core/src/codex.rs`.

| Observed client | Release commit | Archive SHA-256 |
| --- | --- | --- |
| [0.128.0](https://github.com/openai/codex/releases/tag/rust-v0.128.0) | `e4310be51f617f5e60382038fa9cbf53a2429ca4` | `7fa05570bb9b3e65a690bb58933d26bcb804b777ff63ff5d831027354374e047` |
| [0.140.0-alpha.2](https://github.com/openai/codex/tree/rust-v0.140.0-alpha.2) | `8386a8d9c244ee4cc6330e1fb012d161910c04c6` | `1a5a8444d4870e24528cd5f59f3faec6ba769c9090468a4c3915a5250e78b4dc` |
| [0.153.4](https://github.com/openai/codex/releases/tag/rust-v0.153.4) | `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a` | `74d988c0e154aad2b8d0cca4e950fc97fe2a29ff5ebe3b0070cce6d949c9a307` |

Reproduce downloads with
`https://codeload.github.com/openai/codex/tar.gz/refs/tags/rust-v<VERSION>`.
The GitHub API tag objects resolve to the commits above. Sources remain outside
this repository; the fixtures contain invented data only.

## Stable legacy identity

The legacy response-completion handler explicitly discards `response_id`, then
updates usage. A completion ordinal inferred from changing totals is therefore
not a persisted completion identity. Do not release that proposed scheme.
[0.128.0 completion handler](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/core/src/session/turn.rs#L2085)

Use **`legacy-turn-v1:<original turn_id>`**, one aggregate per original turn.
This is an adapter design decision based on persisted turn identity, not a claim
that a legacy record represents the whole turn. Subtract successive raw totals,
validate each delta, normalize it, and sum into the turn's existing observation.
The public submission path creates a fresh UUID; turn start and completion carry
that same submission ID. Reject reuse of a closed turn ID or ambiguous overlapping
turns. Source-specific internal IDs without this identity guarantee are unsupported.
[Submission IDs](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/session/mod.rs#L675),
[turn start](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/tasks/regular.rs#L48),
[turn completion](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/tasks/mod.rs#L741).

The fork path persists inherited rollout items and seeds their latest cumulative
usage. An interrupted fork appends a boundary for the inherited active turn;
new child work begins under another turn ID. This matches the local copied fork.
The JSONL writer stamps each copied item with its current time, so outer timestamps
cannot identify inherited work. Item IDs are also unsuitable: newer fork code can
assign missing response-item IDs when copying older history.
[Fork restoration](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/session/mod.rs#L1270),
[interrupted fork](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/thread_manager.rs#L1577),
[timestamp writer](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/rollout/src/recorder.rs#L1673),
[assigning missing item IDs](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L1466).

Consequences of the chosen key, independent of token values:

- Copies, renamed files, archives, and inherited prefixes retain the original turn
  key. Neither owning child ID nor newly written outer timestamps enter it.
- Appending more completions to an open turn updates that key. Repeats, inserted
  no-op notifications, or a corrected number of increments cannot shift any key.
- Valid counter corrections retaining the original turn remain under the same key.
  Explicit all-zero request/delta vectors can produce a zero observation under
  that key, so a correction to zero replaces the old value in the retained store.
  Ordinary repeats of nonzero last usage do not create zero events for new turns.
- Removing an entire turn or all its usage records is retained history, as in the
  existing ledger, not evidence to erase its previously imported usage. Conflicting
  lifecycle IDs or invalid corrected deltas reject the file.
- A partial inherited turn may have a smaller observed aggregate than its parent's
  complete turn. Existing ancestor selection chooses the parent's observation.
  Without that parent, only the observed prefix is known. New child work has its
  own key and remains additive. No canonical-selection change is required.

Legacy observations retain aggregate granularity and their original turn identity,
with a complete request-usage breakdown for pricing. Rechecked the matched
0.128.0 producer on 2026-09-09: `ResponseEvent::Completed` passes that response's
usage to `update_token_usage_info`, which appends it and emits the snapshot;
`append_last_usage` adds the same vector to cumulative usage and replaces last
usage. Thus a validated cumulative delta equal to last usage preserves the
reported request's token counts without needing a persisted response ID.
Unchanged snapshots and recomputed context estimates add no request usage.
The breakdown must sum to the aggregate, and corrections replace it under the
same turn identity. Each request selects its own context price band.
A zero-valued aggregate is not a claim that a request took place. Response records
retain exact per-response granularity and `response-v1:<response_id>` keys.

## Fresh, resumed, and reset counters

`ContextManager::new` starts with no token info. `TokenUsageInfo::new_or_append`
creates a zero cumulative vector and adds the first usage; later completions add
to it. Rate-limit updates publish the existing snapshot without adding usage.
[Initial state](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/core/src/context_manager/history.rs#L61),
[append arithmetic](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/protocol/src/protocol.rs#L2080),
[usage and rate-limit paths](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/core/src/session/mod.rs#L2805).

Supported zero baseline: a complete owning header and original start/context
sequence, followed by first accounting total equal to last usage (or an explicit
all-zero snapshot), with no earlier restore/checkpoint/reset marker. Copied forks
must contain that same complete inherited prefix; do not start at zero at the
child header. A detached nonzero baseline or a checkpoint-only history is rejected.
A file indistinguishable from a valid full history after arbitrary manual edits
cannot be authenticated by this parser; no stronger integrity guarantee is claimed.

Resume/fork loads the latest `TokenCount.info` from the retained rollout and seeds
state from it. Therefore continue the same cumulative baseline across repeated
headers and new turns; never reset it on resume or fork. The local fork preserves
both inherited counters and their turn IDs.
[Resume/fork seed](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/core/src/session/mod.rs#L1192),
[0.140.0-alpha.2 seed and lookup](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/session/mod.rs#L1256).

`recompute_token_usage` retains cumulative usage while replacing only the last
vector with a context-size estimate: categories zero, total possibly nonzero.
Ignore that last vector when cumulative totals are unchanged. In contrast,
`fill_to_context_window` replaces the cumulative vector with a context-size value;
this is not incurred usage and must reject the file. Any unexplained cumulative
decrease, inconsistent delta, or accounting checkpoint that omits history also
rejects. No reset epoch or replacement-history traversal is supported in v1.
[Recompute](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/core/src/session/mod.rs#L2817),
[context-window overwrite](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/protocol/src/protocol.rs#L2111).

## Upgrade and mirror ordering

The 0.153.4 completion handler persists its response record before updating the
legacy token info; the cumulative notification is sent later, after pending tools
finish. Thus a response can exist without its mirror in a successful file prefix.
[Completion path](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/turn.rs#L2591),
[delayed notification](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/turn.rs#L2805),
[record persistence](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L4342).

Resume separately restores legacy info and the latest response record. With only
legacy history there is no prior response record. The response accumulator starts
at zero, while legacy counters retain their pre-upgrade total. The earlier fixture
assumption that both cumulative totals always match was wrong.
[Separate restoration](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L1448),
[response accumulator](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/state/session.rs#L160).

For the supported upgrade, require complete legacy history ending at a closed
turn and a new turn whose response record precedes any increasing mirror. Let
`L` be the pre-upgrade raw legacy total, `R` accumulated response usage, and `C`
the legacy notification total. Validate each counter with checked arithmetic:

```text
response.thread_token_usage = R
mirror.total_token_usage = L + R
mirror.last_token_usage = the pending response.usage
mirror.total_token_usage - previous confirmed mirror total = pending response.usage
```

For response-native histories `L = 0`. Validate turn accumulators within their
original turns separately; they restart at a new turn. These are validation
vectors, never extra additive streams. Missing old cache-write fields use the
producer's default zero for baseline arithmetic while retaining unknown legacy
cache detail. A verified mirror only advances validation state; it does not
create a legacy event, including zero-usage response mirrors.

Support the evidenced one-pending-response ordering. Match original thread/turn,
known baseline, and counter progression together; equal counts alone are
insufficient. Repeated unchanged totals are no-ops. A response-only tail counts
immediately; its later mirror keeps all keys and totals unchanged. Reject an
unmatched increasing notification, mismatched offset, multiple pending responses
with ambiguous mirror pairing, or legacy-first conversion. Switching formats
inside an already-accounted legacy turn is unsupported; a later rejection keeps
the previously imported prefix. No aliases or ledger migration are needed.

Response corrections update the selected observation, not the original historical
mirror arithmetic. The adversarial correction fixture follows its original
verified mirror and does not enqueue a second mirror. Other correction/mirror
orders are not established by that case; reject unproven cumulative progression
instead of charging the corrected response again.

The upgrade fixture combines observed legacy/response shapes according to this
matched producer control flow. It is **producer-backed**, not a claim that a local
upgrade session was observed. Arbitrary rewriting of an already imported legacy
turn into response records remains unsupported: a whole-file parser cannot infer
old identities once the source has removed them. Detecting such external edits
across files would require additional provenance outside this slice's scope.

## Review control and forwarded events

A read-only follow-up inspected the 17 rejected review-parent rollouts: one used
legacy `entered_review_mode` / `exited_review_mode` events (`0.144.1`), and 16 used
`item_completed` with `EnteredReviewMode` / `ExitedReviewMode` (`0.153.4`). Each had
a separate child rollout marked `source.subagent = "review"`. Parent review spans
contained no accounting or turn-context records. Only metadata, event shapes,
ordering, and identifier equality were inspected; the regression tests use
invented data, not copied local records.

The matched `0.153.4` producer explains the apparent boundary mismatch:

- The review path explicitly notes that it emits no parent `TurnStarted`.
  [Review operation](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/review.rs#L197-L210).
- The delegate filters child `TokenCount` events. Review consumes the child's
  completion/abort and forwards other events, including the child's start and
  non-assistant presentation items. The parent later emits its own terminal event.
  [Delegate filter](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/codex_delegate.rs#L300-L314),
  [review forwarding](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/tasks/review.rs#L145-L185).
- Forwarding wraps messages in the parent's runtime event ID, but rollout
  persistence stores only `event.msg`, retaining embedded child identifiers.
  Accounting records are written directly to the originating session's rollout.
  [Event wrapper](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L2091-L2094),
  [event persistence](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L2396-L2399),
  [usage persistence](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/mod.rs#L4342-L4373).
- History mode determines whether review markers use legacy events or completed
  items; this is not a client-version switch. Abort emits review exit before the
  parent's abort. A null review result alone does not prove abort.
  [Persistence policy](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/rollout/src/policy.rs#L91-L132),
  [abort test](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/tests/suite/review.rs#L357-L376).

Therefore review control must not use the child's start as a parent accounting
boundary. Keep explicit parent review identity until its terminal event, even
after review exit. Do not relax ordinary boundary matching or silently skip
accounting inside reviews. The supported shape requires explicit identities:
optional legacy marker IDs without evidence cannot authorize arbitrary unmatched
completions. Nested reviews or an already active ordinary parent turn are not
established by these samples and remain unsupported.

Settings are not excluded by the forwarding filters. Explicit thread ownership
can distinguish parent defaults from child settings; unscoped review settings are
ambiguous and must clear local default evidence rather than silently retaining or
overwriting it. This is a conservative attribution rule, not extra usage.
[Settings publisher](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/thread_settings.rs#L88-L114).

## Cache and settings evidence

The subagent fixture uses the serialized `SubAgentSource::ThreadSpawn` object,
including its parent thread and depth. An arbitrary string in `source.subagent`
is not that enum's wire shape. The explicit metadata parent establishes lineage;
the shared `session_id` and `root_turn_id` do not establish it on their own.
[Subagent source type](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/protocol/src/protocol.rs#L2822),
[session metadata](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/protocol/src/protocol.rs#L3037).

0.128.0's `TokenUsage` has no cache-write member and its API conversion cannot
preserve such a subdivision. 0.153.4 includes the field with a deserialization
default. A compatibility default is not proof the source reported zero. Preserve
known tokens and mark omitted cache-write detail incomplete. Exact pricing needs
this subdivision only when writes cost more than ordinary input. The Codex report
explicitly estimates missing writes as ordinary input and counts that assumption.
[Old usage type](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/protocol/src/protocol.rs#L2057),
[old API conversion](https://github.com/openai/codex/blob/e4310be51f617f5e60382038fa9cbf53a2429ca4/codex-rs/codex-api/src/sse/responses.rs#L142),
[new usage type](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/protocol/src/protocol.rs#L2214).

The old unscoped settings event is a full snapshot of that session's configuration.
Use it locally only with an unambiguous original owner (including inherited owner
headers); otherwise tier remains unknown. New events explicitly include the
thread ID. Their publisher emits the committed full snapshot, so a missing/null
tier clears previous evidence. Bind requested defaults at a subsequent matching
turn boundary; an in-flight change cannot prove the tier of a running request.
Do not carry parent defaults into a child's own turns or backfill earlier usage.
[Old settings snapshot](https://github.com/openai/codex/blob/8386a8d9c244ee4cc6330e1fb012d161910c04c6/codex-rs/core/src/session/handlers.rs#L158),
[new settings publisher](https://github.com/openai/codex/blob/3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs/core/src/session/thread_settings.rs#L85).
