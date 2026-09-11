use token_tracker::domain::{EstimateUnavailableReason, ModelAttribution, ServiceTier};
use token_tracker::pricing::openai::lookup_rates;

#[test]
fn verified_rates_use_the_whole_request_band_and_explicit_model_aliases() {
    use ServiceTier::{Fast, Standard};

    // Microdollars per million tokens: input, cache read, cache write, output.
    for (model, tier, short, long) in [
        (
            "gpt-6-astra",
            Standard,
            [10_000_000, 1_000_000, 12_500_000, 50_000_000],
            Some([20_000_000, 2_000_000, 25_000_000, 75_000_000]),
        ),
        (
            "gpt-6-astra",
            Fast,
            [20_000_000, 2_000_000, 25_000_000, 100_000_000],
            Some([40_000_000, 4_000_000, 50_000_000, 150_000_000]),
        ),
        (
            "gpt-5.6-sol",
            Standard,
            [4_000_000, 400_000, 5_000_000, 20_000_000],
            Some([8_000_000, 800_000, 10_000_000, 30_000_000]),
        ),
        (
            "gpt-5.6-sol",
            Fast,
            [8_000_000, 800_000, 10_000_000, 40_000_000],
            Some([16_000_000, 1_600_000, 20_000_000, 60_000_000]),
        ),
        (
            "gpt-5.6-terra",
            Standard,
            [2_000_000, 200_000, 2_500_000, 12_000_000],
            Some([4_000_000, 400_000, 5_000_000, 18_000_000]),
        ),
        (
            "gpt-5.6-terra",
            Fast,
            [4_000_000, 400_000, 5_000_000, 24_000_000],
            Some([8_000_000, 800_000, 10_000_000, 36_000_000]),
        ),
        (
            "gpt-5.6-luna",
            Standard,
            [200_000, 20_000, 250_000, 1_200_000],
            Some([400_000, 40_000, 500_000, 1_800_000]),
        ),
        (
            "gpt-5.6-luna",
            Fast,
            [400_000, 40_000, 500_000, 2_400_000],
            Some([800_000, 80_000, 1_000_000, 3_600_000]),
        ),
        (
            "gpt-5.5",
            Standard,
            [5_000_000, 500_000, 5_000_000, 30_000_000],
            Some([10_000_000, 1_000_000, 10_000_000, 45_000_000]),
        ),
        (
            "gpt-5.5",
            Fast,
            [12_500_000, 1_250_000, 12_500_000, 75_000_000],
            None,
        ),
        (
            "gpt-5.4-mini",
            Standard,
            [750_000, 75_000, 750_000, 4_500_000],
            Some([750_000, 75_000, 750_000, 4_500_000]),
        ),
        (
            "gpt-5.4-mini",
            Fast,
            [1_500_000, 150_000, 1_500_000, 9_000_000],
            Some([1_500_000, 150_000, 1_500_000, 9_000_000]),
        ),
    ] {
        let names: &[&str] = match model {
            "gpt-5.6-sol" => &["gpt-5.6-sol", "gpt-5.6"],
            "gpt-5.5" => &["gpt-5.5", "gpt-5.5-2026-04-23"],
            "gpt-5.4-mini" => &["gpt-5.4-mini", "gpt-5.4-mini-2026-03-17"],
            _ => &[model],
        };
        for name in names {
            let attribution = ModelAttribution {
                provider: "openai".into(),
                model: (*name).into(),
            };
            for (input, expected) in [
                (0, Some(short)),
                (272_000, Some(short)),
                (272_001, long),
                (u128::MAX, long),
            ] {
                let rates = lookup_rates(&attribution, &tier, input);
                assert_eq!(
                    rates.map(|rates| [
                        rates.input,
                        rates.cache_read,
                        rates.cache_write,
                        rates.output
                    ]),
                    expected.ok_or(EstimateUnavailableReason::UnsupportedContextBand),
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
        ("openai", "codex-auto-review", Standard, UnsupportedModel),
        ("openai", "codex-auto-review", Fast, UnsupportedModel),
        ("openai", "gpt-5.5-custom", Fast, UnsupportedModel),
        ("openai", "gpt-5.4-mini", Unknown, UnknownTier),
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
