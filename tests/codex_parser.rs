use std::error::Error;
use std::io::{self, BufReader, Cursor, Read};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{ParseCompletion, ParseContext, ParsedSession, SessionParser};
use token_tracker::domain::{AgentId, ParentSession, SessionMetadata, Timestamp};

const RESPONSE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");
const FORK: &str = include_str!("fixtures/codex/legacy-fork.jsonl");
const HEADER: &str = r#"{"timestamp":"2026-01-02T00:00:00Z","type":"session_meta","payload":{"id":"thread-main","timestamp":"2026-01-01T02:00:00+02:00","cwd":"/work/fixture","cli_version":"0.153.4"}}"#;

fn parse(source: &str) -> Result<ParsedSession, CodexParseError> {
    parse_bytes(source.as_bytes())
}

fn parse_bytes(source: &[u8]) -> Result<ParsedSession, CodexParseError> {
    CodexSessionParser::new().parse(
        &mut BufReader::with_capacity(1, Cursor::new(source)),
        ParseContext {
            source_path: Path::new("/does-not-exist/rollout-unrelated-name.jsonl"),
        },
    )
}

fn record(entry_type: &str, payload: Value) -> Value {
    json!({"timestamp": "2026-01-01T00:00:05Z", "type": entry_type, "payload": payload})
}

#[test]
fn preserves_original_metadata_across_resume_and_client_upgrades() {
    let original = parse(HEADER).unwrap();
    assert_eq!(
        original.metadata,
        SessionMetadata {
            agent: AgentId::from("codex"),
            session_id: "thread-main".into(),

            working_directory: Some(PathBuf::from("/work/fixture")),
            started_at: Timestamp::from_unix_milliseconds(1_767_225_600_000),
            name: None,
            parent_session: None,
        }
    );
    assert!(original.events.is_empty());
    assert_eq!(original.completion, ParseCompletion::Complete);

    let mut resumed: Value = serde_json::from_str(HEADER).unwrap();
    resumed["timestamp"] = json!("2026-02-01T00:00:00Z");
    resumed["payload"]["timestamp"] = json!("2026-01-01T00:00:00Z");
    resumed["payload"]["cwd"] = json!("/work/elsewhere");
    resumed["payload"]["cli_version"] = json!("future-client");
    let parsed = parse(&format!("{HEADER}\n{resumed}")).unwrap();
    assert_eq!(parsed.metadata, original.metadata);

    for fixture in [
        include_str!("fixtures/codex/legacy-resume-compaction.jsonl"),
        include_str!("fixtures/codex/upgrade-response-first.jsonl"),
    ] {
        let first = parse(fixture.lines().next().unwrap()).unwrap();
        assert_eq!(parse(fixture).unwrap().metadata, first.metadata);
    }

    let minimal = record(
        "session_meta",
        json!({"id": "thread-minimal", "timestamp": "2026-01-01T00:00:00Z"}),
    );
    let parsed = parse(&minimal.to_string()).unwrap();
    assert_eq!(parsed.metadata.working_directory, None);
}

#[test]
fn keeps_fork_owner_and_uses_explicit_subagent_parent() {
    for fixture in [
        FORK,
        include_str!("fixtures/codex/legacy-partial-fork.jsonl"),
    ] {
        let parsed = parse(fixture).unwrap();
        assert_eq!(parsed.metadata.session_id, "thread-fork");
        assert_eq!(
            parsed.metadata.started_at,
            Timestamp::from_unix_milliseconds(1_767_312_000_000)
        );
        assert_eq!(
            parsed.metadata.parent_session,
            Some(ParentSession::SessionId("thread-parent".into()))
        );
    }

    let subagent = include_str!("fixtures/codex/response-subagent.jsonl");
    let parsed = parse(subagent).unwrap();
    assert_eq!(parsed.metadata.session_id, "thread-child");
    assert_eq!(
        parsed.metadata.parent_session,
        Some(ParentSession::SessionId("thread-main".into()))
    );
    let no_parent = subagent.replace(",\"parent_thread_id\":\"thread-main\"", "");
    assert_eq!(parse(&no_parent).unwrap().metadata.parent_session, None);
}

