use super::*;
use crate::core::{
    CacheDetail, PricingContext, RawServiceTier, RequestGranularity, ServiceTier, TierEvidence,
};

fn context() -> PricingContext {
    PricingContext {
        tier: ServiceTier::Standard,
        raw_tier: RawServiceTier::Value("default".into()),
        tier_evidence: TierEvidence::RequestedSetting,
        request_granularity: RequestGranularity::ExactSingleRequest,
        cache_detail: CacheDetail::Complete,
    }
}

fn assert_version_one(store: &SqliteUsageStore) {
    assert_eq!(
        store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn fresh_database_includes_pricing_columns_and_keeps_pi_context_absent() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    assert_version_one(&store);
    let import = session_import("/sessions/pi.jsonl", 10);
    store.commit_import(&import).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations.len(), 1);
    assert_eq!(snapshot.observations[0].event, import.parsed.events[0]);

    let mut statement = store
        .connection
        .prepare(
            "SELECT name, type, \"notnull\" FROM pragma_table_info('source_observations')
             WHERE name LIKE 'pricing_%' ORDER BY cid",
        )
        .unwrap();
    let columns = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let expected = [
        "pricing_tier",
        "pricing_unsupported_tier",
        "pricing_raw_tier_kind",
        "pricing_raw_tier_value",
        "pricing_tier_evidence",
        "pricing_request_granularity",
        "pricing_cache_detail",
    ];
    assert_eq!(columns.len(), expected.len());
    for name in expected {
        assert!(columns.contains(&(name.into(), "TEXT".into(), false)));
    }
}

#[test]
fn snapshot_round_trips_every_pricing_enum_without_normalizing_facts() {
    let database = TempDatabase::new();
    let mut import = session_import("/sessions/codex.jsonl", u64::MAX);
    import.parsed.metadata.agent = AgentId::from("codex");
    let mut base = import.parsed.events.remove(0);
    base.identity.agent = AgentId::from("codex");
    let contexts = [
        None,
        Some(context()),
        Some(PricingContext {
            tier: ServiceTier::Fast,
            raw_tier: RawServiceTier::Value("priority".into()),
            tier_evidence: TierEvidence::ServedResponse,
            ..context()
        }),
        Some(PricingContext {
            tier: ServiceTier::Unknown,
            raw_tier: RawServiceTier::Missing,
            tier_evidence: TierEvidence::Unknown,
            request_granularity: RequestGranularity::AggregateOrUnknown,
            cache_detail: CacheDetail::Incomplete,
        }),
        Some(PricingContext {
            tier: ServiceTier::Unknown,
            raw_tier: RawServiceTier::Null,
            ..context()
        }),
        Some(PricingContext {
            tier: ServiceTier::Unknown,
            raw_tier: RawServiceTier::Value("auto".into()),
            ..context()
        }),
        Some(PricingContext {
            tier: ServiceTier::Unsupported("Batch / 未知".into()),
            raw_tier: RawServiceTier::Value("  DIFFERENT raw tier  ".into()),
            ..context()
        }),
        Some(PricingContext {
            tier: ServiceTier::Unsupported(String::new()),
            raw_tier: RawServiceTier::Value(String::new()),
            ..context()
        }),
    ];
    for (index, pricing_context) in contexts.into_iter().enumerate() {
        let mut event = base.clone();
        event.identity.adapter_key = format!("response-v1:{index}");
        event.pricing_context = pricing_context;
        import.parsed.events.push(event);
    }
    {
        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        assert_eq!(
            store.commit_import(&import).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: import.parsed.events.len() as u64,
                observations_inserted: import.parsed.events.len() as u64,
                observations_updated: 0,
            })
        );
    }
    let mut store = SqliteUsageStore::open(&database.path).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations.len(), import.parsed.events.len());
    for expected in &import.parsed.events {
        let actual = snapshot
            .observations
            .iter()
            .find(|observation| observation.event.identity == expected.identity)
            .unwrap();
        assert_eq!(&actual.event, expected);
        assert_eq!(actual.session, snapshot.sessions[0].key);
    }
    assert_eq!(
        store.commit_import(&import).unwrap(),
        CommitImportOutcome::Applied(ImportStats::default())
    );
}

