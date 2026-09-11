use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufReader, Cursor, Read};
use std::path::Path;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use token_tracker::adapters::claude::{
    ClaudeParseError, ClaudeSessionDiscovery, ClaudeSessionParser,
};
use token_tracker::application::{
    ImportAdapter, ParseCompletion, ParseContext, ParsedSession, SessionAdapter, SessionParser,
    UsageReadStore,
};
use token_tracker::domain::{ParentSession, ServiceTier, Timestamp, TokenCounts, UsageKind};
use token_tracker::storage::SqliteUsageStore;

const SOURCE_PATH: &str =
    "/invented/claude/projects/fixture/11111111-1111-4111-8111-111111111111.jsonl";
const SNAPSHOTS: &str = include_str!("fixtures/claude/snapshots.jsonl");
const CHILD: &str = include_str!("fixtures/claude/child-a1b2c3d.jsonl");

fn parse(source: &[u8], path: &str) -> Result<ParsedSession, ClaudeParseError> {
    ClaudeSessionParser::new().parse(
        &mut BufReader::with_capacity(1, Cursor::new(source)),
        ParseContext {
            source_path: Path::new(path),
        },
    )
}

fn oracle() -> Value {
    serde_json::from_str(include_str!("fixtures/claude/expectations.json")).unwrap()
}

fn time(timestamp: Timestamp) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp.as_unix_milliseconds())
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn tokens(tokens: TokenCounts) -> Value {
    json!([
        tokens.input,
        tokens.cache_read,
        tokens.cache_write,
        tokens.output
    ])
}

fn expected_billing(event: &Value) -> Value {
    let facts = &event["pricing_facts"];
    let tier = |value: &Value, allow_fast: bool| match value["value"].as_str() {
        None => ServiceTier::Unknown,
        Some("standard") => ServiceTier::Standard,
        Some("fast") if allow_fast => ServiceTier::Fast,
        Some(value) => ServiceTier::Unsupported(value.into()),
    };
    let components = facts["components"].as_array().unwrap();
    let mut durations = BTreeMap::<u32, u64>::new();
    let mut complete = true;
    for component in components {
        if component["cache_creation"].is_null() {
            complete &= component["tokens"][2] == 0;
        } else {
            for (seconds, field) in [(300, "five_minute_tokens"), (3600, "one_hour_tokens")] {
                *durations.entry(seconds).or_default() +=
                    component["cache_creation"][field].as_u64().unwrap();
            }
        }
    }
    json!({
        "speed": tier(&facts["speed"], true),
        "service_tier": tier(&facts["service_tier"], false),
        "requests": components.iter().map(|component| component["tokens"].clone()).collect::<Vec<_>>(),
        "cache_writes": complete.then(|| durations.into_iter().map(|(seconds, tokens)| json!({"duration_seconds": seconds, "tokens": tokens})).collect::<Vec<_>>()),
    })
}

