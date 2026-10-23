//! Ledger behaviour against a real PostgreSQL. Set FROOD_TEST_DATABASE_URL to an admin URL;
//! each test creates its own database. Without the variable the tests skip themselves.

use chrono::{DateTime, TimeZone, Utc};
use frood_ledger::model::*;
use frood_ledger::{Ledger, LedgerError, PolicyConfig};
use serde_json::json;
use uuid::Uuid;

async fn ledger(name: &str) -> Option<Ledger> {
    let url = frood_store::testing::database_url()?;
    let pool = frood_store::testing::fresh_database(&url, name)
        .await
        .expect("test database");
    Some(Ledger::new(pool, PolicyConfig::default()))
}

macro_rules! ledger_or_skip {
    ($name:literal) => {
        match ledger($name).await {
            Some(ledger) => ledger,
            None => {
                eprintln!("FROOD_TEST_DATABASE_URL is not set; skipping");
                return;
            }
        }
    };
}

fn user() -> Actor {
    Actor::parse("user:alice").unwrap()
}

fn agent() -> Actor {
    Actor::parse("agent:daily-sweep").unwrap()
}

fn day(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
}

async fn capture(ledger: &Ledger, text: &str) -> Uuid {
    let result = ledger
        .capture_source(
            &user(),
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: text.into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    result.source.evidence[0].id
}

fn commitment(text: &str, due: &str, said_at: DateTime<Utc>, evidence: Uuid) -> AssertionContent {
    AssertionContent {
        kind: "commitment".into(),
        text: text.into(),
        subjects: vec![],
        slots: json!({ "due": due }),
        modality: Modality::Agreed,
        authority: json!([{ "who": "Olya" }]),
        said_at: Some(said_at),
        valid_from: None,
        review_after: None,
        evidence: vec![evidence],
    }
}

fn proposal(risk: Risk, base: Vec<BaseRef>, operations: Vec<Operation>) -> NewProposal {
    NewProposal {
        idempotency_key: None,
        rationale: None,
        risk,
        confidence: None,
        base,
        operations: Some(operations),
        alternatives: None,
        evidence: vec![],
    }
}

fn created_id(proposal: &Proposal, journal: &[JournalEvent]) -> Uuid {
    let event = journal
        .iter()
        .rev()
        .find(|e| e.event_type == "assertion.created")
        .unwrap_or_else(|| panic!("no assertion.created for {}", proposal.id));
    Uuid::parse_str(event.data["assertion_id"].as_str().unwrap()).unwrap()
}

fn accept(alternative: Option<&str>) -> VerdictInput {
    VerdictInput {
        decision: Decision::Accept,
        alternative: alternative.map(str::to_owned),
        operations: None,
        reason: None,
        deferred_until: None,
    }
}

#[tokio::test]
async fn capture_is_deduplicated() {
    let ledger = ledger_or_skip!("ledger_capture");
    let input = NewSource {
        kind: "note".into(),
        origin: "capture".into(),
        uri: None,
        title: None,
        content: "release 2.4 on Nov 14".into(),
        said_at: None,
        metadata: json!(null),
    };
    let first = ledger.capture_source(&user(), input.clone()).await.unwrap();
    let second = ledger.capture_source(&user(), input).await.unwrap();
    assert!(first.created && !second.created);
    assert_eq!(first.source.id, second.source.id);
    assert_eq!(first.source.evidence.len(), 1);
    assert_eq!(first.source.evidence[0].anchor, json!({ "type": "whole" }));
}

#[tokio::test]
async fn own_fact_is_accepted_by_policy_but_agent_fact_waits() {
    let ledger = ledger_or_skip!("ledger_policy");
    let evidence = capture(&ledger, "agreed with Olya: release 2.4 on Nov 14").await;

    let own = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Release 2.4 on Nov 14",
                        "2026-11-14",
                        day(2026, 9, 2),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    assert_eq!(own.state, ProposalState::Accepted);
    assert_eq!(own.verdicts[0].decided_by, "policy:own-facts");

    let from_agent = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Docs freeze on Nov 10",
                        "2026-11-10",
                        day(2026, 9, 3),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    assert_eq!(from_agent.state, ProposalState::Open);
    assert!(from_agent.verdicts.is_empty());

    let accepted = ledger
        .submit_verdict(&user(), from_agent.id, accept(None))
        .await
        .unwrap();
    assert_eq!(accepted.state, ProposalState::Accepted);
    assert_eq!(accepted.verdicts[0].decided_by, "user:alice");

    let all = ledger
        .assertions(&AssertionFilter {
            kind: Some("commitment".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
}

/// The worked example of docs/11 §14: the release moves from Nov 14 to Nov 28.
#[tokio::test]
async fn superseding_keeps_both_time_axes() {
    let ledger = ledger_or_skip!("ledger_bitemporal");
    let evidence = capture(&ledger, "release 2.4 on Nov 14").await;
    let first = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Release 2.4 on Nov 14",
                        "2026-11-14",
                        day(2026, 9, 2),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    let journal = ledger.journal(0, 1000).await.unwrap();
    let id = created_id(&first, &journal);
    let before_change = Utc::now();

    let meeting = capture(&ledger, "meeting: moving the release to Nov 28").await;
    let conflict = ledger
        .open_proposal(
            &agent(),
            NewProposal {
                idempotency_key: None,
                rationale: Some("meeting decided to move the release".into()),
                risk: Risk::Conflict,
                confidence: Some(0.8),
                base: vec![BaseRef {
                    assertion_id: id,
                    version: 1,
                }],
                operations: None,
                alternatives: Some(vec![
                    Alternative {
                        key: "move".into(),
                        label: Some("Replace with Nov 28".into()),
                        operations: vec![Operation::Supersede {
                            assertion_id: id,
                            assertion: commitment(
                                "Release 2.4 on Nov 28",
                                "2026-11-28",
                                day(2026, 10, 10),
                                meeting,
                            ),
                        }],
                    },
                    Alternative {
                        key: "keep".into(),
                        label: Some("Keep Nov 14".into()),
                        operations: vec![],
                    },
                ]),
                evidence: vec![meeting],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        conflict.state,
        ProposalState::Open,
        "conflicts are never auto-accepted"
    );
    assert_eq!(
        ledger.assertion(id, None, None).await.unwrap().flags,
        vec![Flag::Contested]
    );

    let missing_choice = ledger
        .submit_verdict(&user(), conflict.id, accept(None))
        .await;
    assert!(matches!(missing_choice, Err(LedgerError::Invalid(_))));
    ledger
        .submit_verdict(&user(), conflict.id, accept(Some("move")))
        .await
        .unwrap();

    // What is true now.
    let now = ledger.assertion(id, None, Some(Utc::now())).await.unwrap();
    assert_eq!(now.version.slots["due"], "2026-11-28");
    assert_eq!(now.version.status, Status::Active);
    assert!(now.flags.is_empty(), "contested is cleared once resolved");

    // What held on Oct 1, by current knowledge.
    let oct1 = ledger
        .assertion(id, None, Some(day(2026, 10, 1)))
        .await
        .unwrap();
    assert_eq!(oct1.version.slots["due"], "2026-11-14");
    assert_eq!(oct1.version.status, Status::Superseded);
    assert_eq!(oct1.version.valid_to, Some(day(2026, 10, 10)));

    // What we believed before the verdict about Oct 11: the change was not known yet.
    let then = ledger
        .assertion(id, Some(before_change), Some(day(2026, 10, 11)))
        .await
        .unwrap();
    assert_eq!(then.version.slots["due"], "2026-11-14");
    assert_eq!(then.version.version, 1);

    let history = ledger.history(id).await.unwrap();
    assert_eq!(history.len(), 3);
    assert!(history[0].recorded_to.is_some());
    assert!(history[1].recorded_to.is_none() && history[2].recorded_to.is_none());
}

#[tokio::test]
async fn stale_base_blocks_the_second_change() {
    let ledger = ledger_or_skip!("ledger_stale");
    let evidence = capture(&ledger, "owner of billing is Petr").await;
    let created = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: AssertionContent {
                        kind: "responsibility".into(),
                        text: "Petr owns billing".into(),
                        subjects: vec![],
                        slots: json!({ "person": "Petr", "area": "billing" }),
                        modality: Modality::Decided,
                        authority: json!([]),
                        said_at: None,
                        valid_from: Some(day(2026, 1, 1)),
                        review_after: None,
                        evidence: vec![evidence],
                    },
                }],
            ),
        )
        .await
        .unwrap();
    let id = created_id(&created, &ledger.journal(0, 1000).await.unwrap());

    let change = |who: &str| {
        proposal(
            Risk::Change,
            vec![BaseRef {
                assertion_id: id,
                version: 1,
            }],
            vec![Operation::Supersede {
                assertion_id: id,
                assertion: AssertionContent {
                    kind: "responsibility".into(),
                    text: format!("{who} owns billing"),
                    subjects: vec![],
                    slots: json!({ "person": who, "area": "billing" }),
                    modality: Modality::Decided,
                    authority: json!([]),
                    said_at: None,
                    valid_from: Some(day(2026, 6, 1)),
                    review_after: None,
                    evidence: vec![evidence],
                },
            }],
        )
    };
    let to_olya = ledger
        .open_proposal(&agent(), change("Olya"))
        .await
        .unwrap();
    let to_ivan = ledger
        .open_proposal(&agent(), change("Ivan"))
        .await
        .unwrap();
    ledger
        .submit_verdict(&user(), to_olya.id, accept(None))
        .await
        .unwrap();

    match ledger
        .submit_verdict(&user(), to_ivan.id, accept(None))
        .await
    {
        Err(LedgerError::Stale {
            proposal,
            assertion,
            expected,
            actual,
        }) => {
            assert_eq!(
                (proposal, assertion, expected, actual),
                (Some(to_ivan.id), id, 1, 3)
            );
        }
        other => panic!("expected Stale, got {other:?}"),
    }
    assert_eq!(
        ledger.proposal(to_ivan.id).await.unwrap().state,
        ProposalState::Stale
    );
    let now = ledger.assertion(id, None, Some(Utc::now())).await.unwrap();
    assert_eq!(now.version.slots["person"], "Olya");

    // Opening a proposal against an old version fails right away.
    assert!(matches!(
        ledger.open_proposal(&agent(), change("Ivan")).await,
        Err(LedgerError::Stale { proposal: None, .. })
    ));
}

#[tokio::test]
async fn idempotency_reject_and_close() {
    let ledger = ledger_or_skip!("ledger_lifecycle");
    let evidence = capture(&ledger, "ship the beta by Oct 30").await;
    let mut input = proposal(
        Risk::New,
        vec![],
        vec![Operation::Create {
            key: None,
            assertion: commitment(
                "Ship the beta by Oct 30",
                "2026-10-30",
                day(2026, 10, 1),
                evidence,
            ),
        }],
    );
    input.idempotency_key = Some("sweep:2026-10-17:beta".into());
    let first = ledger.open_proposal(&agent(), input.clone()).await.unwrap();
    let again = ledger.open_proposal(&agent(), input).await.unwrap();
    assert_eq!(first.id, again.id);

    let rejected = ledger
        .submit_verdict(
            &user(),
            first.id,
            VerdictInput {
                decision: Decision::Reject,
                alternative: None,
                operations: None,
                reason: Some("only discussed, not agreed".into()),
                deferred_until: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(rejected.state, ProposalState::Rejected);
    assert!(matches!(
        ledger.submit_verdict(&user(), first.id, accept(None)).await,
        Err(LedgerError::AlreadyDecided(_, ProposalState::Rejected))
    ));
    assert!(
        ledger
            .assertions(&AssertionFilter::default())
            .await
            .unwrap()
            .is_empty()
    );

    let created = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Ship the beta by Oct 30",
                        "2026-10-30",
                        day(2026, 10, 1),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    let id = created_id(&created, &ledger.journal(0, 1000).await.unwrap());
    let closed = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::Change,
                vec![BaseRef {
                    assertion_id: id,
                    version: 1,
                }],
                vec![Operation::Close {
                    assertion_id: id,
                    status: Status::Completed,
                    valid_to: Some(day(2026, 10, 15)),
                }],
            ),
        )
        .await
        .unwrap();
    ledger
        .submit_verdict(&user(), closed.id, accept(None))
        .await
        .unwrap();

    let active_now = ledger
        .assertions(&AssertionFilter {
            status: Some(Status::Active),
            valid_at: Some(Utc::now()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(active_now.is_empty());
    let latest = ledger.assertion(id, None, None).await.unwrap();
    assert_eq!(latest.version.status, Status::Completed);
    assert_eq!(latest.version.valid_to, Some(day(2026, 10, 15)));
}

#[tokio::test]
async fn slot_filters_answer_deadline_questions() {
    let ledger = ledger_or_skip!("ledger_slots");
    let evidence = capture(&ledger, "deadlines").await;
    for (text, due) in [
        ("A", "2026-10-20"),
        ("B", "2026-10-24"),
        ("C", "2026-11-02"),
    ] {
        ledger
            .open_proposal(
                &user(),
                proposal(
                    Risk::New,
                    vec![],
                    vec![Operation::Create {
                        key: None,
                        assertion: commitment(text, due, day(2026, 10, 1), evidence),
                    }],
                ),
            )
            .await
            .unwrap();
    }
    let this_week = ledger
        .assertions(&AssertionFilter {
            kind: Some("commitment".into()),
            status: Some(Status::Active),
            slots: vec![
                SlotFilter {
                    key: "due".into(),
                    op: SlotOp::Gte,
                    value: "2026-10-19".into(),
                },
                SlotFilter {
                    key: "due".into(),
                    op: SlotOp::Lt,
                    value: "2026-10-26".into(),
                },
            ],
            ..Default::default()
        })
        .await
        .unwrap();
    let texts: Vec<&str> = this_week.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["A", "B"]);

    let bad_key = ledger
        .assertions(&AssertionFilter {
            slots: vec![SlotFilter {
                key: "due'; drop table x".into(),
                op: SlotOp::Eq,
                value: "x".into(),
            }],
            ..Default::default()
        })
        .await;
    assert!(matches!(bad_key, Err(LedgerError::Invalid(_))));
}

#[tokio::test]
async fn entities_refs_and_merges() {
    let ledger = ledger_or_skip!("ledger_entities");
    let evidence = capture(&ledger, "Orion release is owned by Olya").await;
    let created = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![
                    Operation::CreateEntity {
                        key: Some("orion".into()),
                        entity_type: "project".into(),
                        name: "Orion".into(),
                        aliases: vec![],
                    },
                    Operation::Create {
                        key: Some("owner".into()),
                        assertion: AssertionContent {
                            kind: "responsibility".into(),
                            text: "Olya owns the Orion release".into(),
                            subjects: vec!["ref:orion".into()],
                            slots: json!({ "person": "Olya" }),
                            modality: Modality::Decided,
                            authority: json!([]),
                            said_at: None,
                            valid_from: None,
                            review_after: None,
                            evidence: vec![evidence],
                        },
                    },
                ],
            ),
        )
        .await
        .unwrap();
    assert_eq!(created.state, ProposalState::Accepted);
    let orion = ledger
        .entities(Some("ori"), Some("project"), 10)
        .await
        .unwrap()
        .remove(0);
    let view = ledger.entity(orion.id).await.unwrap();
    assert_eq!(view.assertions.len(), 1);

    // A duplicate entity from an agent, merged with high confidence by policy.
    let dup = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::CreateEntity {
                    key: None,
                    entity_type: "project".into(),
                    name: "Project Orion".into(),
                    aliases: vec!["orion".into()],
                }],
            ),
        )
        .await
        .unwrap();
    ledger
        .submit_verdict(&user(), dup.id, accept(None))
        .await
        .unwrap();
    let duplicate = ledger
        .entities(Some("project orion"), None, 10)
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id != orion.id)
        .unwrap();
    let mut merge = proposal(
        Risk::EntityMerge,
        vec![],
        vec![Operation::MergeEntities {
            from: duplicate.id,
            into: orion.id,
        }],
    );
    merge.confidence = Some(0.95);
    let merged = ledger.open_proposal(&agent(), merge).await.unwrap();
    assert_eq!(merged.state, ProposalState::Accepted);
    assert_eq!(merged.verdicts[0].decided_by, "policy:entity-merge");

    // Asking about the merged duplicate still finds Orion's knowledge.
    let via_duplicate = ledger
        .assertions(&AssertionFilter {
            entity: Some(duplicate.id),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(via_duplicate.len(), 1);
    assert_eq!(
        ledger.entity(orion.id).await.unwrap().merged,
        vec![duplicate.id]
    );
}

#[tokio::test]
async fn validation_and_journal_order() {
    let ledger = ledger_or_skip!("ledger_validation");
    let evidence = capture(&ledger, "note").await;
    let unknown_kind = AssertionContent {
        kind: "gossip".into(),
        ..commitment("x", "2026-10-30", day(2026, 10, 1), evidence)
    };
    for bad in [
        proposal(
            Risk::New,
            vec![],
            vec![Operation::Create {
                key: None,
                assertion: unknown_kind,
            }],
        ),
        proposal(
            Risk::New,
            vec![],
            vec![Operation::Create {
                key: None,
                assertion: AssertionContent {
                    evidence: vec![],
                    ..commitment("x", "2026-10-30", day(2026, 10, 1), evidence)
                },
            }],
        ),
        proposal(
            Risk::Change,
            vec![],
            vec![Operation::Supersede {
                assertion_id: Uuid::now_v7(),
                assertion: commitment("x", "2026-10-30", day(2026, 10, 1), evidence),
            }],
        ),
    ] {
        assert!(matches!(
            ledger.open_proposal(&user(), bad).await,
            Err(LedgerError::Invalid(_))
        ));
    }

    let events = ledger.journal(0, 1000).await.unwrap();
    assert!(events.windows(2).all(|w| w[0].position < w[1].position));
    assert_eq!(events[0].event_type, "source.captured");
    let after_first = ledger.journal(events[0].position, 10).await.unwrap();
    assert!(after_first.iter().all(|e| e.position > events[0].position));
}

fn verdict(decision: Decision) -> VerdictInput {
    VerdictInput {
        decision,
        alternative: None,
        operations: None,
        reason: None,
        deferred_until: None,
    }
}

/// A commitment the user stated, and an agent's proposal to move its date.
async fn release_and_a_move(ledger: &Ledger) -> (Uuid, Proposal) {
    let evidence = capture(ledger, "release 2.4 on Nov 14").await;
    let first = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Release 2.4 on Nov 14",
                        "2026-11-14",
                        day(2026, 9, 2),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    let journal = ledger.journal(0, 1000).await.unwrap();
    let id = created_id(&first, &journal);
    let meeting = capture(ledger, "meeting: moving the release to Nov 28").await;
    let change = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::Conflict,
                vec![BaseRef {
                    assertion_id: id,
                    version: 1,
                }],
                vec![Operation::Supersede {
                    assertion_id: id,
                    assertion: commitment(
                        "Release 2.4 on Nov 28",
                        "2026-11-28",
                        day(2026, 10, 10),
                        meeting,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    (id, change)
}

async fn current_text(ledger: &Ledger, id: Uuid) -> Option<String> {
    ledger
        .assertion(id, None, None)
        .await
        .ok()
        .map(|a| a.version.text)
}

#[tokio::test]
async fn an_accepted_change_is_undone_and_can_be_decided_again() {
    let ledger = ledger_or_skip!("ledger_undo_accept");
    let (id, change) = release_and_a_move(&ledger).await;
    ledger
        .submit_verdict(&user(), change.id, accept(None))
        .await
        .unwrap();
    assert_eq!(
        current_text(&ledger, id).await.as_deref(),
        Some("Release 2.4 on Nov 28")
    );

    let undone = ledger.undo(&user(), change.id).await.unwrap();
    assert_eq!(undone.state, ProposalState::Open);
    assert_eq!(
        current_text(&ledger, id).await.as_deref(),
        Some("Release 2.4 on Nov 14")
    );
    let decisions: Vec<Decision> = undone.verdicts.iter().map(|v| v.decision).collect();
    assert_eq!(decisions, [Decision::Accept, Decision::Undo]);
    // History keeps everything; current knowledge is as before, in both time axes.
    let history = ledger.history(id).await.unwrap();
    let current: Vec<&AssertionVersion> =
        history.iter().filter(|v| v.recorded_to.is_none()).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].valid_from, day(2026, 9, 2));
    assert_eq!(current[0].verdict_id, undone.verdicts[1].id);
    assert!(history.len() > 3);
    let journal = ledger.journal(0, 1000).await.unwrap();
    assert!(journal.iter().any(|e| e.event_type == "proposal.reopened"));

    // The base moved to the restored version: deciding again works.
    ledger
        .submit_verdict(&user(), change.id, accept(None))
        .await
        .unwrap();
    assert_eq!(
        current_text(&ledger, id).await.as_deref(),
        Some("Release 2.4 on Nov 28")
    );
    // Undo twice is undo of the newest verdict; with nothing left to undo it says so.
    ledger.undo(&user(), change.id).await.unwrap();
    assert!(matches!(
        ledger.undo(&user(), change.id).await,
        Err(LedgerError::Invalid(_))
    ));
}

#[tokio::test]
async fn rejected_and_deferred_open_again() {
    let ledger = ledger_or_skip!("ledger_undo_reject");
    let (id, change) = release_and_a_move(&ledger).await;
    for decision in [Decision::Reject, Decision::Defer] {
        let mut input = verdict(decision);
        if decision == Decision::Defer {
            input.deferred_until = Some(Utc::now() + chrono::Duration::days(1));
        }
        ledger
            .submit_verdict(&user(), change.id, input)
            .await
            .unwrap();
        let undone = ledger.undo(&user(), change.id).await.unwrap();
        assert_eq!(undone.state, ProposalState::Open, "{decision:?}");
        assert_eq!(undone.deferred_until, None);
    }
    // Still contested: the conflict waits for a person again.
    let view = ledger.assertion(id, None, None).await.unwrap();
    assert!(view.flags.contains(&Flag::Contested), "{:?}", view.flags);
}

#[tokio::test]
async fn undoing_a_creation_takes_it_out_and_a_later_change_blocks_undo() {
    let ledger = ledger_or_skip!("ledger_undo_create");
    let evidence = capture(&ledger, "Olya owns billing").await;
    let created = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Olya ships billing",
                        "2026-12-01",
                        day(2026, 10, 1),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    ledger
        .submit_verdict(&user(), created.id, accept(None))
        .await
        .unwrap();
    let journal = ledger.journal(0, 1000).await.unwrap();
    let id = created_id(&created, &journal);
    ledger.undo(&user(), created.id).await.unwrap();
    assert_eq!(current_text(&ledger, id).await, None);

    // Accepted, then changed by someone else: undoing the first would lose the second.
    let (id, change) = release_and_a_move(&ledger).await;
    ledger
        .submit_verdict(&user(), change.id, accept(None))
        .await
        .unwrap();
    let current = ledger.assertion(id, None, None).await.unwrap().version;
    let later = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::Change,
                vec![BaseRef {
                    assertion_id: id,
                    version: current.version,
                }],
                vec![Operation::Supersede {
                    assertion_id: id,
                    assertion: commitment(
                        "Release 2.4 on Dec 5",
                        "2026-12-05",
                        day(2026, 10, 20),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    ledger
        .submit_verdict(&user(), later.id, accept(None))
        .await
        .unwrap();
    assert!(matches!(
        ledger.undo(&user(), change.id).await,
        Err(LedgerError::Stale { .. })
    ));
    assert_eq!(
        current_text(&ledger, id).await.as_deref(),
        Some("Release 2.4 on Dec 5")
    );
}

#[tokio::test]
async fn a_note_taken_back_takes_what_came_from_it() {
    let ledger = ledger_or_skip!("ledger_withdraw");
    let note = "release 2.4 on Nov 14, Olya owns billing";
    let captured = ledger
        .capture_source(
            &user(),
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: note.into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    let source = captured.source.id;
    let evidence = captured.source.evidence[0].id;
    // The person's own fact is accepted by the policy; an agent's reading waits.
    let own = ledger
        .open_proposal(
            &user(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Release 2.4 on Nov 14",
                        "2026-11-14",
                        day(2026, 10, 1),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    assert_eq!(own.state, ProposalState::Accepted);
    let journal = ledger.journal(0, 1000).await.unwrap();
    let id = created_id(&own, &journal);
    let waiting = ledger
        .open_proposal(
            &agent(),
            proposal(
                Risk::New,
                vec![],
                vec![Operation::Create {
                    key: None,
                    assertion: commitment(
                        "Olya owns billing",
                        "2026-12-01",
                        day(2026, 10, 1),
                        evidence,
                    ),
                }],
            ),
        )
        .await
        .unwrap();
    assert_eq!(waiting.state, ProposalState::Open);

    let done = ledger.withdraw_source(&user(), source).await.unwrap();
    assert_eq!((done.closed, done.kept), (2, 0));
    assert_eq!(
        current_text(&ledger, id).await,
        None,
        "the fact from the note is gone"
    );
    assert_eq!(
        ledger.proposal(waiting.id).await.unwrap().state,
        ProposalState::Rejected
    );
    assert!(ledger.source(source).await.unwrap().withdrawn_at.is_some());
    // Twice is once.
    let again = ledger.withdraw_source(&user(), source).await.unwrap();
    assert_eq!((again.closed, again.kept), (0, 0));

    // The same note sent again comes back, to be processed anew.
    let back = ledger
        .capture_source(
            &user(),
            NewSource {
                kind: "note".into(),
                origin: "capture".into(),
                uri: None,
                title: None,
                content: note.into(),
                said_at: None,
                metadata: json!({}),
            },
        )
        .await
        .unwrap();
    assert!(back.created);
    assert_eq!(back.source.id, source);
    assert!(back.source.withdrawn_at.is_none());
}
