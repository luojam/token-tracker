use super::UsageEvent;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportFilters {
    pub agents: Vec<String>,
    pub providers: Vec<String>,
    pub models: Vec<String>,
}

impl ReportFilters {
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty() && self.providers.is_empty() && self.models.is_empty()
    }

    pub fn matches(&self, event: &UsageEvent) -> bool {
        self.matches_dimensions(
            event.identity.agent.as_str(),
            event.attribution.as_ref().map(|a| a.provider.as_str()),
            event.attribution.as_ref().map(|a| a.model.as_str()),
        )
    }

    pub fn matches_dimensions(
        &self,
        agent: &str,
        provider: Option<&str>,
        model: Option<&str>,
    ) -> bool {
        let matches = |values: &[String], value: Option<&str>| {
            values.is_empty() || value.is_some_and(|value| values.iter().any(|v| v == value))
        };

        matches(&self.agents, Some(agent))
            && matches(&self.providers, provider)
            && matches(&self.models, model)
    }
}
