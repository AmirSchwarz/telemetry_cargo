//! Versioned telemetry envelopes and a bounded, strict event registry.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt, time::SystemTime};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_REGISTRY_EVENTS: usize = 256;
pub const REDACTED: &str = "[redacted]";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    EssentialOperations,
    Diagnostics,
    Performance,
    UsageAggregate,
    UsageSession,
    ExtendedSupport,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::EssentialOperations,
        Self::Diagnostics,
        Self::Performance,
        Self::UsageAggregate,
        Self::UsageSession,
        Self::ExtendedSupport,
    ];
}

pub type Properties = BTreeMap<String, Value>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema_version: u32,
    pub event_id: String,
    pub app_id: String,
    pub app_version: String,
    pub platform: String,
    pub sdk_version: String,
    pub category: Category,
    pub event_name: String,
    /// Wall-clock timestamp in Unix milliseconds. Durations use monotonic time.
    pub occurred_at: u64,
    pub properties: Properties,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyRule {
    Identifier,
    Number,
    Boolean,
    Enum(Vec<String>),
    RedactedText,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventDefinition {
    pub name: String,
    pub category: Category,
    pub properties: BTreeMap<String, PropertyRule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_properties: usize,
    pub max_string_bytes: usize,
    pub max_event_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_properties: 32,
            max_string_bytes: 128,
            max_event_bytes: 8192,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> Result<(), SchemaError> {
        if self.max_properties == 0
            || self.max_properties > 128
            || self.max_string_bytes < REDACTED.len()
            || self.max_string_bytes > 4096
            || self.max_event_bytes < 256
            || self.max_event_bytes > 1024 * 1024
        {
            return Err(SchemaError("invalid event limits"));
        }
        Ok(())
    }
}

/// An intentionally bounded, input-free error description (safe for diagnostics).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemaError(pub &'static str);
impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for SchemaError {}

/// Semantic IDs are application-defined, coarse labels, never user-derived data.
pub fn validate_id(value: &str) -> Result<(), SchemaError> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(SchemaError("invalid semantic identifier"));
    }
    Ok(())
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Debug)]
pub struct Registry {
    definitions: BTreeMap<String, EventDefinition>,
}

impl Default for Registry {
    fn default() -> Self {
        let definitions: Vec<EventDefinition> =
            serde_json::from_str(include_str!("../schema/registry-v1.json"))
                .expect("built-in event registry is valid");
        let mut registry = Self {
            definitions: BTreeMap::new(),
        };
        for definition in definitions {
            registry
                .register(definition)
                .expect("built-in definition is valid");
        }
        registry
    }
}

impl Registry {
    pub fn register(&mut self, definition: EventDefinition) -> Result<(), SchemaError> {
        validate_id(&definition.name)?;
        if self.definitions.contains_key(&definition.name) {
            return Err(SchemaError("event definition already registered"));
        }
        if self.definitions.len() >= MAX_REGISTRY_EVENTS || definition.properties.len() > 32 {
            return Err(SchemaError("event registry budget exceeded"));
        }
        for (key, rule) in &definition.properties {
            validate_id(key)?;
            if let PropertyRule::Enum(values) = rule {
                if values.is_empty() || values.len() > 64 {
                    return Err(SchemaError("invalid property enum"));
                }
                for value in values {
                    validate_id(value)?;
                }
                let mut unique = values.clone();
                unique.sort();
                unique.dedup();
                if unique.len() != values.len() {
                    return Err(SchemaError("duplicate enum value"));
                }
            }
        }
        self.definitions.insert(definition.name.clone(), definition);
        Ok(())
    }

    pub fn definition(&self, name: &str) -> Option<&EventDefinition> {
        self.definitions.get(name)
    }

    pub fn validate(&self, event: &Event, limits: &Limits) -> Result<(), SchemaError> {
        validate_envelope(event, limits)?;
        let definition = self
            .definition(&event.event_name)
            .ok_or(SchemaError("unregistered event"))?;
        if event.category != definition.category {
            return Err(SchemaError("event category mismatch"));
        }
        for (key, value) in &event.properties {
            let rule = definition
                .properties
                .get(key)
                .ok_or(SchemaError("property not allowlisted"))?;
            let valid = match rule {
                PropertyRule::Identifier => value.as_str().is_some_and(|s| validate_id(s).is_ok()),
                PropertyRule::Number => value.as_f64().is_some_and(f64::is_finite),
                PropertyRule::Boolean => value.is_boolean(),
                PropertyRule::Enum(allowed) => value
                    .as_str()
                    .is_some_and(|s| allowed.iter().any(|a| a == s)),
                PropertyRule::RedactedText => value.as_str() == Some(REDACTED),
            };
            if !valid {
                return Err(SchemaError("property value violates schema"));
            }
        }
        Ok(())
    }
}