#[test]
fn rejects_unrelated_headers_and_conflicting_original_metadata() {
    let first: Value = serde_json::from_str(HEADER).unwrap();
    for (field, value) in [
        ("id", "thread-impostor"),
        ("timestamp", "2026-02-01T00:00:00Z"),
        ("parent_thread_id", "thread-parent"),
        ("forked_from_id", "thread-parent"),
    ] {
        let mut changed = first.clone();
        changed["payload"][field] = json!(value);
        assert!(matches!(
            parse(&format!("{HEADER}\n{changed}")),
            Err(CodexParseError::InvalidField { line: 2, .. })
        ));
    }

    let owner = FORK.lines().next().unwrap();
    let ancestor = FORK.lines().nth(1).unwrap();
    let mut conflict: Value = serde_json::from_str(owner).unwrap();
    conflict["payload"]["parent_thread_id"] = json!("different-parent");
    assert!(parse(&conflict.to_string()).is_err());
    conflict["payload"]["parent_thread_id"] = json!("thread-fork");
    conflict["payload"]["forked_from_id"] = json!("thread-fork");
    assert!(parse(&conflict.to_string()).is_err());

    let mut ancestor_change: Value = serde_json::from_str(ancestor).unwrap();
    ancestor_change["payload"]["timestamp"] = json!("2025-01-01T00:00:00Z");
    assert!(matches!(
        parse(&format!("{owner}\n{ancestor}\n{ancestor_change}")),
        Err(CodexParseError::InvalidField { line: 3, .. })
    ));
    ancestor_change["payload"]["forked_from_id"] = json!("thread-fork");
    assert!(parse(&format!("{owner}\n{ancestor_change}")).is_err());

    let task = record("event_msg", json!({"type": "task_started", "turn_id": "a"}));
    assert!(matches!(
        parse(&format!("{owner}\n{task}\n{ancestor}")),
        Err(CodexParseError::InvalidField { line: 3, .. })
    ));
    let subagent_owner = owner.replace("forked_from_id", "parent_thread_id");
    assert!(parse(&format!("{subagent_owner}\n{ancestor}")).is_err());
}