fn check_usage(parsed: &ParsedSession, expected: &Value, name: &str) {
    let actual: BTreeMap<_, _> = parsed
        .events
        .iter()
        .map(|event| {
            assert_eq!(event.identity.agent.as_str(), "claude", "{name}");
            assert_eq!(event.kind, UsageKind::Assistant, "{name}");
            assert_eq!(event.recorded_cost, None, "{name}");
            let pricing = event.pricing_context.as_ref().unwrap();
            assert!(pricing.usage_matches(event.tokens), "{name}");
            (
                event.identity.adapter_key.clone(),
                json!({
                    "key": event.identity.adapter_key,
                    "timestamp": time(event.timestamp),
                    "kind": "assistant",
                    "attribution": event.attribution.as_ref().map(|attribution| json!({
                        "provider": attribution.provider,
                        "model": attribution.model,
                    })),
                    "tokens": tokens(event.tokens),
                    "recorded_cost": null,
                    "pricing_facts": {
                        "speed": pricing.speed,
                        "service_tier": pricing.tier,
                        "requests": pricing.request_usage.as_ref().unwrap().iter().copied().map(tokens).collect::<Vec<_>>(),
                        "cache_writes": pricing.cache_writes,
                    },
                }),
            )
        })
        .collect();
    let events: BTreeMap<_, _> = expected["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| {
            let mut normalized = event.clone();
            normalized["pricing_facts"] = expected_billing(event);
            (event["key"].as_str().unwrap().to_owned(), normalized)
        })
        .collect();
    assert_eq!(actual, events, "{name}");
    assert_eq!(parsed.events.len(), actual.len(), "{name}");
    let notices: BTreeMap<_, _> = parsed
        .notices
        .iter()
        .map(|notice| {
            assert!(notice.line.is_some(), "{name}");
            (
                serde_json::to_value(&notice.code)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                notice.count.get(),
            )
        })
        .collect();
    let expected_notices: BTreeMap<_, _> = expected["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| {
            (
                notice["code"].as_str().unwrap().to_owned(),
                notice["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(notices, expected_notices, "{name}");
    assert_eq!(parsed.notices.len(), notices.len(), "{name}");
    assert_eq!(
        parsed
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u128>(),
        u128::from(expected["total_tokens"].as_u64().unwrap()),
        "{name}"
    );
}

#[test]
fn all_fixtures_match_metadata_accounting_and_error_oracles() {
    for (name, expected) in oracle()["fixtures"].as_object().unwrap() {
        let source = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/claude")
                .join(name),
        )
        .unwrap();
        let result = parse(&source, expected["source_path"].as_str().unwrap());
        if !expected["error"].is_null() {
            let (line, reason) = match result.unwrap_err() {
                ClaudeParseError::MalformedLine { line } => (line, "invalid_json"),
                ClaudeParseError::InvalidField { line, field } => (
                    line,
                    match field {
                        "conflicting model" => "conflicting_model",
                        "conflicting request id" => "conflicting_request_id",
                        "cache duration sum mismatch" => "cache_duration_sum_mismatch",
                        "required counter" => "invalid_required_counter",
                        "non-compaction total mismatch" => "non_compaction_total_mismatch",
                        _ => panic!("{name}: unexpected field {field}"),
                    },
                ),
                error => panic!("{name}: unexpected error {error}"),
            };
            assert_eq!(json!(line), expected["error"]["line"], "{name}");
            assert_eq!(json!(reason), expected["error"]["reason"], "{name}");
            continue;
        }
        let parsed = result.unwrap_or_else(|error| panic!("{name}: {error}"));
        let metadata = &parsed.metadata;
        assert_eq!(
            json!({
                "agent": metadata.agent.as_str(),
                "session_id": metadata.session_id,
                "format_version": null,
                "working_directory": metadata.working_directory,
                "started_at": time(metadata.started_at),
                "name": metadata.name,
                "parent_session": metadata.parent_session.as_ref().map(|parent| match parent {
                    ParentSession::SessionId(id) => json!({"session_id": id}),
                    ParentSession::SourcePath(_) => panic!("unexpected parent path"),
                }),
            }),
            expected["session"],
            "{name}"
        );
        assert_eq!(
            match parsed.completion {
                ParseCompletion::Complete => "complete",
                ParseCompletion::IncompleteFinalLine => "incomplete_final_line",
            },
            expected["completion"],
            "{name}"
        );
        check_usage(&parsed, expected, name);
    }
}

#[test]
fn snapshot_prefixes_keep_finals_and_first_final_timestamps() {
    for expected in oracle()["prefixes"].as_array().unwrap() {
        let count = expected["complete_lines"].as_u64().unwrap() as usize;
        let source = SNAPSHOTS.lines().take(count).collect::<Vec<_>>().join("\n");
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert_eq!(parsed.completion, ParseCompletion::Complete);
        check_usage(&parsed, expected, &format!("prefix {count}"));
    }
}

fn final_record() -> Value {
    serde_json::from_str(SNAPSHOTS.lines().nth(3).unwrap()).unwrap()
}

fn parse_record(record: &Value) -> Result<ParsedSession, ClaudeParseError> {
    parse(record.to_string().as_bytes(), SOURCE_PATH)
}

#[test]
fn iteration_models_use_the_model_from_earlier_snapshots() {
    let mut original = final_record();
    let mut iteration = original["message"]["usage"].clone();
    iteration["type"] = json!("message");
    iteration["model"] = original["message"]["model"].clone();
    original["message"]["usage"]["iterations"] = json!([iteration]);
    let expected = parse_record(&original).unwrap();
    assert_eq!(expected.events.len(), 1);

    let mut final_snapshot = original.clone();
    final_snapshot["message"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    for stop_reason in [original["message"]["stop_reason"].clone(), Value::Null] {
        let mut earlier = original.clone();
        earlier["message"]["stop_reason"] = stop_reason;
        let source = format!("{earlier}\n{final_snapshot}");
        assert_eq!(parse(source.as_bytes(), SOURCE_PATH).unwrap(), expected);
    }

    final_snapshot["message"]["usage"]["iterations"][0]["model"] = json!("different-model");
    let source = format!("{original}\n{final_snapshot}");
    let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
    assert!(parsed.events.is_empty());
    assert_eq!(parsed.notices.len(), 1);
    assert_eq!(parsed.notices[0].code, "unsupported_response_accounting");
}

#[test]
fn final_usage_is_revalidated_when_a_placeholder_supplies_the_model() {
    let mut original = final_record();
    let mut iteration = original["message"]["usage"].clone();
    iteration["type"] = json!("message");
    iteration["model"] = original["message"]["model"].clone();
    original["message"]["usage"]["iterations"] = json!([iteration]);
    let expected = parse_record(&original).unwrap();
    assert_eq!(expected.events.len(), 1);

    let mut final_snapshot = original.clone();
    final_snapshot["message"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    let mut placeholder = original.clone();
    placeholder["timestamp"] = json!("2026-01-01T00:00:06Z");
    placeholder["message"]["stop_reason"] = Value::Null;
    placeholder["message"]["usage"] = json!({
        "input_tokens": 999,
        "output_tokens": 0,
        "cache_read_input_tokens": 999,
        "cache_creation_input_tokens": 0,
    });
    for source in [
        format!("{placeholder}\n{final_snapshot}"),
        format!("{final_snapshot}\n{placeholder}"),
    ] {
        assert_eq!(parse(source.as_bytes(), SOURCE_PATH).unwrap(), expected);
    }

    placeholder["message"]["model"] = json!("different-model");
    let source = format!("{final_snapshot}\n{placeholder}");
    let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
    assert!(parsed.events.is_empty());
    assert_eq!(parsed.notices.len(), 1);
    assert_eq!(parsed.notices[0].code, "unsupported_response_accounting");
    assert_eq!(parsed.notices[0].line.unwrap().get(), 1);
}

#[test]
fn validates_source_identity_and_requires_record_metadata() {
    for (field, value) in [
        ("sessionId", json!("22222222-2222-4222-8222-222222222222")),
        ("sessionId", json!(42)),
        ("agentId", json!("child")),
        ("timestamp", json!("PRIVATE_INVALID_TIMESTAMP")),
    ] {
        let mut record = final_record();
        record[field] = value;
        let error = parse_record(&record).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    let mut record = final_record();
    record.as_object_mut().unwrap().remove("sessionId");
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::MissingMetadata { field: "sessionId" })
    ));
    record = json!({"sessionId": "11111111-1111-4111-8111-111111111111", "session_id": "ignored"});
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::MissingMetadata { field: "timestamp" })
    ));
    assert!(parse(b"", SOURCE_PATH).is_err());

    let child_path = "/invented/project/11111111-1111-4111-8111-111111111111/subagents/nested/agent-a1b2c3d.jsonl";
    assert!(parse(CHILD.as_bytes(), child_path).is_ok());
    let mut child: Value = serde_json::from_str(CHILD).unwrap();
    child.as_object_mut().unwrap().remove("agentId");
    assert!(matches!(
        parse(child.to_string().as_bytes(), child_path),
        Err(ClaudeParseError::MissingMetadata { field: "agentId" })
    ));
    child["agentId"] = json!("different");
    assert!(parse(child.to_string().as_bytes(), child_path).is_err());
    for path in [
        "relative.jsonl",
        "/project/not-a-session.jsonl",
        "/project/agent-a.jsonl",
        "/project/11111111-1111-4111-8111-111111111111/subagents/agent-a:b.jsonl",
    ] {
        assert!(matches!(
            parse(CHILD.as_bytes(), path),
            Err(ClaudeParseError::InvalidSourcePath)
        ));
    }

    let mut later = final_record();
    later["cwd"] = json!("/later");
    later["version"] = json!({"unknown": [1, 2]});
    let earlier =
        json!({"type": {"unknown": true}, "timestamp": "2025-01-01T00:00:00Z", "cwd": "/earlier"});
    let parsed = parse(format!("{later}\n{earlier}").as_bytes(), SOURCE_PATH).unwrap();
    assert_eq!(time(parsed.metadata.started_at), "2025-01-01T00:00:00Z");
    assert_eq!(
        parsed.metadata.working_directory.unwrap(),
        Path::new("/earlier")
    );
}