#[test]
fn context_only_corrections_update_each_field_after_reopen_and_can_clear_context() {
    let database = TempDatabase::new();
    let mut import = session_import("/sessions/a.jsonl", 10);
    {
        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        store.commit_import(&import).unwrap();
    }
    let mut value = context();
    let mut corrections = vec![Some(value.clone())];
    value.tier = ServiceTier::Fast;
    corrections.push(Some(value.clone()));
    value.tier = ServiceTier::Unsupported("batch".into());
    corrections.push(Some(value.clone()));
    value.tier = ServiceTier::Unsupported("other".into());
    corrections.push(Some(value.clone()));
    value.raw_tier = RawServiceTier::Value("fast".into());
    corrections.push(Some(value.clone()));
    value.raw_tier = RawServiceTier::Null;
    corrections.push(Some(value.clone()));
    value.raw_tier = RawServiceTier::Missing;
    corrections.push(Some(value.clone()));
    value.tier_evidence = TierEvidence::ServedResponse;
    corrections.push(Some(value.clone()));
    value.request_granularity = RequestGranularity::AggregateOrUnknown;
    corrections.push(Some(value.clone()));
    value.cache_detail = CacheDetail::Incomplete;
    corrections.push(Some(value));
    corrections.push(None);

    for (index, correction) in corrections.into_iter().enumerate() {
        import.parsed.events[0].pricing_context = correction;
        import.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000 + index as i64);
        {
            let mut store = SqliteUsageStore::open(&database.path).unwrap();
            assert_eq!(
                store.commit_import(&import).unwrap(),
                CommitImportOutcome::Applied(ImportStats {
                    observations_updated: 1,
                    ..ImportStats::default()
                })
            );
        }
        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        let snapshot = store.usage_snapshot().unwrap();
        assert_eq!(snapshot.observations.len(), 1);
        assert_eq!(snapshot.observations[0].event, import.parsed.events[0]);
        assert_eq!(
            store.commit_import(&import).unwrap(),
            CommitImportOutcome::Applied(ImportStats::default())
        );
    }
}

#[test]
fn malformed_pricing_columns_fail_constraints_and_reads_even_when_checks_are_bypassed() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut import = session_import("/sessions/a.jsonl", 10);
    import.parsed.events[0].pricing_context = Some(context());
    store.commit_import(&import).unwrap();
    for assignment in [
        "pricing_tier = 'invalid'",
        "pricing_tier = NULL",
        "pricing_tier = 'unsupported'",
        "pricing_unsupported_tier = 'unexpected'",
        "pricing_raw_tier_kind = 'invalid'",
        "pricing_raw_tier_kind = NULL",
        "pricing_raw_tier_kind = 'missing'",
        "pricing_raw_tier_value = NULL",
        "pricing_tier_evidence = 'invalid'",
        "pricing_tier_evidence = NULL",
        "pricing_request_granularity = 'invalid'",
        "pricing_request_granularity = NULL",
        "pricing_cache_detail = 'invalid'",
        "pricing_cache_detail = NULL",
    ] {
        let sql = format!("UPDATE source_observations SET {assignment}");
        let error = store.connection.execute(&sql, []).unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation),
            "{assignment}"
        );
        store
            .connection
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        store.connection.execute(&sql, []).unwrap();
        assert!(store.usage_snapshot().is_err(), "{assignment}");
        store
            .connection
            .pragma_update(None, "ignore_check_constraints", false)
            .unwrap();
        store.commit_import(&import).unwrap();
        assert_eq!(
            store.usage_snapshot().unwrap().observations[0].event,
            import.parsed.events[0]
        );
    }
}
