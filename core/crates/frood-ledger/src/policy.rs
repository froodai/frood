//! Auto-verdict policies (docs/11 §8). Every policy verdict is recorded with the policy's name.

use crate::model::{Actor, LinkKind, Operation, Proposal, Risk};

#[derive(Debug, Clone, PartialEq)]
pub struct PolicyConfig {
    /// A user's own new facts without conflicts are accepted automatically.
    pub auto_accept_own_facts: bool,
    /// A user's change of knowledge only they wrote is accepted automatically (docs/27 §4).
    pub auto_accept_own_changes: bool,
    /// New knowledge agents find in ordinary sources is accepted, marked not verified
    /// (docs/27 §6); changes and conflicts still wait for a person.
    pub accept_agent_findings: bool,
    /// Entity merges at or above this confidence are accepted automatically.
    pub entity_merge_threshold: Option<f32>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            auto_accept_own_facts: true,
            auto_accept_own_changes: true,
            accept_agent_findings: true,
            entity_merge_threshold: Some(0.9),
        }
    }
}

/// What the policies may know about where a proposal comes from, beyond its author.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProposalContext {
    /// The proposal comes from an agent run of a job with `auto_accept`, which reads one
    /// source marked `authoritative` (W4′).
    pub authoritative_source: Option<String>,
    /// Of the assertions the proposal changes, those only its author ever wrote — their notes,
    /// imports and edits — and that nothing contests. The ledger fills it in.
    pub own_assertions: Vec<uuid::Uuid>,
    /// It comes from imported memory (an assistant's retelling, docs/27 §5): what is accepted
    /// is marked not verified with this reason, and its changes to knowledge wait for a person
    /// — that is where memories disagree.
    pub mark_unverified: Option<String>,
}

