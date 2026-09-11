use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionDiscovery, CodexSessionParser};
use token_tracker::application::{
    ParseCompletion, ParseContext, ParsedSession, SessionParser, UsageReadStore, UsageStore,
    synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp, TokenCounts, UsageKind};
use token_tracker::storage::SqliteUsageStore;

const FRESH: &str = include_str!("fixtures/codex/legacy-fresh.jsonl");
const RESUMED: &str = include_str!("fixtures/codex/legacy-resume-compaction.jsonl");
const CORRECTED: &str = include_str!("fixtures/codex/legacy-resume-corrected.jsonl");
const ZERO: &str = include_str!("fixtures/codex/legacy-zero-correction.jsonl");

fn parse(source: &str) -> Result<ParsedSession, CodexParseError> {
    CodexSessionParser::new().parse(
        &mut Cursor::new(source),
        ParseContext {
            source_path: Path::new("/not-read/rollout-legacy.jsonl"),
        },
    )
}

fn prefix(source: &str, lines: usize) -> String {
    source.lines().take(lines).collect::<Vec<_>>().join("\n")
}

fn tokens(counts: TokenCounts) -> Value {
    json!([
        counts.input,
        counts.cache_read,
        counts.cache_write,
        counts.output
    ])
}

fn fixture(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(name),
    )
    .unwrap()
}

