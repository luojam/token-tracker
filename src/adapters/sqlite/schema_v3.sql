ALTER TABLE source_observations ADD COLUMN pricing_anthropic TEXT
    CHECK (pricing_anthropic IS NULL OR
           (pricing_tier IS NOT NULL AND pricing_request_usage IS NULL
            AND typeof(pricing_anthropic) = 'text'));

PRAGMA user_version = 3;