#[test]
fn rejects_bad_counters_and_checked_sum_conflicts_including_placeholders() {
    for final_snapshot in [false, true] {
        for field in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ] {
            for invalid in [
                Value::Null,
                json!(-1),
                json!(1.5),
                json!("PRIVATE_COUNTER"),
                json!(true),
                json!([]),
                json!(18446744073709551616.0_f64),
            ] {
                let mut record = final_record();
                record["message"]["usage"][field] = invalid;
                if !final_snapshot {
                    record["message"]["stop_reason"] = Value::Null;
                }
                let error = parse_record(&record).unwrap_err();
                assert!(matches!(
                    error,
                    ClaudeParseError::InvalidField {
                        field: "required counter",
                        ..
                    }
                ));
                assert!(!format!("{error:?} {error}").contains("PRIVATE"));
            }
            let mut record = final_record();
            record["message"]["usage"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(parse_record(&record).is_err());
        }
    }
    let mut record = final_record();
    record["message"]["usage"]["cache_creation"] = json!({
        "ephemeral_5m_input_tokens": u64::MAX, "ephemeral_1h_input_tokens": 1,
    });
    assert!(parse_record(&record).is_err());
    record["message"]["usage"] = json!({
        "input_tokens": u64::MAX, "output_tokens": 0,
        "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
    });
    assert_eq!(
        parse_record(&record).unwrap().events[0].tokens.input,
        u64::MAX
    );
    let mut message_iteration = record["message"]["usage"].clone();
    message_iteration["type"] = json!("message");
    let compaction = json!({"type": "compaction", "input_tokens": 1, "output_tokens": 0,
        "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    record["message"]["usage"]["iterations"] = json!([message_iteration, compaction]);
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::InvalidField {
            field: "counter overflow",
            ..
        })
    ));

    for field in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let mut record = final_record();
        let mut iteration = record["message"]["usage"].clone();
        iteration["type"] = json!("message");
        iteration.as_object_mut().unwrap().remove("cache_creation");
        iteration[field] = json!(iteration[field].as_u64().unwrap() + 1);
        record["message"]["usage"]["iterations"] = json!([iteration]);
        assert!(matches!(
            parse_record(&record),
            Err(ClaudeParseError::InvalidField {
                field: "non-compaction total mismatch",
                ..
            })
        ));
    }
}

