//! Auto-verdict policies (docs/11 §8). Every policy verdict is recorded with the policy's name.

use crate::model::{Actor, LinkKind, Operation, Proposal, Risk};

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyConfig {
    /// A user's own new facts without conflicts are accepted automatically.
    pub auto_accept_own_facts: bool,
    /// Entity merges at or above this confidence are accepted automatically.
    pub entity_merge_threshold: Option<f32>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auto_accept_own_facts: true,
            entity_merge_threshold: Some(0.9),
        }
    }
}

/// Which policy, if any, accepts the proposal, and which alternative it takes.
pub(crate) fn evaluate(
    proposal: &Proposal,
    config: &PolicyConfig,
) -> Option<(&'static str, String)> {
    let [only] = proposal.alternatives.as_slice() else {
        return None;
    };
    let author_is_user = Actor::parse(&proposal.author).is_ok_and(|a| a.is_user());

    if config.auto_accept_own_facts
        && author_is_user
        && proposal.risk == Risk::New
        && proposal.base.is_empty()
        && only.operations.iter().all(|op| match op {
            Operation::Create { .. } | Operation::CreateEntity { .. } => true,
            Operation::Link { kind, .. } => *kind != LinkKind::Contradicts,
            _ => false,
        })
    {
        return Some(("own-facts", only.key.clone()));
    }

    if let Some(threshold) = config.entity_merge_threshold
        && proposal.risk == Risk::EntityMerge
        && proposal.confidence.is_some_and(|c| c >= threshold)
        && only
            .operations
            .iter()
            .all(|op| matches!(op, Operation::MergeEntities { .. }))
    {
        return Some(("entity-merge", only.key.clone()));
    }

    None
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::{Alternative, AssertionContent, BaseRef, Modality, ProposalState};

    fn proposal(author: &str, risk: Risk, operations: Vec<Operation>) -> Proposal {
        Proposal {
            id: Uuid::now_v7(),
            idempotency_key: None,
            author: author.to_owned(),
            rationale: None,
            risk,
            confidence: None,
            base: Vec::new(),
            alternatives: vec![Alternative {
                key: "default".to_owned(),
                label: None,
                operations,
            }],
            evidence: Vec::new(),
            state: ProposalState::Open,
            deferred_until: None,
            created_at: Utc::now(),
            decided_at: None,
            verdicts: Vec::new(),
        }
    }

    fn create() -> Operation {
        Operation::Create {
            key: None,
            assertion: AssertionContent {
                kind: "fact".to_owned(),
                text: "x".to_owned(),
                subjects: Vec::new(),
                slots: serde_json::json!({}),
                modality: Modality::Reported,
                authority: serde_json::json!([]),
                said_at: None,
                valid_from: None,
                review_after: None,
                evidence: vec![Uuid::now_v7()],
            },
        }
    }

    #[test]
    fn users_own_new_facts_are_accepted() {
        let config = PolicyConfig::default();
        assert_eq!(
            evaluate(&proposal("user:alice", Risk::New, vec![create()]), &config).map(|p| p.0),
            Some("own-facts")
        );
    }

    #[test]
    fn agents_conflicts_and_changes_go_to_review() {
        let config = PolicyConfig::default();
        assert!(
            evaluate(
                &proposal("agent:daily-sweep", Risk::New, vec![create()]),
                &config
            )
            .is_none()
        );
        assert!(
            evaluate(
                &proposal("user:alice", Risk::Conflict, vec![create()]),
                &config
            )
            .is_none()
        );
        let mut with_base = proposal("user:alice", Risk::New, vec![create()]);
        with_base.base.push(BaseRef {
            assertion_id: Uuid::now_v7(),
            version: 1,
        });
        assert!(evaluate(&with_base, &config).is_none());
        let disabled = PolicyConfig {
            auto_accept_own_facts: false,
            ..PolicyConfig::default()
        };
        assert!(
            evaluate(
                &proposal("user:alice", Risk::New, vec![create()]),
                &disabled
            )
            .is_none()
        );
    }

    #[test]
    fn confident_entity_merges_are_accepted() {
        let config = PolicyConfig::default();
        let merge = Operation::MergeEntities {
            from: Uuid::now_v7(),
            into: Uuid::now_v7(),
        };
        let mut p = proposal("agent:extract", Risk::EntityMerge, vec![merge]);
        p.confidence = Some(0.95);
        assert_eq!(evaluate(&p, &config).map(|p| p.0), Some("entity-merge"));
        p.confidence = Some(0.5);
        assert!(evaluate(&p, &config).is_none());
    }
}
