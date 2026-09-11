use super::*;
use crate::domain::{
    AnthropicBilling, CacheWriteTokens, KnownRequests, PricingContext, RequestBreakdown,
    ServiceSpeed, ServiceTier, TierEvidence,
};

fn context(tokens: TokenCounts) -> PricingContext {
    PricingContext::Anthropic(AnthropicBilling {
        tier: ServiceTier::Standard,
        tier_evidence: TierEvidence::ServedResponse,
        speed: ServiceSpeed::Fast,
        requests: RequestBreakdown::KnownRequests(KnownRequests::new(tokens)),
        cache_writes: Some(vec![CacheWriteTokens {
            duration_seconds: 300,
            tokens: tokens.cache_write,
        }]),
    })
}

#[test]
fn billing_round_trips_and_corrections_are_atomic_with_usage() {
    let database = TempDatabase::new();
    let mut import = session_import("/sessions/billing.jsonl", 10);
    import.parsed.events[0].pricing_context = Some(context(import.parsed.events[0].tokens));
    {
        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        store.commit_import(&import).unwrap();
    }
    let mut store = SqliteUsageStore::open(&database.path).unwrap();
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0].event,
        import.parsed.events[0]
    );
    assert_eq!(
        store.commit_import(&import).unwrap(),
        CommitImportOutcome::Applied(ImportStats::default())
    );
    let before = store.usage_snapshot().unwrap();
    let state = store.source_states(&"pi".into()).unwrap();
    import.parsed.events[0].tokens.input = 20;
    assert!(store.commit_import(&import).is_err());
    assert_eq!(store.usage_snapshot().unwrap(), before);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), state);
    import.parsed.events[0].pricing_context = Some(context(import.parsed.events[0].tokens));
    assert_eq!(
        store.commit_import(&import).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            observations_updated: 1,
            ..ImportStats::default()
        })
    );
    import.parsed.events[0].pricing_context = None;
    store.commit_import(&import).unwrap();
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0].event,
        import.parsed.events[0]
    );
    assert_eq!(
        store
            .connection
            .query_row("SELECT COUNT(*) FROM billing_inputs", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn normalization_failures_and_omissions_preserve_billing() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut original = session_import("/sessions/a.jsonl", 10);
    original.parsed.events[0].pricing_context = Some(context(original.parsed.events[0].tokens));
    store.commit_import(&original).unwrap();
    let mut other = original.clone();
    other.source.path = "/sessions/b.jsonl".into();
    store.commit_import(&other).unwrap();
    let before = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();

    let mut normalized = original.clone();
    normalized.normalization_version = 2;
    normalized.parsed.events[0].tokens.input = 20;
    normalized.parsed.events[0].pricing_context = Some(context(normalized.parsed.events[0].tokens));
    let mut invalid = normalized.parsed.events[0].clone();
    invalid.identity.adapter_key = "invalid-event".into();
    invalid.tokens.input = u64::MAX;
    invalid.pricing_context = None;
    normalized.parsed.events.push(invalid);
    assert!(matches!(
        store.commit_import(&normalized),
        Err(SqliteStoreError::ValueOutOfRange(_))
    ));
    assert_eq!(store.usage_snapshot().unwrap(), before);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);

    normalized.parsed.events.clear();
    store.commit_import(&normalized).unwrap();
    assert_eq!(store.usage_snapshot().unwrap(), before);
}

#[test]
fn corrupt_billing_and_out_of_range_counts_are_rejected() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut import = session_import("/sessions/billing.jsonl", 10);
    import.parsed.events[0].pricing_context = Some(context(import.parsed.events[0].tokens));
    store.commit_import(&import).unwrap();
    store
        .connection
        .execute("UPDATE billing_inputs SET facts = '{}'", [])
        .unwrap();
    assert!(matches!(
        store.usage_snapshot(),
        Err(SqliteStoreError::CorruptData(_))
    ));
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    assert!(matches!(
        store.commit_import(&session_import("/sessions/huge.jsonl", u64::MAX)),
        Err(SqliteStoreError::ValueOutOfRange(_))
    ));
    assert!(store.usage_snapshot().unwrap().observations.is_empty());
    assert!(store.source_states(&"pi".into()).unwrap().is_empty());
}

#[test]
fn stored_billing_rejects_invalid_breakdowns() {
    use crate::storage::billing::decode;
    use serde_json::json;

    let tokens = TokenCounts::default();
    let request = |input| TokenCounts { input, ..tokens };
    for (field, value) in [
        ("requests", json!({"known_requests": []})),
        ("requests", json!({"known_requests": [request(1)]})),
        (
            "requests",
            json!({"known_requests": [request(u64::MAX), request(1)]}),
        ),
        (
            "cache_writes",
            json!([{"duration_seconds": 300, "tokens": 1}]),
        ),
        (
            "cache_writes",
            json!([
                {"duration_seconds": 300, "tokens": u64::MAX},
                {"duration_seconds": 3600, "tokens": 1}
            ]),
        ),
    ] {
        let mut stored = serde_json::to_value(context(tokens)).unwrap();
        stored["anthropic"][field] = value;
        assert!(matches!(
            decode(Some(stored.to_string()), tokens),
            Err(SqliteStoreError::CorruptData(_))
        ));
    }
}

#[test]
fn unsupported_schema_versions_are_rejected_and_left_untouched() {
    for version in [-1, 2, 3, 4, 5] {
        let database = TempDatabase::new();
        let connection = Connection::open(&database.path).unwrap();
        connection
            .execute_batch("CREATE TABLE preserved(value); INSERT INTO preserved VALUES (42)")
            .unwrap();
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        assert!(
            matches!(SqliteUsageStore::open(&database.path), Err(SqliteStoreError::UnsupportedSchemaVersion(v)) if v == version)
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM preserved", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            42
        );
    }
}
