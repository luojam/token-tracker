use super::ObjectWire;
use crate::domain::{
    CacheDetail, ModelAttribution, PricingContext, RequestGranularity, ServiceTier, TierEvidence,
};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum RawServiceTier {
    Missing,
    Null,
    Value(String),
}

#[derive(Default)]
pub(super) struct ContextState {
    threads: BTreeMap<String, ThreadContext>,
    scope: Option<String>,
    active: Option<TurnContext>,
}

#[derive(Default)]
struct ThreadContext {
    provider: Option<String>,
    settings: Option<RawServiceTier>,
}

struct TurnContext {
    id: String,
    owner: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    has_context: bool,
    model_conflict: bool,
    tier: TierContext,
}

#[derive(Clone, PartialEq, Eq)]
struct TierContext {
    tier: ServiceTier,
    raw: RawServiceTier,
    evidence: TierEvidence,
}

impl TierContext {
    fn new(raw: Option<&RawServiceTier>) -> Self {
        let tier = match raw {
            Some(RawServiceTier::Value(value)) => match value.as_str() {
                "default" => ServiceTier::Standard,
                "priority" | "fast" => ServiceTier::Fast,
                "auto" => ServiceTier::Unknown,
                _ => ServiceTier::Unsupported(value.clone()),
            },
            _ => ServiceTier::Unknown,
        };
        Self {
            tier,
            raw: raw.cloned().unwrap_or(RawServiceTier::Missing),
            evidence: if raw.is_some() {
                TierEvidence::RequestedSetting
            } else {
                TierEvidence::Unknown
            },
        }
    }

    fn invalidate(&mut self) {
        self.tier = ServiceTier::Unknown;
        self.evidence = TierEvidence::Unknown;
    }
}

impl ContextState {
    pub(super) fn accept_header(&mut self, id: &str, provider: Option<String>, scoped: bool) {
        let thread = self.threads.entry(id.to_owned()).or_default();
        thread.provider = provider.clone();
        self.scope = scoped.then(|| id.to_owned());
        if let Some(turn) = &mut self.active
            && (turn.owner != self.scope || turn.provider != provider)
        {
            turn.tier.invalidate();
            if !turn.has_context {
                turn.owner = self.scope.clone();
                turn.provider = self.scope.as_ref().and(provider);
            } else {
                turn.provider = None;
            }
        }
    }

    pub(super) fn accept_boundary(&mut self, boundary: &str, id: &str) {
        self.active = if boundary == "task_started" {
            let thread = self.scope.as_ref().and_then(|id| self.threads.get(id));
            Some(TurnContext {
                id: id.to_owned(),
                owner: self.scope.clone(),
                provider: thread.and_then(|thread| thread.provider.clone()),
                model: None,
                has_context: false,
                model_conflict: false,
                tier: TierContext::new(thread.and_then(|thread| thread.settings.as_ref())),
            })
        } else {
            None
        };
    }

    pub(super) fn accept_context(&mut self, model: Option<String>) {
        if let Some(turn) = &mut self.active {
            if turn.has_context && turn.model != model {
                turn.model_conflict = true;
            } else if !turn.has_context {
                turn.model = model.filter(|value| !value.trim().is_empty());
                turn.has_context = true;
            }
        }
    }

    pub(super) fn accept_settings(&mut self, settings: SettingsWire, allow_unscoped: bool) {
        let owner = settings.thread_id.as_ref().or(self.scope.as_ref());
        let Some(owner) = owner else { return };
        let Some(thread) = self.threads.get_mut(owner) else {
            return;
        };
        // Unscoped review events may be forwarded from the child, not local defaults.
        let raw = (settings.thread_id.is_some() || allow_unscoped)
            .then_some(settings.thread_settings.0.service_tier.0);
        let changed = thread.settings != raw;
        thread.settings = raw;
        // A changed default is not a request-start or served-tier record.
        if changed
            && let Some(turn) = &mut self.active
            && turn.owner.as_ref() == Some(owner)
        {
            turn.tier.invalidate();
        }
    }

    pub(super) fn observation(
        &self,
        turn_id: &str,
        thread_id: Option<&str>,
        granularity: RequestGranularity,
        cache_detail: CacheDetail,
    ) -> (Option<ModelAttribution>, PricingContext, RawServiceTier) {
        let turn = self.active.as_ref().filter(|turn| {
            turn.id == turn_id
                && thread_id.is_none_or(|id| turn.owner.as_deref().is_none_or(|owner| owner == id))
        });
        let attribution = turn.and_then(|turn| {
            if turn.model_conflict {
                return None;
            }
            let provider = match (turn.owner.as_deref(), thread_id) {
                (None, Some(id)) => self
                    .threads
                    .get(id)
                    .and_then(|thread| thread.provider.as_ref()),
                _ => turn.provider.as_ref(),
            };
            Some(ModelAttribution {
                provider: provider.filter(|value| !value.trim().is_empty())?.clone(),
                model: turn.model.clone()?,
            })
        });
        let tier = turn
            .filter(|turn| thread_id.is_none_or(|id| turn.owner.as_deref() == Some(id)))
            .map(|turn| turn.tier.clone())
            .unwrap_or_else(|| TierContext::new(None));
        (
            attribution,
            PricingContext {
                tier: tier.tier,
                provider: "openai".into(),
                speed: ServiceTier::Standard,
                tier_evidence: tier.evidence,
                request_granularity: granularity,
                cache_detail,
                request_usage: None,
                cache_writes: None,
            },
            tier.raw,
        )
    }
}

#[derive(Deserialize)]
pub(super) struct SettingsWire {
    thread_id: Option<String>,
    thread_settings: ObjectWire<SettingsSnapshot>,
}

#[derive(Deserialize)]
struct SettingsSnapshot {
    #[serde(default)]
    service_tier: TierWire,
}

struct TierWire(RawServiceTier);

impl Default for TierWire {
    fn default() -> Self {
        Self(RawServiceTier::Missing)
    }
}

impl<'de> Deserialize<'de> for TierWire {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<String>::deserialize(deserializer)
            .map(|value| Self(value.map_or(RawServiceTier::Null, RawServiceTier::Value)))
    }
}