#[test]
fn truncated_final_numbers_preserve_complete_records() {
    let original = final_record();
    let expected = parse_record(&original).unwrap();
    for number in ["-", "1.", "1e", "1E", "1e+", "1E-", "-1.2e+"] {
        let source = format!("{original}\n{{\"ignored\":{number}");
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert_eq!(parsed.events, expected.events, "{number}");
        assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
        assert_eq!(parsed.notices.len(), 1);
        assert_eq!(parsed.notices[0].code, "truncated_tail");
        assert_eq!(parsed.notices[0].line.unwrap().get(), 2);
        assert!(parse(format!("{source}\n").as_bytes(), SOURCE_PATH).is_err());
    }
    for number in ["1e++", "1..", "01.", "+", "--"] {
        let source = format!("{original}\n{{\"ignored\":{number}");
        assert!(
            matches!(
                parse(source.as_bytes(), SOURCE_PATH),
                Err(ClaudeParseError::MalformedLine { line: 2 })
            ),
            "{number}"
        );
    }
}

#[test]
fn incomplete_final_unicode_escapes_require_hex_digits() {
    let original = final_record();
    for escape in [r"\u", r"\u0", r"\u0a", r"\u0aF", r"\\uX", r#"\"\u0a"#] {
        let source = format!("{original}\n{{\"ignored\":\"{escape}");
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
        assert_eq!(parsed.events.len(), 1);
    }
    for escape in [r"\uX", r"\u0X", r"\u00X", r"\uX.", r#"\"\uX"#] {
        let source = format!("{original}\n{{\"ignored\":\"{escape}");
        assert!(
            matches!(
                parse(source.as_bytes(), SOURCE_PATH),
                Err(ClaudeParseError::MalformedLine { line: 2 })
            ),
            "{escape}"
        );
    }
}

