use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;

pub const SYNC_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dot {
    pub actor_id: String,
    pub counter: u64,
}

impl Ord for Dot {
    fn cmp(&self, other: &Self) -> Ordering {
        self.counter
            .cmp(&other.counter)
            .then_with(|| self.actor_id.cmp(&other.actor_id))
    }
}

impl PartialOrd for Dot {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A compact causal context. Only the greatest observed counter per actor is
/// transmitted, making delta exchange proportional to the number of devices.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionVector(pub BTreeMap<String, u64>);

impl VersionVector {
    pub fn observes(&self, dot: &Dot) -> bool {
        self.0.get(&dot.actor_id).copied().unwrap_or(0) >= dot.counter
    }

    pub fn observe(&mut self, dot: &Dot) {
        self.0
            .entry(dot.actor_id.clone())
            .and_modify(|counter| *counter = (*counter).max(dot.counter))
            .or_insert(dot.counter);
    }

    pub fn merge(&mut self, other: &Self) {
        for (actor, counter) in &other.0 {
            self.0
                .entry(actor.clone())
                .and_modify(|current| *current = (*current).max(*counter))
                .or_insert(*counter);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentVersion {
    pub dot: Dot,
    pub context: VersionVector,
    pub value: String,
}

/// A multi-value register for item content. Concurrent edits are retained as
/// separate values and surfaced as a conflict instead of silently overwriting
/// either writer. A later resolution observes all variants and replaces them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentRegister {
    pub versions: Vec<ContentVersion>,
}

impl ContentRegister {
    pub fn apply(&mut self, incoming: ContentVersion) {
        if self
            .versions
            .iter()
            .any(|existing| existing.dot == incoming.dot)
        {
            return;
        }

        // The incoming assignment is obsolete if an existing assignment was
        // authored after observing it.
        if self
            .versions
            .iter()
            .any(|existing| existing.context.observes(&incoming.dot))
        {
            return;
        }

        // Assignments observed by the incoming edit are causally superseded.
        self.versions
            .retain(|existing| !incoming.context.observes(&existing.dot));
        self.versions.push(incoming);
        self.versions.sort_by(|a, b| a.dot.cmp(&b.dot));
    }

    pub fn has_conflict(&self) -> bool {
        self.versions.len() > 1
    }

    /// Stable projection for clients that cannot show a conflict UI yet. The
    /// complete set remains persisted and available for explicit resolution.
    pub fn projected_value(&self) -> Option<&str> {
        self.versions.last().map(|version| version.value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LwwValue {
    pub stamp: Dot,
    pub value: Value,
}

impl LwwValue {
    pub fn merge(&mut self, incoming: Self) {
        if incoming.stamp > self.stamp {
            *self = incoming;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntityKind {
    Section,
    Item,
    Attachment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Mutation {
    SetMetadata {
        field: String,
        value: Value,
    },
    SetContent {
        context: VersionVector,
        value: String,
    },
    Delete,
    ResolveContent {
        context: VersionVector,
        value: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub schema_version: u16,
    pub workspace_id: String,
    pub entity_kind: EntityKind,
    pub entity_id: String,
    pub dot: Dot,
    pub mutation: Mutation,
}

impl Operation {
    pub fn id(&self) -> String {
        format!("{}:{}", self.dot.actor_id, self.dot.counter)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncPayload {
    pub schema_version: u16,
    pub workspace_id: String,
    pub frontier: VersionVector,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityState {
    pub metadata: BTreeMap<String, LwwValue>,
    pub content: ContentRegister,
    pub deleted_at: Option<Dot>,
}

impl EntityState {
    pub fn apply(&mut self, operation: &Operation) {
        // Entity IDs are never reused. Tombstones are permanent until a future
        // explicit Undelete mutation is added to the protocol.
        if self.deleted_at.is_some() {
            return;
        }
        match &operation.mutation {
            Mutation::SetMetadata { field, value } => {
                let incoming = LwwValue {
                    stamp: operation.dot.clone(),
                    value: value.clone(),
                };
                match self.metadata.get_mut(field) {
                    Some(existing) => existing.merge(incoming),
                    None => {
                        self.metadata.insert(field.clone(), incoming);
                    }
                }
            }
            Mutation::SetContent { context, value }
            | Mutation::ResolveContent { context, value } => {
                self.content.apply(ContentVersion {
                    dot: operation.dot.clone(),
                    context: context.clone(),
                    value: value.clone(),
                });
            }
            Mutation::Delete => {
                self.deleted_at = Some(operation.dot.clone());
            }
        }
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    pub fn value(&self, field: &str) -> Option<&Value> {
        self.metadata.get(field).map(|value| &value.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(actor: &str, counter: u64, context: &[(&str, u64)], value: &str) -> ContentVersion {
        ContentVersion {
            dot: Dot {
                actor_id: actor.into(),
                counter,
            },
            context: VersionVector(
                context
                    .iter()
                    .map(|(actor, counter)| ((*actor).into(), *counter))
                    .collect(),
            ),
            value: value.into(),
        }
    }

    #[test]
    fn concurrent_content_edits_are_retained() {
        let mut register = ContentRegister::default();
        register.apply(version("mac", 1, &[], "from mac"));
        register.apply(version("phone", 1, &[], "from phone"));

        assert!(register.has_conflict());
        assert_eq!(register.versions.len(), 2);
        assert!(register.versions.iter().any(|v| v.value == "from mac"));
        assert!(register.versions.iter().any(|v| v.value == "from phone"));
    }

    #[test]
    fn resolution_supersedes_every_observed_variant() {
        let mut register = ContentRegister::default();
        register.apply(version("mac", 1, &[], "from mac"));
        register.apply(version("phone", 1, &[], "from phone"));
        register.apply(version("phone", 2, &[("mac", 1), ("phone", 1)], "combined"));

        assert!(!register.has_conflict());
        assert_eq!(register.projected_value(), Some("combined"));
    }

    #[test]
    fn merges_are_idempotent_and_order_independent() {
        let mac = version("mac", 2, &[], "M");
        let phone = version("phone", 3, &[], "P");
        let mut left = ContentRegister::default();
        left.apply(mac.clone());
        left.apply(phone.clone());
        left.apply(phone.clone());
        let mut right = ContentRegister::default();
        right.apply(phone);
        right.apply(mac);

        assert_eq!(left, right);
    }
}
