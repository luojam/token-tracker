use token_tracker::application::pricing::lookup_rates;
use token_tracker::core::{EstimateUnavailableReason, ModelAttribution, ServiceTier};

#[test]
fn verified_rates_use_the_whole_request_band_and_explicit_model_aliases() {
    use ServiceTier::{Fast, Standard};

    // Microdollars per million tokens: input, cache read, cache write, output.
    for (model, tier, short, long) in [
        (
            "gpt-6-astra",
            Standard,
            [10_000_000, 1_000_000, 12_500_000, 50_000_000],
            [20_000_000, 2_000_000, 25_000_000, 75_000_000],
        ),
        (
            "gpt-6-astra",
            Fast,
            [20_000_000, 2_000_000, 25_000_000, 100_000_000],
            [40_000_000, 4_000_000, 50_000_000, 150_000_000],
        ),
        (
            "gpt-5.6-sol",
            Standard,
            [4_000_000, 400_000, 5_000_000, 20_000_000],
            [8_000_000, 800_000, 10_000_000, 30_000_000],
        ),
        (
            "gpt-5.6-sol",
            Fast,
            [8_000_000, 800_000, 10_000_000, 40_000_000],
            [16_000_000, 1_600_000, 20_000_000, 60_000_000],
        ),
    ] {
        let names: &[&str] = if model == "gpt-5.6-sol" {
            &["gpt-5.6-sol", "gpt-5.6"]
        } else {
            &["gpt-6-astra"]
        };
        for name in names {
            let attribution = ModelAttribution {
                provider: "openai".into(),
                model: (*name).into(),
            };
            for (input, expected) in [
                (0, short),
                (272_000, short),
                (272_001, long),
                (u128::MAX, long),
            ] {
                let rates = lookup_rates(&attribution, &tier, input).unwrap();
                assert_eq!(
                    [
                        rates.input,
                        rates.cache_read,
                        rates.cache_write,
                        rates.output
                    ],
                    expected,
                    "{name} {tier:?} {input} input tokens",
                );
            }
        }
    }
}

#[test]
fn unknown_names_and_tiers_never_fall_back_to_supported_rates() {
    use EstimateUnavailableReason::{
        UnknownTier, UnsupportedModel, UnsupportedProvider, UnsupportedTier,
    };
    use ServiceTier::{Fast, Standard, Unknown, Unsupported};

    for (provider, model, tier, reason) in [
        ("", "gpt-6-astra", Standard, UnsupportedProvider),
        ("OpenAI", "gpt-6-astra", Fast, UnsupportedProvider),
        ("azure", "gpt-6-astra", Standard, UnsupportedProvider),
        ("openai", "", Standard, UnsupportedModel),
        ("openai", "gpt-6-astra-custom", Fast, UnsupportedModel),
        ("openai", "gpt-5.6-sol-2026-09-07", Fast, UnsupportedModel),
        ("openai", "GPT-5.6", Standard, UnsupportedModel),
        ("openai", "gpt-6-astra", Unknown, UnknownTier),
        (
            "openai",
            "gpt-5.6",
            Unsupported("flex".into()),
            UnsupportedTier,
        ),
        (
            "openai",
            "gpt-6-astra",
            Unsupported("ultrafast".into()),
            UnsupportedTier,
        ),
    ] {
        let attribution = ModelAttribution {
            provider: provider.into(),
            model: model.into(),
        };
        for input in [272_000, 272_001] {
            assert_eq!(lookup_rates(&attribution, &tier, input), Err(reason));
        }
    }
}