/// Which policy, if any, accepts the proposal, and which alternative it takes.
pub(crate) fn evaluate(
    proposal: &Proposal,
    config: &PolicyConfig,
    context: &ProposalContext,
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

    // A person moving knowledge to another sphere changes a label, not what is known
    // (docs/26 §3): accepted at once.
    if author_is_user
        && !only.operations.is_empty()
        && only
            .operations
            .iter()
            .all(|op| matches!(op, Operation::SetSphere { .. }))
    {
        return Some(("own-sphere", only.key.clone()));
    }

    // A person confirming knowledge is right (it was marked not verified): their own check.
    if author_is_user
        && !only.operations.is_empty()
        && only
            .operations
            .iter()
            .all(|op| matches!(op, Operation::Reverify { .. }))
    {
        return Some(("own-check", only.key.clone()));
    }

    // A person changing what only they wrote (docs/27 §4): "moved to Postgres 17" over their own
    // "Postgres 16". Knowledge from anyone or anything else still changes by a verdict.
    let own = |id: &uuid::Uuid| context.own_assertions.contains(id);
    if config.auto_accept_own_changes
        && context.mark_unverified.is_none()
        && author_is_user
        && matches!(proposal.risk, Risk::New | Risk::Change)
        && only
            .operations
            .iter()
            .any(|op| matches!(op, Operation::Supersede { .. } | Operation::Close { .. }))
        && only.operations.iter().all(|op| match op {
            Operation::Create { .. } | Operation::CreateEntity { .. } => true,
            Operation::Link { kind, .. } => *kind != LinkKind::Contradicts,
            Operation::Supersede { assertion_id, .. }
            | Operation::Close { assertion_id, .. }
            | Operation::Reverify { assertion_id, .. }
            | Operation::SetSphere { assertion_id, .. } => own(assertion_id),
            _ => false,
        })
    {
        return Some(("own-change", only.key.clone()));
    }

    // W4′: an agent reading an authoritative source may record new knowledge and changes;
    // contradictions always wait for a person.
    if context.authoritative_source.is_some()
        && proposal.author.starts_with("agent:")
        && matches!(proposal.risk, Risk::New | Risk::Change)
        && only.operations.iter().all(|op| match op {
            Operation::Link { kind, .. } => *kind != LinkKind::Contradicts,
            Operation::MergeEntities { .. } => false,
            _ => true,
        })
    {
        return Some(("authoritative-source", only.key.clone()));
    }

    // QF3: new knowledge an agent found in an ordinary source is taken at once and marked not
    // verified; changes, conflicts and merges wait for a person.
    if config.accept_agent_findings
        && proposal.author.starts_with("agent:")
        && proposal.risk == Risk::New
        && proposal.base.is_empty()
        && only.operations.iter().all(|op| match op {
            Operation::Create { .. } | Operation::CreateEntity { .. } => true,
            Operation::Link { kind, .. } => *kind != LinkKind::Contradicts,
            _ => false,
        })
    {
        return Some(("agent-finding", only.key.clone()));
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

    fn none() -> ProposalContext {
        ProposalContext::default()
    }

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
                title: None,
                sphere: None,
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
            evaluate(
                &proposal("user:alice", Risk::New, vec![create()]),
                &config,
                &none()
            )
            .map(|p| p.0),
            Some("own-facts")
        );
    }

    #[test]
    fn agents_new_findings_are_taken_unverified_their_changes_wait() {
        let config = PolicyConfig::default();
        assert_eq!(
            evaluate(
                &proposal("agent:daily-sweep", Risk::New, vec![create()]),
                &config,
                &none()
            )
            .map(|p| p.0),
            Some("agent-finding")
        );
        let id = Uuid::now_v7();
        let mut change = proposal("agent:daily-sweep", Risk::Change, vec![supersede(id)]);
        change.base.push(BaseRef {
            assertion_id: id,
            version: 1,
        });
        assert!(evaluate(&change, &config, &none()).is_none());
        let off = PolicyConfig {
            accept_agent_findings: false,
            ..PolicyConfig::default()
        };
        assert!(
            evaluate(
                &proposal("agent:daily-sweep", Risk::New, vec![create()]),
                &off,
                &none()
            )
            .is_none()
        );
    }

    fn supersede(assertion_id: Uuid) -> Operation {
        let Operation::Create { assertion, .. } = create() else {
            unreachable!()
        };
        Operation::Supersede {
            assertion_id,
            assertion,
        }
    }

    #[test]
    fn a_persons_change_of_their_own_knowledge_is_accepted() {
        let config = PolicyConfig::default();
        let (mine, theirs) = (Uuid::now_v7(), Uuid::now_v7());
        let context = ProposalContext {
            own_assertions: vec![mine],
            ..ProposalContext::default()
        };
        let change = |id: Uuid, risk| {
            let mut p = proposal("user:alice/via/extract", risk, vec![supersede(id)]);
            p.base.push(BaseRef {
                assertion_id: id,
                version: 1,
            });
            p
        };
        assert_eq!(
            evaluate(&change(mine, Risk::Change), &config, &context).map(|p| p.0),
            Some("own-change")
        );
        assert!(evaluate(&change(theirs, Risk::Change), &config, &context).is_none());
        assert!(evaluate(&change(mine, Risk::Conflict), &config, &context).is_none());
        let off = PolicyConfig {
            auto_accept_own_changes: false,
            ..PolicyConfig::default()
        };
        assert!(evaluate(&change(mine, Risk::Change), &off, &context).is_none());
        // Confirming knowledge marked not verified is a person's own check.
        let mut check = proposal(
            "user:alice",
            Risk::Change,
            vec![Operation::Reverify {
                assertion_id: theirs,
                review_after: None,
            }],
        );
        check.base.push(BaseRef {
            assertion_id: theirs,
            version: 1,
        });
        assert_eq!(
            evaluate(&check, &config, &none()).map(|p| p.0),
            Some("own-check")
        );
        check.author = "agent:reverify".into();
        assert!(evaluate(&check, &config, &none()).is_none());
    }

    #[test]
    fn conflicts_and_bases_go_to_review() {
        let config = PolicyConfig::default();
        assert!(
            evaluate(
                &proposal("user:alice", Risk::Conflict, vec![create()]),
                &config,
                &none()
            )
            .is_none()
        );
        let mut with_base = proposal("user:alice", Risk::New, vec![create()]);
        with_base.base.push(BaseRef {
            assertion_id: Uuid::now_v7(),
            version: 1,
        });
        assert!(evaluate(&with_base, &config, &none()).is_none());
        let disabled = PolicyConfig {
            auto_accept_own_facts: false,
            ..PolicyConfig::default()
        };
        assert!(
            evaluate(
                &proposal("user:alice", Risk::New, vec![create()]),
                &disabled,
                &none()
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
        assert_eq!(
            evaluate(&p, &config, &none()).map(|p| p.0),
            Some("entity-merge")
        );
        p.confidence = Some(0.5);
        assert!(evaluate(&p, &config, &none()).is_none());
    }

    #[test]
    fn agents_reading_an_authoritative_source_record_but_never_contradict() {
        let config = PolicyConfig::default();
        let adr = ProposalContext {
            authoritative_source: Some("adr-repo".into()),
            ..ProposalContext::default()
        };
        let agent = |risk| proposal("agent:daily-sweep", risk, vec![create()]);
        assert_eq!(
            evaluate(&agent(Risk::New), &config, &adr).map(|p| p.0),
            Some("authoritative-source")
        );
        assert_eq!(
            evaluate(&agent(Risk::Change), &config, &adr).map(|p| p.0),
            Some("authoritative-source")
        );
        assert!(evaluate(&agent(Risk::Conflict), &config, &adr).is_none());
        assert_eq!(
            evaluate(&agent(Risk::New), &config, &none()).map(|p| p.0),
            Some("agent-finding"),
            "without the authoritative source: taken, but not verified"
        );
        let human = proposal("user:alice", Risk::Change, vec![create()]);
        assert!(
            evaluate(&human, &config, &adr).is_none(),
            "only agents' runs"
        );
    }
}