#[test]
fn legacy_fixtures_match_turn_aggregate_oracles() {
    let expectations: Value =
        serde_json::from_str(include_str!("fixtures/codex/expectations.json")).unwrap();
    for (name, expected) in expectations["fixtures"].as_object().unwrap() {
        if !name.starts_with("legacy-") {
            continue;
        }
        let parsed = parse(&fixture(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        let events = expected["events"].as_array().unwrap();
        assert_eq!(parsed.events.len(), events.len(), "{name}");
        for expected in events {
            let actual = parsed
                .events
                .iter()
                .find(|event| event.identity.adapter_key == expected["key"])
                .unwrap_or_else(|| panic!("{name}: missing {}", expected["key"]));
            assert_eq!(actual.identity.agent, AgentId::from("codex"), "{name}");
            assert_eq!(actual.identity.adapter_key, expected["key"], "{name}");
            assert_eq!(tokens(actual.tokens), expected["tokens"], "{name}");
            assert_eq!(actual.kind, UsageKind::Other, "{name}");
            assert_eq!(actual.recorded_cost, None, "{name}");
            let context = actual.pricing_context.as_ref().unwrap();
            assert!(context.request_usage.is_some(), "{name}");
            assert!(context.request_usage_matches(actual.tokens), "{name}");
            let owner_known = name != "legacy-partial-fork.jsonl"
                && (name != "legacy-fork.jsonl"
                    || actual.identity.adapter_key.ends_with("turn-fork"));
            assert_eq!(actual.attribution.is_some(), owner_known, "{name}");
        }
        let total: u128 = parsed.events.iter().map(|event| event.tokens.total()).sum();
        assert_eq!(
            total,
            u128::from(expected["total_tokens"].as_u64().unwrap()),
            "{name}"
        );
        assert_eq!(parsed.completion, ParseCompletion::Complete, "{name}");
    }
}

#[test]
fn legacy_aggregates_use_task_start_timestamps_for_open_and_closed_turns() {
    let source = fixture("legacy-partial-fork.jsonl");
    for (turn_id, started_at, lines) in [
        ("turn-legacy-a", "2026-01-02T00:01:02Z", 6),
        ("turn-fork", "2026-01-02T00:01:07Z", 10),
    ] {
        let expected = Timestamp::from_unix_milliseconds(
            chrono::DateTime::parse_from_rfc3339(started_at)
                .unwrap()
                .timestamp_millis(),
        );
        for input in [prefix(&source, lines), source.clone()] {
            let parsed = parse(&input).unwrap();
            let event = parsed
                .events
                .iter()
                .find(|event| event.identity.adapter_key == format!("legacy-turn-v1:{turn_id}"))
                .unwrap();
            assert_eq!(event.timestamp, expected, "{turn_id}");
        }
    }
}

#[test]
fn open_turn_prefixes_keep_identity_and_timestamp_through_noops_and_partial_tails() {
    let expectations: Value =
        serde_json::from_str(include_str!("fixtures/codex/expectations.json")).unwrap();
    for name in ["legacy-fresh.jsonl", "reject-legacy-first-mirror.jsonl"] {
        for (lines, expected) in expectations["prefixes"][name].as_object().unwrap() {
            if !expected.is_array() {
                continue;
            }
            let parsed = parse(&prefix(&fixture(name), lines.parse().unwrap())).unwrap();
            let actual: Vec<_> = parsed
                .events
                .iter()
                .map(|event| json!([event.identity.adapter_key, tokens(event.tokens)]))
                .collect();
            assert_eq!(json!(actual), *expected, "{name}:{lines}");
        }
    }

    let original = parse(&prefix(FRESH, 5)).unwrap();
    let mut next: Value = serde_json::from_str(FRESH.lines().nth(6).unwrap()).unwrap();
    next["timestamp"] = json!("2026-02-01T00:00:00Z");
    let missing = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","rate_limits":{}}}"#;
    let source = format!("{}\n{missing}\n{next}", prefix(FRESH, 6));
    let extended = parse(&source).unwrap();
    assert_eq!(extended.events[0].identity, original.events[0].identity);
    assert_eq!(extended.events[0].timestamp, original.events[0].timestamp);
    assert_eq!(extended.events[0].tokens.total(), 250);

    let next = next.to_string();
    let partial = parse(&format!(
        "{}\n{}",
        prefix(FRESH, 6),
        &next[..next.len() - 1]
    ))
    .unwrap();
    assert_eq!(partial.events, original.events);
    assert_eq!(partial.completion, ParseCompletion::IncompleteFinalLine);
}

#[test]
fn explicit_zero_usage_keeps_turn_keys_without_inventing_events_for_repeats() {
    let mut source = prefix(FRESH, 8);
    source.push('\n');
    source.push_str(
        &RESUMED
            .lines()
            .skip(8)
            .take(4)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let repeated = parse(&source).unwrap();
    assert_eq!(repeated.events.len(), 1);

    let mut zero: Value = serde_json::from_str(RESUMED.lines().nth(11).unwrap()).unwrap();
    for counter in zero["payload"]["info"]["last_token_usage"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        *counter = json!(0);
    }
    let with_zero = parse(&format!("{source}\n{zero}")).unwrap();
    assert_eq!(with_zero.events.len(), 2);
    assert_eq!(
        with_zero.events[1].identity.adapter_key,
        "legacy-turn-v1:turn-legacy-b"
    );
    assert_eq!(with_zero.events[1].tokens, TokenCounts::default());

    let corrected = parse(CORRECTED).unwrap();
    let resumed = parse(RESUMED).unwrap();
    assert_eq!(corrected.events[1], resumed.events[1]);
    assert_eq!(
        parse(ZERO).unwrap().events[0].identity,
        resumed.events[0].identity
    );
    assert_eq!(
        parse(ZERO).unwrap().events[0].tokens,
        TokenCounts::default()
    );
}

#[test]
fn rejects_unsupported_baselines_resets_and_turn_lifecycles_without_content() {
    for name in [
        "reject-initial-baseline.jsonl",
        "reject-checkpoint-baseline.jsonl",
        "reject-counter-decrease.jsonl",
        "reject-context-reset.jsonl",
        "reject-overlapping-delta.jsonl",
        "reject-legacy-correction.jsonl",
        "reject-reused-turn.jsonl",
        "reject-overlapping-turns.jsonl",
        "reject-mid-turn-upgrade.jsonl",
        "reject-legacy-first-mirror.jsonl",
    ] {
        assert!(parse(&fixture(name)).is_err(), "{name}");
    }

    for omitted in [1, 2] {
        let source = FRESH
            .lines()
            .enumerate()
            .filter(|(index, _)| *index != omitted)
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(parse(&source).is_err(), "missing lifecycle line {omitted}");
    }
    let detached_fork = fixture("legacy-fork.jsonl")
        .lines()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse(&detached_fork).is_err());

    for boundary in ["task_complete", "turn_aborted", "task_started"] {
        let conflict = json!({"timestamp": "2026-01-01T00:00:00Z", "type": "event_msg",
            "payload": {"type": boundary, "turn_id": "SECRET_CONFLICT", "content": "SECRET_CONTENT"}});
        let error = parse(&format!("{}\n{conflict}", prefix(FRESH, 5))).unwrap_err();
        assert!(matches!(
            error,
            CodexParseError::InvalidField { line: 6, .. }
        ));
        assert!(!format!("{error:?} {error}").contains("SECRET"));
    }
}

#[test]
fn validates_raw_deltas_and_last_usage_with_checked_arithmetic() {
    let first: Value = serde_json::from_str(FRESH.lines().nth(4).unwrap()).unwrap();
    for changes in [
        json!({"input_tokens": 110, "cached_input_tokens": 60, "output_tokens": 20, "total_tokens": 130}),
        json!({"reasoning_output_tokens": 1}),
        json!({"output_tokens": 20, "reasoning_output_tokens": 13, "total_tokens": 120}),
        json!({"input_tokens": u64::MAX, "total_tokens": 9}),
        json!({"cached_input_tokens": u64::MAX, "cache_write_input_tokens": 1}),
    ] {
        let mut changed = first.clone();
        for vector in ["total_token_usage", "last_token_usage"] {
            for (counter, value) in changes.as_object().unwrap() {
                changed["payload"]["info"][vector][counter] = value.clone();
            }
        }
        assert!(
            parse(&format!("{}\n{changed}", prefix(FRESH, 5))).is_err(),
            "{changes}"
        );
    }

    let mut mismatch: Value = serde_json::from_str(FRESH.lines().nth(6).unwrap()).unwrap();
    mismatch["payload"]["info"]["last_token_usage"]["reasoning_output_tokens"] = json!(3);
    assert!(matches!(
        parse(&format!("{}\n{mismatch}", prefix(FRESH, 5))),
        Err(CodexParseError::InvalidField {
            field: "event_msg.payload.info.last_token_usage",
            ..
        })
    ));

    let mut cache_first = first.clone();
    for vector in ["total_token_usage", "last_token_usage"] {
        cache_first["payload"]["info"][vector]["cache_write_input_tokens"] = json!(10);
    }
    let mut cache_next = cache_first.clone();
    cache_next["payload"]["info"]["total_token_usage"] = json!({
        "input_tokens": 200, "cached_input_tokens": 80, "cache_write_input_tokens": 30,
        "output_tokens": 20, "reasoning_output_tokens": 4, "total_tokens": 220,
    });
    cache_next["payload"]["info"]["last_token_usage"]["cache_write_input_tokens"] = json!(20);
    let parsed = parse(&format!(
        "{}\n{cache_first}\n{cache_next}",
        prefix(FRESH, 3)
    ))
    .unwrap();
    assert_eq!(tokens(parsed.events[0].tokens), json!([90, 80, 30, 20]));

    let mut largest = first;
    for vector in ["total_token_usage", "last_token_usage"] {
        largest["payload"]["info"][vector] = json!({
            "input_tokens": u64::MAX - 1, "cached_input_tokens": u64::MAX - 2,
            "cache_write_input_tokens": 1, "output_tokens": 1,
            "reasoning_output_tokens": 1, "total_tokens": u64::MAX,
        });
    }
    let parsed = parse(&format!("{}\n{largest}", prefix(FRESH, 3))).unwrap();
    assert_eq!(
        tokens(parsed.events[0].tokens),
        json!([0, u64::MAX - 2, 1, 1])
    );
    assert_eq!(parsed.events[0].tokens.total(), u128::from(u64::MAX));

    let mut disappeared = largest.clone();
    disappeared["payload"]["info"]["total_token_usage"]
        .as_object_mut()
        .unwrap()
        .remove("cache_write_input_tokens");
    assert!(parse(&format!("{}\n{largest}\n{disappeared}", prefix(FRESH, 3))).is_err());
}

struct TempTree(PathBuf);

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn synchronization_updates_turn_aggregates_and_retains_the_last_valid_import() {
    let tree = TempTree(
        std::env::temp_dir().join(format!("token-tracker-codex-legacy-{}", std::process::id())),
    );
    fs::create_dir(&tree.0).unwrap();
    let path = tree.0.join("rollout-legacy.jsonl");
    let discovery = CodexSessionDiscovery::new([&tree.0]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let cases = [
        (prefix(FRESH, 5), 1, 0, 110),
        (prefix(FRESH, 6), 0, 0, 110),
        (FRESH.to_owned(), 0, 1, 250),
        (ZERO.to_owned(), 0, 1, 0),
        (RESUMED.to_owned(), 1, 1, 360),
        (CORRECTED.to_owned(), 0, 1, 410),
    ];
    for (index, (source, inserted, updated, total)) in cases.into_iter().enumerate() {
        fs::write(&path, source).unwrap();
        let report = synchronize_sessions_at(
            &discovery,
            &CodexSessionParser::new(),
            &mut store,
            Timestamp::from_unix_milliseconds(index as i64),
        )
        .unwrap();
        assert!(report.warnings.is_empty(), "{report:?}");
        assert_eq!(report.counts.files_imported, 1);
        assert_eq!(report.counts.event_identities_inserted, inserted);
        assert_eq!(report.counts.observations_inserted, inserted);
        assert_eq!(report.counts.observations_updated, updated);
        let snapshot = store.usage_snapshot().unwrap();
        assert_eq!(
            snapshot
                .observations
                .iter()
                .map(|observation| observation.event.tokens.total())
                .sum::<u128>(),
            total
        );
    }

    let snapshot = store.usage_snapshot().unwrap();
    let last_imported = store.source_states(&AgentId::from("codex")).unwrap()[0]
        .last_imported_revision
        .clone();
    fs::write(&path, fixture("reject-counter-decrease.jsonl")).unwrap();
    let report = synchronize_sessions_at(
        &discovery,
        &CodexSessionParser::new(),
        &mut store,
        Timestamp::from_unix_milliseconds(100),
    )
    .unwrap();
    assert_eq!(report.counts.files_failed, 1);
    assert_eq!(report.counts.files_imported, 0);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
    assert_eq!(
        store.source_states(&AgentId::from("codex")).unwrap()[0].last_imported_revision,
        last_imported
    );
}