/// Basic envelope checks, also used when inspecting persisted data. This does not
/// replace registry validation: an unregistered event must not be exported.
pub fn validate_envelope(event: &Event, limits: &Limits) -> Result<(), SchemaError> {
    limits.validate()?;
    if event.schema_version != SCHEMA_VERSION {
        return Err(SchemaError("unsupported schema version"));
    }
    if !is_random_id(&event.event_id) {
        return Err(SchemaError("invalid event id"));
    }
    validate_id(&event.app_id)?;
    validate_id(&event.event_name)?;
    validate_id(&event.platform)?;
    for metadata in [&event.app_version, &event.sdk_version] {
        if metadata.is_empty()
            || metadata.len() > limits.max_string_bytes
            || !metadata
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'))
        {
            return Err(SchemaError("invalid version metadata"));
        }
    }
    if event.category != Category::UsageSession
        && (event.session_id.is_some() || event.workflow_id.is_some())
    {
        return Err(SchemaError(
            "session linkage requires usage_session category",
        ));
    }
    for id in [&event.session_id, &event.workflow_id]
        .into_iter()
        .flatten()
    {
        if !is_random_id(id) {
            return Err(SchemaError("invalid ephemeral id"));
        }
    }
    if event.properties.len() > limits.max_properties {
        return Err(SchemaError("too many properties"));
    }
    for (key, value) in &event.properties {
        validate_id(key)?;
        if value
            .as_str()
            .is_some_and(|s| s.len() > limits.max_string_bytes)
            || value.is_array()
            || value.is_object()
            || value.is_null()
        {
            return Err(SchemaError("invalid property size or type"));
        }
    }
    if serde_json::to_vec(event)
        .map_err(|_| SchemaError("cannot encode event"))?
        .len()
        > limits.max_event_bytes
    {
        return Err(SchemaError("event byte budget exceeded"));
    }
    Ok(())
}

fn is_random_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| {
        id.get_version() == Some(uuid::Version::Random)
            && id.get_variant() == uuid::Variant::RFC4122
            && id.hyphenated().to_string().eq_ignore_ascii_case(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Event {
        Event {
            schema_version: 1,
            event_id: new_id(),
            app_id: "sample".into(),
            app_version: "1.0".into(),
            platform: "linux".into(),
            sdk_version: "0.1.0".into(),
            category: Category::UsageAggregate,
            event_name: "action".into(),
            occurred_at: 1,
            properties: BTreeMap::from([
                ("id".into(), Value::from("search.submit")),
                ("result".into(), Value::from("success")),
            ]),
            session_id: None,
            workflow_id: None,
        }
    }
    #[test]
    fn strict_registry_rejects_unknown_and_sensitive_fields() {
        let registry = Registry::default();
        let mut event = event();
        assert!(registry.validate(&event, &Limits::default()).is_ok());
        event
            .properties
            .insert("document_path".into(), Value::from("/private/document"));
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event.properties.remove("document_path");
        event
            .properties
            .insert("id".into(), Value::from("https://example.org/?secret=abc"));
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event.event_name = "unknown".into();
        assert!(registry.validate(&event, &Limits::default()).is_err());
    }
    #[test]
    fn metadata_and_bytes_are_bounded() {
        let registry = Registry::default();
        let mut event = event();
        event.category = Category::Diagnostics;
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event = self::event();
        event.schema_version = 2;
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event = self::event();
        event.app_version = "secret@example.org".into();
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event = self::event();
        let limits = Limits {
            max_event_bytes: 256,
            ..Limits::default()
        };
        assert!(registry.validate(&event, &limits).is_err());
    }
    #[test]
    fn registered_definitions_cannot_replace_builtins() {
        let mut registry = Registry::default();
        let action = registry.definition("action").unwrap().clone();
        assert!(registry.register(action).is_err());
        assert!(registry
            .register(EventDefinition {
                name: "custom.counter".into(),
                category: Category::Performance,
                properties: BTreeMap::from([("count".into(), PropertyRule::Number)])
            })
            .is_ok());
    }
    #[test]
    fn raw_error_messages_cannot_pass_validation() {
        let mut event = event();
        event.category = Category::Diagnostics;
        event.event_name = "error".into();
        event.properties = BTreeMap::from([
            ("code".into(), Value::from("io.failure")),
            ("message".into(), Value::from("secret")),
        ]);
        assert!(Registry::default()
            .validate(&event, &Limits::default())
            .is_err());
        event
            .properties
            .insert("message".into(), Value::from(REDACTED));
        assert!(Registry::default()
            .validate(&event, &Limits::default())
            .is_ok());
    }
    #[test]
    fn envelopes_deny_unknown_fields() {
        let mut value = serde_json::to_value(event()).unwrap();
        value["device_id"] = Value::from("forbidden");
        assert!(serde_json::from_value::<Event>(value).is_err());
    }

    #[test]
    fn canonical_fixture_round_trips_without_semantic_changes() {
        let fixture: Value =
            serde_json::from_str(include_str!("../schema/action-v1.json")).unwrap();
        let event: Event = serde_json::from_value(fixture.clone()).unwrap();
        Registry::default()
            .validate(&event, &Limits::default())
            .unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), fixture);
    }

    #[test]
    fn device_derived_uuid_versions_are_rejected() {
        let mut event = event();
        event.event_id = "550e8400-e29b-11d4-a716-446655440000".into();
        assert!(Registry::default()
            .validate(&event, &Limits::default())
            .is_err());
    }

    #[test]
    fn optional_session_linkage_requires_session_category() {
        let registry = Registry::default();
        let mut event = event();
        for category in Category::ALL
            .into_iter()
            .filter(|c| *c != Category::UsageSession)
        {
            event.category = category;
            event.session_id = Some(new_id());
            event.workflow_id = None;
            assert!(validate_envelope(&event, &Limits::default()).is_err());
            event.session_id = None;
            event.workflow_id = Some(new_id());
            assert!(validate_envelope(&event, &Limits::default()).is_err());
        }
        event.category = Category::Diagnostics;
        event.event_name = "error".into();
        event.properties = Properties::from([("code".into(), "io.failure".into())]);
        assert!(registry.validate(&event, &Limits::default()).is_err());
        event.category = Category::UsageSession;
        event.event_name = "workflow.start".into();
        event.properties = Properties::from([("name".into(), "document.import".into())]);
        event.session_id = Some(new_id());
        assert!(registry.validate(&event, &Limits::default()).is_ok());
    }
}