#[test]
fn requires_a_complete_header_with_original_id_and_source_timestamp() {
    assert!(matches!(parse(""), Err(CodexParseError::MissingHeader)));
    for source in ["{", r#"{"type":"session_meta","payload":"#] {
        assert!(matches!(
            parse(source),
            Err(CodexParseError::IncompleteHeader)
        ));
    }
    assert!(matches!(
        parse(r#"{"type":"response_item","payload":{}}"#),
        Err(CodexParseError::InvalidHeader)
    ));

    for (pointer, value) in [
        ("/payload/id", Value::Null),
        ("/payload/id", json!(" ")),
        ("/payload/timestamp", Value::Null),
        ("/payload/timestamp", json!("SECRET_INVALID_TIMESTAMP")),
        ("/timestamp", json!("SECRET_INVALID_TIMESTAMP")),
        ("/payload", json!("SECRET_NOT_METADATA")),
    ] {
        let mut header: Value = serde_json::from_str(HEADER).unwrap();
        *header.pointer_mut(pointer).unwrap() = value;
        let error = parse(&header.to_string()).unwrap_err();
        assert!(matches!(
            error,
            CodexParseError::InvalidField { line: 1, .. }
        ));
        assert!(!format!("{error:?} {error}").contains("SECRET"));
        assert!(error.source().is_none());
    }
    for missing in ["id", "timestamp"] {
        let mut header: Value = serde_json::from_str(HEADER).unwrap();
        header["payload"].as_object_mut().unwrap().remove(missing);
        assert!(parse(&header.to_string()).is_err());
    }
    let mut bytes = b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"".to_vec();
    bytes.extend_from_slice(&[0xf0, 0x9f]);
    assert!(matches!(
        parse_bytes(&bytes),
        Err(CodexParseError::IncompleteHeader)
    ));
}

#[test]
fn distinguishes_complete_and_truncated_tails_from_malformed_data() {
    for ending in ["", "\n", "\r\n"] {
        let source = format!("{}{ending}", RESPONSE.trim_end());
        assert_eq!(
            parse(&source).unwrap().completion,
            ParseCompletion::Complete
        );
    }
    let partial = include_str!("fixtures/codex/response-partial-tail.jsonl");
    assert_eq!(
        parse(partial).unwrap().completion,
        ParseCompletion::IncompleteFinalLine
    );
    for tail in [
        "{",
        r#"{"type":"future","payload":"#,
        r#"{"type":"future","payload":"\u0a"#,
        r#"{"type":"future","payload":"\\uX"#,
        r#"{"type":"future","payload":1."#,
        r#"{"type":"future","payload":1e+"#,
    ] {
        assert_eq!(
            parse(&format!("{HEADER}\n{tail}")).unwrap().completion,
            ParseCompletion::IncompleteFinalLine
        );
        assert!(matches!(
            parse(&format!("{HEADER}\n{tail}\n")),
            Err(CodexParseError::MalformedLine { line: 2 })
        ));
    }
    for tail in [
        r#"{"type":"future","payload":]}"#,
        r#"{"type":"future"} {"#,
        r#"{"type":"future","payload":"SECRET"} trailing"#,
        r#"{"type":"future","payload":"\uX"#,
        r#"{"type":"future","payload":1e++"#,
        r#"{"type":"future","payload":[1 2."#,
    ] {
        for ending in ["", "\n"] {
            let error = parse(&format!("{HEADER}\n{tail}{ending}")).unwrap_err();
            assert!(matches!(error, CodexParseError::MalformedLine { line: 2 }));
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
    }

    let prefix = format!("{HEADER}\n{{\"type\":\"future\",\"payload\":\"");
    let mut truncated = prefix.as_bytes().to_vec();
    truncated.extend_from_slice(&[0xf0, 0x9f]);
    assert_eq!(
        parse_bytes(&truncated).unwrap().completion,
        ParseCompletion::IncompleteFinalLine
    );
    truncated.push(b'\n');
    assert!(matches!(
        parse_bytes(&truncated),
        Err(CodexParseError::InvalidUtf8 { line: 2 })
    ));
    let mut invalid = prefix.into_bytes();
    invalid.push(0xff);
    assert!(matches!(
        parse_bytes(&invalid),
        Err(CodexParseError::InvalidUtf8 { line: 2 })
    ));
    for malformed_prefix in [
        r#"{"type":"future","payload":]}"#,
        r#"{"type":"future"} "#,
        r#"{"type":"future","payload":"#,
        r#"{"type":"future","payload":"\u"#,
    ] {
        let mut bytes = format!("{HEADER}\n{malformed_prefix}").into_bytes();
        bytes.extend_from_slice(&[0xf0, 0x9f]);
        assert!(matches!(
            parse_bytes(&bytes),
            Err(CodexParseError::MalformedLine { line: 2 })
        ));
    }
}

#[test]
fn validates_recognized_accounting_fields_without_exposing_values() {
    let response: Value = serde_json::from_str(RESPONSE.lines().nth(3).unwrap()).unwrap();
    for (pointer, value) in [
        ("/payload/usage/input_tokens", json!(-1)),
        ("/payload/usage/cached_input_tokens", json!(1.5)),
        ("/payload/usage/cache_write_input_tokens", Value::Null),
        (
            "/payload/usage/output_tokens",
            json!("SECRET_NOT_A_COUNTER"),
        ),
        (
            "/payload/usage/reasoning_output_tokens",
            json!({"SECRET": 1}),
        ),
        ("/payload/usage/total_tokens", Value::Null),
        ("/payload/usage", json!([100, 40, 0, 10, 2, 110])),
        ("/payload/turn_token_usage", Value::Null),
        ("/payload/thread_token_usage", json!(false)),
        ("/payload/response_id", json!("")),
        ("/payload/thread_id", json!(["SECRET_NOT_AN_ID"])),
        ("/payload/turn_id", Value::Null),
        ("/timestamp", json!("SECRET_NOT_A_TIMESTAMP")),
    ] {
        let mut changed = response.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        for ending in ["", "\n"] {
            let error = parse(&format!("{HEADER}\n{changed}{ending}")).unwrap_err();
            assert!(matches!(
                error,
                CodexParseError::InvalidField { line: 2, .. }
            ));
            assert!(error.to_string().contains("line 2"));
            assert!(!format!("{error:?} {error}").contains("SECRET"));
            assert!(error.source().is_none());
        }
    }
    let mut missing = response.clone();
    missing["payload"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("input_tokens");
    assert!(parse(&format!("{HEADER}\n{missing}")).is_err());
    let overflow = response.to_string().replace(
        "\"input_tokens\":100",
        "\"input_tokens\":18446744073709551616",
    );
    assert!(parse(&format!("{HEADER}\n{overflow}")).is_err());

    for payload in [
        json!({"type": "token_count", "info": "SECRET_NOT_INFO"}),
        json!({"type": "token_count", "info": {"total_token_usage": {}}}),
        json!({"type": "task_started", "turn_id": ""}),
        json!({"type": "task_complete", "turn_id": 3}),
        json!({"type": "turn_aborted"}),
    ] {
        let entry = record("event_msg", payload);
        let error = parse(&format!("{HEADER}\n{entry}")).unwrap_err();
        assert!(matches!(
            error,
            CodexParseError::InvalidField { line: 2, .. }
        ));
        assert!(!format!("{error:?} {error}").contains("SECRET"));
    }
    let context = record("turn_context", json!({"turn_id": false}));
    assert!(parse(&format!("{HEADER}\n{context}")).is_err());
    let array_context = record("turn_context", json!(["turn-main"]));
    assert!(parse(&format!("{HEADER}\n{array_context}")).is_err());
    assert!(parse(&format!("{HEADER}\n[\"future_record\"]")).is_err());

    let legacy = include_str!("fixtures/codex/legacy-fresh.jsonl");
    assert_eq!(parse(legacy).unwrap().completion, ParseCompletion::Complete);
    let bad_legacy = legacy.replace("\"input_tokens\":100", "\"input_tokens\":false");
    assert!(matches!(
        parse(&bad_legacy),
        Err(CodexParseError::InvalidField { line: 5, .. })
    ));
    let missing_info = record("event_msg", json!({"type": "token_count"}));
    assert!(parse(&format!("{HEADER}\n{missing_info}")).is_ok());
}

#[test]
fn ignores_unknown_fields_and_content_without_client_version_switches() {
    let mut header: Value = serde_json::from_str(HEADER).unwrap();
    header["payload"]["instructions"] = json!("SECRET_INSTRUCTIONS");
    header["payload"]["session_id"] = json!("shared-root");
    let records = [
        record(
            "response_item",
            json!({"type": "message", "content": "SECRET_PROMPT"}),
        ),
        record(
            "response_item",
            json!({"type": "function_call_output", "output": "SECRET_TOOL"}),
        ),
        record(
            "compacted",
            json!({"message": "SECRET_SUMMARY", "replacement_history": ["SECRET_HISTORY"]}),
        ),
        record(
            "event_msg",
            json!({"type": "future_event", "info": "SECRET_FUTURE"}),
        ),
        json!({"type": "future_record", "payload": {"usage": "SECRET_UNKNOWN"}}),
    ];
    let source = std::iter::once(header.to_string())
        .chain(records.iter().map(Value::to_string))
        .collect::<Vec<_>>()
        .join("\n");
    let parsed = parse(&source).unwrap();
    assert_eq!(parsed.metadata.session_id, "thread-main");
    assert_eq!(parsed.metadata.parent_session, None);
    assert!(!format!("{parsed:?}").contains("SECRET"));

    let fixture = include_str!("fixtures/codex/reject-legacy-first-mirror.jsonl");
    let prefix = fixture.lines().take(4).collect::<Vec<_>>().join("\n");
    for client in ["0.128.0", "0.153.4", "future-client"] {
        let parsed = parse(&prefix.replace("0.153.4", client)).unwrap();
        assert_eq!(parsed.completion, ParseCompletion::Complete);
    }
}

#[test]
fn read_failure_is_an_error_and_does_not_expose_reader_content() {
    struct FailingReader;
    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("SECRET_READER_CONTENT"))
        }
    }
    let input = Cursor::new(format!("{HEADER}\n").into_bytes()).chain(FailingReader);
    let error = CodexSessionParser::new()
        .parse(
            &mut BufReader::new(input),
            ParseContext {
                source_path: Path::new("/not-opened.jsonl"),
            },
        )
        .unwrap_err();
    assert!(matches!(error, CodexParseError::Io { line: 2, .. }));
    assert!(!format!("{error:?} {error}").contains("SECRET"));
    assert!(error.source().is_none());
}
