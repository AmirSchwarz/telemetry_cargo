//! Consent is explicit, per-category and disabled by default.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use telemetry_schema::{Category, EventDefinition, Properties, PropertyRule, REDACTED};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Consent {
    pub essential_operations: bool,
    pub diagnostics: bool,
    pub performance: bool,
    pub usage_aggregate: bool,
    pub usage_session: bool,
    pub extended_support: bool,
}

impl Consent {
    pub fn enabled(&self, category: Category) -> bool {
        match category {
            Category::EssentialOperations => self.essential_operations,
            Category::Diagnostics => self.diagnostics,
            Category::Performance => self.performance,
            Category::UsageAggregate => self.usage_aggregate,
            Category::UsageSession => self.usage_session,
            Category::ExtendedSupport => self.extended_support,
        }
    }
    pub fn set(&mut self, category: Category, enabled: bool) {
        *match category {
            Category::EssentialOperations => &mut self.essential_operations,
            Category::Diagnostics => &mut self.diagnostics,
            Category::Performance => &mut self.performance,
            Category::UsageAggregate => &mut self.usage_aggregate,
            Category::UsageSession => &mut self.usage_session,
            Category::ExtendedSupport => &mut self.extended_support,
        } = enabled;
    }
}

#[derive(Clone, Debug)]
pub struct PrivacyGate {
    consent: Consent,
    essential_operations_reviewed: bool,
}

impl PrivacyGate {
    pub fn new(consent: Consent, essential_operations_reviewed: bool) -> Self {
        Self {
            consent,
            essential_operations_reviewed,
        }
    }
    pub fn enabled(&self, category: Category) -> bool {
        self.consent.enabled(category)
            && (category != Category::EssentialOperations || self.essential_operations_reviewed)
    }
    pub fn set_consent(&mut self, category: Category, enabled: bool) {
        self.consent.set(category, enabled);
    }
    pub fn allowed_categories(&self) -> BTreeSet<Category> {
        Category::ALL
            .into_iter()
            .filter(|c| self.enabled(*c))
            .collect()
    }
    /// Erase free-text error/context data before it can reach validation, queues,
    /// transport or diagnostic messages. Never attempt a best-effort PII regex.
    pub fn sanitize(&self, definition: &EventDefinition, properties: &mut Properties) {
        for (key, rule) in &definition.properties {
            if matches!(rule, PropertyRule::RedactedText) && properties.contains_key(key) {
                properties.insert(key.clone(), REDACTED.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_deny_every_category_and_essential_requires_review() {
        let mut gate = PrivacyGate::new(Consent::default(), false);
        assert!(gate.allowed_categories().is_empty());
        for category in Category::ALL {
            gate.set_consent(category, true);
        }
        assert!(!gate.enabled(Category::EssentialOperations));
        assert!(gate.enabled(Category::Diagnostics));
        let consent = Consent {
            essential_operations: true,
            ..Consent::default()
        };
        assert!(PrivacyGate::new(consent, true).enabled(Category::EssentialOperations));
    }
    #[test]
    fn synthetic_secrets_are_erased_entirely() {
        let registry = telemetry_schema::Registry::default();
        let gate = PrivacyGate::new(Consent::default(), false);
        let mut properties = Properties::from([
            ("code".into(), "io.failure".into()),
            (
                "message".into(),
                "token=sk-secret /home/private https://example.org/?q=secret".into(),
            ),
        ]);
        gate.sanitize(registry.definition("error").unwrap(), &mut properties);
        assert_eq!(properties["message"].as_str(), Some(REDACTED));
        assert_eq!(properties["code"].as_str(), Some("io.failure"));
    }
    #[test]
    fn withdrawal_removes_permission() {
        let mut gate = PrivacyGate::new(Consent::default(), false);
        gate.set_consent(Category::UsageSession, true);
        assert!(gate.allowed_categories().contains(&Category::UsageSession));
        gate.set_consent(Category::UsageSession, false);
        assert!(!gate.allowed_categories().contains(&Category::UsageSession));
    }
}