#[test]
fn only_truncated_final_json_is_tolerated_and_payloads_are_discarded() {
    let original = final_record();
    for tail in ["{", "{\"ignored\":\"PRIVATE", "{\"ignored\":[1,"] {
        let parsed = parse(format!("{original}\n{tail}").as_bytes(), SOURCE_PATH).unwrap();
        assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
        assert_eq!(parsed.notices[0].count.get(), 1);
        assert_eq!(parsed.notices[0].line.unwrap().get(), 2);
        assert_eq!(parsed.events.len(), 1);
        assert!(parse(format!("{original}\n{tail}\n").as_bytes(), SOURCE_PATH).is_err());
    }
    for tail in [
        "{\"ignored\": !}",
        "{\"ignored\": !",
        "{\"ignored\":[1,]}",
        "{} garbage",
    ] {
        let error = parse(format!("{original}\n{tail}").as_bytes(), SOURCE_PATH).unwrap_err();
        assert!(matches!(error, ClaudeParseError::MalformedLine { line: 2 }));
    }
    let mut utf8 = format!("{original}\n{{\"ignored\":\"").into_bytes();
    utf8.extend_from_slice(&[0xe2, 0x82]);
    assert_eq!(
        parse(&utf8, SOURCE_PATH).unwrap().completion,
        ParseCompletion::IncompleteFinalLine
    );
    utf8.push(b'\n');
    assert!(matches!(
        parse(&utf8, SOURCE_PATH),
        Err(ClaudeParseError::InvalidUtf8 { line: 2 })
    ));
    for prefix in ["bad", "{}", "{\"ignored\":", r#"{"ignored":"\u"#] {
        let mut bytes = format!("{original}\n{prefix}").into_bytes();
        bytes.push(0xe2);
        assert!(parse(&bytes, SOURCE_PATH).is_err());
    }
    let mut content = original.clone();
    content["message"]["content"] = json!([{"type": "text", "text": "PRIVATE_CONVERSATION"}]);
    content["toolUseResult"] = json!({"text": "PRIVATE_TOOL", "usage": {"input_tokens": 999999}});
    let parsed = parse_record(&content).unwrap();
    assert_eq!(parsed, parse_record(&original).unwrap());
    assert!(!format!("{parsed:?}").contains("PRIVATE"));

    struct FailingReader;
    impl Read for FailingReader {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    let error = ClaudeSessionParser::new()
        .parse(
            &mut BufReader::new(FailingReader),
            ParseContext {
                source_path: Path::new(SOURCE_PATH),
            },
        )
        .unwrap_err();
    assert!(matches!(error, ClaudeParseError::Io { line: 1, .. }));
}

#[test]
fn unsupported_finals_can_be_corrected_and_synthetic_detection_is_explicit() {
    let original = final_record();
    let mut unsupported = original.clone();
    unsupported["message"]["usage"]["iterations"] = json!([]);
    let mut placeholder = original.clone();
    placeholder["message"]["stop_reason"] = Value::Null;
    for (source, first_line) in [
        (format!("{original}\n{unsupported}\n{placeholder}"), 2),
        (format!("{unsupported}\n{unsupported}"), 1),
    ] {
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert!(parsed.events.is_empty());
        assert_eq!(parsed.notices.len(), 1);
        assert_eq!(parsed.notices[0].count.get(), 1);
        assert_eq!(parsed.notices[0].line.unwrap().get(), first_line);
    }
    assert_eq!(
        parse(format!("{unsupported}\n{original}").as_bytes(), SOURCE_PATH).unwrap(),
        parse_record(&original).unwrap()
    );

    let mut zero = original.clone();
    zero["message"]["id"] = json!("arbitrary-response-id");
    zero["message"]["usage"] = json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    assert_eq!(parse_record(&zero).unwrap().events.len(), 1);
    zero["message"]["model"] = json!("<synthetic>");
    assert!(parse_record(&zero).unwrap().events.is_empty());
    zero["message"]["usage"]["input_tokens"] = json!(1);
    assert_eq!(parse_record(&zero).unwrap().events.len(), 1);
}

#[test]
fn parser_is_usable_through_session_adapter() {
    struct TempTree(std::path::PathBuf);
    impl Drop for TempTree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let tree = TempTree(std::env::temp_dir().join(format!(
        "token-tracker-claude-parser-{}",
        std::process::id()
    )));
    let project = tree.0.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("11111111-1111-4111-8111-111111111111.jsonl"),
        SNAPSHOTS,
    )
    .unwrap();
    let adapter = SessionAdapter::new(
        ClaudeSessionDiscovery::new(&tree.0),
        ClaudeSessionParser::new(),
    );
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let result = adapter.synchronize(&mut store).unwrap();
    assert_eq!(result.counts.files_imported, 1);
    assert!(result.warnings.is_empty());
    assert_eq!(store.usage_snapshot().unwrap().sessions.len(), 1);
}
