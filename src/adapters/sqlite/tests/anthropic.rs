use super::*;
use crate::core::{
    AnthropicIteration, AnthropicIterationKind, AnthropicPricingContext, AnthropicUsage,
    AnthropicUsageComponent, CacheCreationTokens, PricingContext, RawServedValue,
};

fn component() -> AnthropicUsageComponent {
    AnthropicUsageComponent {
        tokens: TokenCounts {
            input: 10,
            output: 20,
            cache_read: 100,
            cache_write: 40,
        },
        cache_creation: Some(CacheCreationTokens {
            ephemeral_5m: 30,
            ephemeral_1h: 10,
        }),
    }
}

fn context(usage: AnthropicUsage) -> PricingContext {
    PricingContext::for_anthropic(AnthropicPricingContext {
        speed: RawServedValue::Value("standard".into()),
        service_tier: RawServedValue::Value("standard".into()),
        usage,
    })
}

fn claude_import() -> SessionImport {
    let mut import = session_import("/sessions/claude.jsonl", 10);
    import.parsed.metadata.agent = "claude".into();
    let event = &mut import.parsed.events[0];
    event.identity.agent = "claude".into();
    event.recorded_cost = None;
    event.tokens = component().tokens;
    event.pricing_context = Some(context(AnthropicUsage::Response(component())));
    import
}

#[test]
fn pricing_corrections_survive_reopen_without_changing_copied_observations() {
    let database = TempDatabase::new();
    let mut import = claude_import();
    let mut copy = import.clone();
    copy.source.path = "/sessions/copy.jsonl".into();
    let mut store = SqliteUsageStore::open(&database.path).unwrap();
    store.commit_import(&import).unwrap();
    store.commit_import(&copy).unwrap();

    let mut missing = context(AnthropicUsage::Response(AnthropicUsageComponent {
        cache_creation: None,
        ..component()
    }));
    let facts = missing.anthropic.as_mut().unwrap();
    facts.speed = RawServedValue::Null;
    facts.service_tier = RawServedValue::Missing;
    let iterations = context(AnthropicUsage::Iterations(
        [
            AnthropicIterationKind::Compaction,
            AnthropicIterationKind::Message,
        ]
        .map(|kind| AnthropicIteration {
            kind,
            usage: component(),
        })
        .to_vec(),
    ));
    for (pricing, tokens) in [
        (
            import.parsed.events[0].pricing_context.clone(),
            component().tokens,
        ),
        (Some(missing), component().tokens),
        (
            Some(iterations),
            TokenCounts {
                input: 20,
                output: 40,
                cache_read: 200,
                cache_write: 80,
            },
        ),
        (None, component().tokens),
    ] {
        import.parsed.events[0].pricing_context = pricing;
        import.parsed.events[0].tokens = tokens;
        store.commit_import(&import).unwrap();
        drop(store);
        store = SqliteUsageStore::open(&database.path).unwrap();
        let snapshot = store.usage_snapshot().unwrap();
        assert_eq!(snapshot.observations.len(), 2);
        for expected in [&import, &copy] {
            let actual = snapshot
                .observations
                .iter()
                .find(|observation| observation.session.source_path == expected.source.path)
                .unwrap();
            assert_eq!(actual.event, expected.parsed.events[0]);
        }
    }
}

#[test]
fn invalid_accounting_is_rejected_on_import_and_read() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let import = claude_import();
    store.commit_import(&import).unwrap();
    let before = store.usage_snapshot().unwrap();
    let mut bad_duration = component();
    bad_duration.cache_creation.as_mut().unwrap().ephemeral_5m = 29;
    let mut overflow = component();
    overflow.cache_creation = Some(CacheCreationTokens {
        ephemeral_5m: u64::MAX,
        ephemeral_1h: 41,
    });
    let mut duplicate = import.parsed.events[0].pricing_context.clone().unwrap();
    duplicate.request_usage = Some(vec![component().tokens]);
    let mut too_large = component();
    too_large.tokens.input = u64::MAX;
    let invalid = [
        context(AnthropicUsage::Response(bad_duration)),
        context(AnthropicUsage::Response(overflow)),
        context(AnthropicUsage::Response(AnthropicUsageComponent {
            tokens: TokenCounts::default(),
            cache_creation: None,
        })),
        context(AnthropicUsage::Iterations(vec![])),
        context(AnthropicUsage::Iterations(vec![
            AnthropicIteration {
                kind: AnthropicIterationKind::Message,
                usage: too_large,
            },
            AnthropicIteration {
                kind: AnthropicIterationKind::Compaction,
                usage: component(),
            },
        ])),
        duplicate,
    ];
    store
        .connection
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    for pricing in invalid {
        let mut replacement = import.clone();
        replacement.parsed.events[0].pricing_context = Some(pricing.clone());
        assert!(matches!(
            store.commit_import(&replacement),
            Err(SqliteStoreError::InvalidImport(_))
        ));
        assert_eq!(store.usage_snapshot().unwrap(), before);
        store
            .connection
            .execute(
                "UPDATE source_observations SET pricing_anthropic = ?1, pricing_request_usage = ?2",
                params![
                    serde_json::to_string(&pricing.anthropic).unwrap(),
                    pricing_context::encode_requests(Some(&pricing))
                ],
            )
            .unwrap();
        assert!(matches!(
            store.usage_snapshot(),
            Err(SqliteStoreError::CorruptData(_))
        ));
        store.commit_import(&import).unwrap();
    }
}
