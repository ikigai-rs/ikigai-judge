//! The judge as a host sees it: the ranking (each producer alone and in combination), the
//! disagreement signal and its resolution, stale verdicts after an edit, per-principal
//! corridors, the budget, unparsed and wrong-model answers (a failed call's retry is
//! `tests/retry.rs`). A fake `urn:llm` endpoint, never a
//! real model.

mod common;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use serde_json::json;

const SPAMMY: &str = "cheap pills, buy now at https://pills.example.com/deal";
const HAMMY: &str = "can we meet on tuesday to review the draft";

fn teach_bayes(h: &Host) {
    for n in 0..3 {
        h.train_bayes(&format!("cheap pills buy now deal offer {n}"), "spam");
        h.train_bayes(&format!("meet tuesday review draft agenda {n}"), "ham");
    }
}

// --- each producer alone ------------------------------------------------------------------

#[test]
fn nothing_recorded_is_pending() {
    let h = Host::new();
    assert_eq!(h.class(SPAMMY, &[]), "pending\n");
}

#[test]
fn a_person_alone_decides() {
    let h = Host::new();
    h.correct_as(&Capability::root(), SPAMMY, "ham", &[]);
    assert_eq!(h.class(SPAMMY, &[]), "ham person-shared\n");
}

#[test]
fn a_mapping_alone_decides() {
    let q = question().mapping(std::sync::Arc::new(|source: &str, _item: &str| {
        (source == "newsletter").then(|| "ham".to_string())
    }));
    let h = Host::with(q);
    assert_eq!(
        h.class(SPAMMY, &[("source", "newsletter")]),
        "ham mapping\n"
    );
    assert_eq!(
        h.class(SPAMMY, &[("source", "form")]),
        "pending\n",
        "pure per source"
    );
}

#[test]
fn a_model_alone_decides_and_the_atom_is_an_annotation() {
    let h = Host::new();
    let out = String::from_utf8(
        h.judge(SPAMMY, &[("item", "urn:example:item:1")])
            .unwrap()
            .bytes,
    )
    .unwrap();
    assert_eq!(out, "judged spam model\n");
    assert_eq!(h.class(SPAMMY, &[]), "spam model\n");
    let classifier = h.text(Verb::Source, &format!("urn:iki:judge:classifier:{Q}"), &[]);
    let digest = ikigai_judge::digest_hex(SPAMMY.as_bytes());
    let atom = h.text(
        Verb::Source,
        &format!("urn:iki:judge:model:{Q}:{classifier}:{digest}"),
        &[],
    );
    for want in [
        "a oa:Annotation",
        "oa:motivatedBy oa:classifying",
        "oa:hasTarget <urn:example:item:1>",
        "oa:hasBody <urn:iki:judge:label:contact:spam>",
        &format!("dcterms:identifier \"b3:{digest}\""),
        "dcterms:created \"2026-10-31T10:50:17.123Z\"^^xsd:dateTime",
        "prov:wasAssociatedWith <urn:llm:fake:ask>",
        "prov:wasGeneratedBy <",
        &format!("schema:instrument \"{MODEL}\""),
    ] {
        assert!(atom.contains(want), "{want} in\n{atom}");
    }
}

#[test]
fn a_confident_bayes_alone_decides_an_unsure_one_does_not() {
    let h = Host::new();
    assert_eq!(h.class("nothing alike at all", &[]), "pending\n");
    teach_bayes(&h);
    assert_eq!(h.class("cheap pills buy now", &[]), "spam bayes\n");
    assert_eq!(
        h.class("zebra quantum lattice", &[]),
        "pending\n",
        "unsure answers nothing"
    );
}

// --- in combination -----------------------------------------------------------------------

#[test]
fn person_beats_mapping_beats_model_beats_bayes() {
    let q = question().mapping(std::sync::Arc::new(|source: &str, _: &str| {
        (source == "newsletter").then(|| "spam".to_string())
    }));
    let h = Host::with(q);
    teach_bayes(&h);
    let text = "cheap pills buy now";
    assert_eq!(h.class(text, &[]), "spam bayes\n");
    h.fake.answer("{\"label\": \"ham\"}");
    // The class is not pending (bayes is confident), so the judge spends nothing.
    assert_eq!(
        String::from_utf8(h.judge(text, &[]).unwrap().bytes).unwrap(),
        "unchanged spam bayes\n"
    );
    assert_eq!(h.fake.calls(), 0);

    let h = Host::with(
        question().mapping(std::sync::Arc::new(|source: &str, _: &str| {
            (source == "newsletter").then(|| "spam".to_string())
        })),
    );
    teach_bayes(&h);
    let text = "an unremarkable note";
    h.fake.answer("{\"label\": \"ham\"}");
    h.judge(text, &[]).unwrap();
    assert_eq!(h.class(text, &[]), "ham model\n");
    assert_eq!(h.class(text, &[("source", "newsletter")]), "spam mapping\n");
    h.correct_as(&Capability::root(), text, "ham", &[]);
    assert_eq!(
        h.class(text, &[("source", "newsletter")]),
        "ham person-shared\n"
    );
}

#[test]
fn a_previous_classifiers_verdict_answers_after_the_current_one() {
    let h = Host::new();
    h.judge(HAMMY, &[]).unwrap();
    let old = question();
    // The prompt changes: a new classifier, and the old one listed as previous.
    let mut newer =
        ikigai_judge::Question::new(Q, ["spam", "ham"], "A new prompt.", "fake", MODEL, PRODUCT)
            .unwrap()
            .bayes(Q)
            .budget(100)
            .previous(old.classifier());
    newer = newer.curator("urn:example:curator");
    assert_ne!(newer.classifier().digest(), old.classifier().digest());
    // Same state root, new config.
    let h2 = Host::with(newer);
    copy_dir(h.root.path(), h2.root.path());
    assert_eq!(
        h2.class(HAMMY, &[]),
        "spam model\n",
        "the old verdict stays readable"
    );
    let why = h2.text(Verb::Source, &why_iri(HAMMY), &[("content", HAMMY)]);
    assert!(
        why.contains(&format!("classifier={}", old.classifier().digest())),
        "{why}"
    );
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

// --- the signal and the resolution --------------------------------------------------------

#[test]
fn model_against_confident_bayes_is_a_disagreement_the_model_decides_and_a_correction_resolves() {
    let h = Host::new();
    let text = "unremarkable words first";
    h.fake.answer("{\"label\": \"ham\"}");
    h.judge(text, &[("item", "urn:example:item:7")]).unwrap();
    let items = json!([{"content": text, "item": "urn:example:item:7"}]);
    assert_eq!(h.disagreements(&items), "", "bayes knows nothing yet");

    // Bayes later becomes confident the other way.
    for n in 0..3 {
        h.train_bayes(&format!("unremarkable words first {n}"), "spam");
        h.train_bayes(&format!("tuesday draft review {n}"), "ham");
    }
    assert_eq!(
        h.class(text, &[]),
        "ham model\n",
        "a recorded model verdict outranks bayes"
    );
    let signal = h.disagreements(&items);
    assert!(signal.contains("urn:example:item:7"), "{signal}");
    assert!(signal.contains("deciding model ham"), "{signal}");
    assert!(signal.contains("other bayes spam"), "{signal}");
    assert!(
        signal.contains("modelDigest=b3:"),
        "both answers' provenance: {signal}"
    );

    // The resolution: a curator's correction writes the shared verdict AND trains bayes.
    let before = h.bayes_digest();
    let out = h.correct_as(&h.curator("urn:example:curator"), text, "ham", &[]);
    assert_eq!(out, "corrected ham shared bayes trained\n");
    assert_ne!(h.bayes_digest(), before, "the correction trained bayes");
    assert_eq!(h.disagreements(&items), "", "resolved: it drops out");
    assert_eq!(h.class(text, &[]), "ham person-shared\n");
}

#[test]
fn mapping_against_model_is_a_disagreement() {
    let q = question().mapping(std::sync::Arc::new(|source: &str, _: &str| {
        (source == "newsletter").then(|| "ham".to_string())
    }));
    let h = Host::with(q);
    h.judge("a newsletter issue", &[]).unwrap(); // the fake says spam
    let signal =
        h.disagreements(&json!([{"content": "a newsletter issue", "source": "newsletter"}]));
    assert!(signal.contains("deciding mapping ham"), "{signal}");
    assert!(signal.contains("other model spam"), "{signal}");
}

#[test]
fn disagreements_are_cached_until_a_verdict_or_a_training_changes() {
    let h = Host::new();
    let text = "unremarkable words first";
    h.fake.answer("{\"label\": \"ham\"}");
    h.judge(text, &[]).unwrap();
    let items = json!([{"content": text}]).to_string();
    let probe = request(
        Verb::Source,
        &format!("urn:iki:judge:disagreements:{Q}"),
        &[("items", &items)],
    );
    let root = Capability::root();
    h.disagreements(&json!([{"content": text}]));
    assert!(h.kernel.is_cached(&probe, &root));
    h.train_bayes("something else entirely", "spam");
    assert!(!h.kernel.is_cached(&probe, &root), "a training cuts it");
    h.disagreements(&json!([{"content": text}]));
    assert!(h.kernel.is_cached(&probe, &root));
    h.correct_as(&root, text, "ham", &[]);
    assert!(!h.kernel.is_cached(&probe, &root), "a verdict cuts it");
}

#[test]
fn the_class_is_cached_until_its_atoms_change() {
    let h = Host::new();
    let probe = request(Verb::Source, &class_iri(HAMMY), &[("content", HAMMY)]);
    let root = Capability::root();
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
    assert!(
        h.kernel.is_cached(&probe, &root),
        "pending is cached on the atoms' threads"
    );
    h.judge(HAMMY, &[]).unwrap();
    assert!(
        !h.kernel.is_cached(&probe, &root),
        "writing the model atom cuts it"
    );
    assert_eq!(h.class(HAMMY, &[]), "spam model\n");
}

// --- corridors ----------------------------------------------------------------------------

#[test]
fn two_principals_corrections_do_not_see_each_other_and_a_reader_trains_nothing() {
    let h = Host::new();
    h.judge(HAMMY, &[]).unwrap(); // the model says spam
    let alice = h.reader(ALICE);
    let bob = h.reader(BOB);
    let before = h.bayes_digest();

    assert_eq!(
        h.correct_as(&alice, HAMMY, "ham", &[]),
        "corrected ham personal bayes reader-not-trained\n"
    );
    assert_eq!(
        h.bayes_digest(),
        before,
        "a reader's correction leaves the shared model unchanged"
    );
    assert_eq!(h.class_as(&alice, HAMMY, &[]), "ham person\n");
    assert_eq!(
        h.class_as(&bob, HAMMY, &[]),
        "spam model\n",
        "bob does not see alice's"
    );
    assert_eq!(
        h.class(HAMMY, &[]),
        "spam model\n",
        "nor does the shared view"
    );

    h.correct_as(&bob, HAMMY, "spam", &[]);
    assert_eq!(h.class_as(&alice, HAMMY, &[]), "ham person\n");
    assert_eq!(h.class_as(&bob, HAMMY, &[]), "spam person\n");

    // A reader cannot write the shared corridor even by asking: the path ACL refuses.
    let digest = ikigai_judge::digest_hex(HAMMY.as_bytes());
    let shared = format!(
        "urn:file:person/{}/{Q}/{digest}.ttl",
        ikigai_judge::digest_hex(PRODUCT.as_bytes())
    );
    assert!(matches!(
        h.issue_as(&alice, Verb::Sink, &shared, &[("content", "x")]),
        Err(Error::Denied(_))
    ));

    // A curator's correction is everyone's shared view, under each reader's own.
    h.correct_as(&h.curator(PRODUCT), HAMMY, "ham", &[]);
    assert_eq!(h.class(HAMMY, &[]), "ham person-shared\n");
    assert_eq!(
        h.class_as(&bob, HAMMY, &[]),
        "spam person\n",
        "bob's own still ranks first"
    );
}

// --- stale --------------------------------------------------------------------------------

#[test]
fn an_edit_answers_the_previous_verdict_marked_stale_then_is_re_judged() {
    let h = Host::new();
    let item = ("item", "urn:example:item:42");
    h.judge(HAMMY, &[item]).unwrap();
    let edited = "can we meet on wednesday to review the draft";
    assert_eq!(
        h.class(edited, &[item]),
        format!(
            "spam stale {}\n",
            ikigai_judge::digest_hex(HAMMY.as_bytes())
        )
    );
    let why = h.text(Verb::Source, &why_iri(edited), &[("content", edited), item]);
    assert!(why.contains("stale: the text changed"), "{why}");
    let pending = h.text(
        Verb::Source,
        &format!("urn:iki:judge:pending:{Q}:form"),
        &[(
            "items",
            &json!([{"content": edited, "item": "urn:example:item:42"}]).to_string(),
        )],
    );
    assert!(
        pending.contains(" stale urn:example:item:42"),
        "queued: {pending}"
    );
    assert_eq!(h.fake.calls(), 1, "nothing spent until the judge runs");

    h.fake.answer("{\"label\": \"ham\"}");
    let out = String::from_utf8(h.judge(edited, &[item]).unwrap().bytes).unwrap();
    assert_eq!(out, "judged ham model\n", "a stale class is re-judged");
    assert_eq!(h.fake.calls(), 2);
    assert_eq!(h.class(edited, &[item]), "ham model\n");
}

#[test]
fn a_stale_verdict_against_bayes_on_the_new_text_is_a_disagreement() {
    let h = Host::new();
    let item = ("item", "urn:example:item:9");
    h.fake.answer("{\"label\": \"ham\"}");
    h.judge("first words here", &[item]).unwrap();
    teach_bayes(&h);
    let edited = "cheap pills buy now";
    assert!(h.class(edited, &[item]).starts_with("ham stale "));
    let signal = h.disagreements(&json!([{"content": edited, "item": "urn:example:item:9"}]));
    assert!(signal.contains("deciding stale ham"), "{signal}");
    assert!(signal.contains("other bayes spam"), "{signal}");
}

// --- the judge's refusals -----------------------------------------------------------------

/// A yielding fake holds the first answer in flight while the other judges arrive.
/// Different sources must still share the item's reservation, even with a budget of one.
#[test]
fn concurrent_judges_of_one_item_ask_the_model_once() {
    for answer in [r#"{"label":"spam"}"#, "no label"] {
        let h = Host::with(question().budget(1));
        h.fake.answer(answer);
        let cap = h.worker();
        let iri = format!("urn:iki:judge:judge:{Q}");
        let requests: Vec<_> = (0..8)
            .map(|n| {
                request(
                    Verb::Sink,
                    &iri,
                    &[
                        ("content", "one shared item"),
                        ("source", if n % 2 == 0 { "form" } else { "other" }),
                    ],
                )
            })
            .collect();
        let results = futures::executor::block_on(futures::future::join_all(
            requests.into_iter().map(|r| h.kernel.issue(r, &cap)),
        ));
        assert_eq!(h.fake.calls(), 1, "one paid call for this item");
        for result in results {
            result.unwrap();
        }
        let spent: u32 = ["form", "other"]
            .into_iter()
            .map(|source| {
                h.text(
                    Verb::Source,
                    &format!("urn:iki:judge:spent:{Q}:{source}:2026-10-31"),
                    &[],
                )
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u32>()
                .unwrap()
            })
            .sum();
        assert_eq!(spent, 1);
        h.judge("one shared item", &[]).unwrap();
        assert_eq!(h.fake.calls(), 1, "the recorded verdict is reused");
    }
}

#[test]
fn a_judge_arriving_while_the_model_answers_reuses_its_verdict() {
    let h = Host::new();
    let (release, gate) = futures::channel::oneshot::channel();
    *h.fake.gate.lock().unwrap() = Some(gate);
    let cap = h.worker();
    let iri = format!("urn:iki:judge:judge:{Q}");
    let first = request(
        Verb::Sink,
        &iri,
        &[("content", "in-flight item"), ("source", "form")],
    );
    let later = request(
        Verb::Sink,
        &iri,
        &[("content", "in-flight item"), ("source", "other")],
    );
    futures::executor::block_on(async {
        let first = h.kernel.issue(first, &cap);
        let later = h.kernel.issue(later, &cap);
        futures::pin_mut!(first, later);
        assert!(futures::poll!(&mut first).is_pending());
        assert_eq!(h.fake.calls(), 1, "the first model call has started");
        assert!(futures::poll!(&mut later).is_pending());
        release.send(()).unwrap();
        let (first, later) = futures::join!(first, later);
        assert_eq!(
            String::from_utf8(first.unwrap().bytes).unwrap(),
            "judged spam model\n"
        );
        assert_eq!(
            String::from_utf8(later.unwrap().bytes).unwrap(),
            "unchanged spam model\n"
        );
    });
    assert_eq!(h.fake.calls(), 1);
}

#[test]
fn a_failed_verdict_write_never_pays_for_the_item_again() {
    let h = Host::new();
    // Permit reservations, but refuse the verdict write after the fake model answers.
    let cap = Capability::scoped([
        format!("urn:cap:judge:read:{Q}"),
        format!("urn:cap:judge:judge:{Q}"),
        h.fs("read", ""),
        h.fs("write", &format!("/model/{Q}/spent")),
        h.fs("write", &format!("/model/{Q}/reserved")),
        format!("urn:cap:bayes:read:{Q}"),
    ]);
    let iri = format!("urn:iki:judge:judge:{Q}");
    assert!(h
        .issue_as(
            &cap,
            Verb::Sink,
            &iri,
            &[("content", HAMMY), ("source", "form")]
        )
        .is_err());
    assert_eq!(h.fake.calls(), 1);
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
    // The failure is recorded beside the reservation: refused until its retry time.
    let err = h.judge(HAMMY, &[]).unwrap_err();
    assert!(
        matches!(err, Error::RetryAfter { ref detail, .. } if detail.contains("retry at ")),
        "{err}"
    );

    // A new endpoint instance over the same storage must also honor the saved attempt.
    let root = std::sync::Arc::new(ikigai_core::Fallback::new(vec![
        std::sync::Arc::new(ikigai_judge::space([question()])),
        h.root_space.clone(),
    ]));
    let kernel = ikigai_core::Kernel::with_meta_renderer(
        root,
        std::sync::Arc::new(ikigai_vocab::TurtleRenderer),
    )
    .with_clock(std::sync::Arc::new(ikigai_core::FixedClock::at(NOW)));
    let err = futures::executor::block_on(kernel.issue(
        request(Verb::Sink, &iri, &[("content", HAMMY), ("source", "other")]),
        &h.worker(),
    ))
    .unwrap_err();
    assert!(matches!(err, Error::RetryAfter { .. }), "{err}");
    assert_eq!(h.fake.calls(), 1);
    assert_eq!(
        h.text(
            Verb::Source,
            &format!("urn:iki:judge:spent:{Q}:form:2026-10-31"),
            &[]
        ),
        "spent 1 of 100\n"
    );
    assert_eq!(
        h.text(
            Verb::Source,
            &format!("urn:iki:judge:spent:{Q}:other:2026-10-31"),
            &[]
        ),
        "spent 0 of 100\n"
    );
}

#[test]
fn a_canceled_model_call_is_not_paid_again() {
    let h = Host::new();
    let (_release, gate) = futures::channel::oneshot::channel();
    *h.fake.gate.lock().unwrap() = Some(gate);
    let cap = h.worker();
    let iri = format!("urn:iki:judge:judge:{Q}");
    futures::executor::block_on(async {
        let attempt = h.kernel.issue(
            request(Verb::Sink, &iri, &[("content", HAMMY), ("source", "form")]),
            &cap,
        );
        futures::pin_mut!(attempt);
        assert!(futures::poll!(&mut attempt).is_pending());
        assert_eq!(h.fake.calls(), 1);
        // Leaving this scope drops the request while its model response is outstanding.
    });
    assert!(matches!(h.judge(HAMMY, &[]), Err(Error::Conflict(_))));
    assert_eq!(h.fake.calls(), 1);
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
}

#[test]
fn a_failed_item_reservation_never_calls_the_model() {
    let h = Host::new();
    let classifier = question().classifier().digest();
    let cap = Capability::scoped([
        format!("urn:cap:judge:read:{Q}"),
        format!("urn:cap:judge:judge:{Q}"),
        h.fs("read", ""),
        h.fs("write", &format!("/model/{Q}/spent")),
        h.fs("write", &format!("/model/{Q}/{classifier}")),
        format!("urn:cap:bayes:read:{Q}"),
    ]);
    assert!(h
        .issue_as(
            &cap,
            Verb::Sink,
            &format!("urn:iki:judge:judge:{Q}"),
            &[("content", HAMMY), ("source", "form")]
        )
        .is_err());
    assert_eq!(h.fake.calls(), 0);
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
    // A reservation that was never saved must not strand the item.
    h.judge(HAMMY, &[]).unwrap();
    assert_eq!(h.fake.calls(), 1);
}

#[test]
fn concurrent_judgments_reserve_only_the_available_model_calls() {
    const CALLERS: usize = 8;
    for (budget, initial) in [(0, 0), (1, 0), (3, 0), (3, 1)] {
        let h = Host::with_yielding_spent(question().budget(budget));
        for n in 0..initial {
            h.judge(&format!("earlier item {n}"), &[]).unwrap();
        }
        let cap = h.worker();
        let iri = format!("urn:iki:judge:judge:{Q}");
        let requests: Vec<_> = (0..CALLERS)
            .map(|n| {
                request(
                    Verb::Sink,
                    &iri,
                    &[
                        ("content", &format!("concurrent item {n}")),
                        ("source", "form"),
                    ],
                )
            })
            .collect();
        let results = futures::executor::block_on(futures::future::join_all(
            requests.into_iter().map(|r| h.kernel.issue(r, &cap)),
        ));
        let calls = h.fake.calls();
        let spent = h.text(
            Verb::Source,
            &format!("urn:iki:judge:spent:{Q}:form:2026-10-31"),
            &[],
        );
        assert!(
            calls <= budget as usize,
            "{calls} calls exceeded budget {budget}; {spent}"
        );
        assert_eq!(calls, budget as usize);
        assert_eq!(spent, format!("spent {calls} of {budget}\n"));
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            calls - initial
        );
        for (n, result) in results.into_iter().enumerate() {
            if let Err(err) = result {
                assert!(matches!(err, Error::RetryAfter { .. }), "{err}");
                assert_eq!(h.class(&format!("concurrent item {n}"), &[]), "pending\n");
            }
        }
    }
}

#[test]
fn past_the_budget_the_judge_refuses_until_tomorrow_and_the_item_stays_pending() {
    let h = Host::with(question().budget(1));
    h.judge("first item", &[]).unwrap();
    let err = h.judge("second item", &[]).unwrap_err();
    assert!(
        matches!(err, Error::RetryAfter { delay: Some(delay), .. }
        if delay == std::time::Duration::from_millis(47_382_877)),
        "{err}"
    );
    assert_eq!(h.fake.calls(), 1, "no call past the budget");
    assert_eq!(h.class("second item", &[]), "pending\n");
    let spent = h.text(
        Verb::Source,
        &format!("urn:iki:judge:spent:{Q}:form:2026-10-31"),
        &[],
    );
    assert_eq!(spent, "spent 1 of 1\n");
    // Another source has its own budget.
    let args = [("content", "second item"), ("source", "other")];
    h.issue_as(
        &h.worker(),
        Verb::Sink,
        &format!("urn:iki:judge:judge:{Q}"),
        &args,
    )
    .unwrap();
}

#[test]
fn a_text_past_the_bound_is_refused_not_truncated() {
    let h = Host::new();
    let big = "a".repeat(ikigai_judge::MAX_TEXT_BYTES + 1);
    assert!(
        matches!(h.judge(&big, &[]), Err(Error::InvalidArgument { name, .. }) if name == "content")
    );
    assert_eq!(h.fake.calls(), 0);
}

#[test]
fn an_off_label_answer_is_recorded_unparsed_and_never_retried() {
    let h = Host::new();
    h.fake.answer("I'd say it's probably fine?");
    let out = String::from_utf8(h.judge(HAMMY, &[]).unwrap().bytes).unwrap();
    assert_eq!(out, "unparsed pending model\n");
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
    let why = h.text(Verb::Source, &why_iri(HAMMY), &[("content", HAMMY)]);
    assert!(
        why.contains("unparsed=I'd say it's probably fine?"),
        "{why}"
    );
    for _ in 0..3 {
        let again = String::from_utf8(h.judge(HAMMY, &[]).unwrap().bytes).unwrap();
        assert_eq!(again, "unchanged pending unparsed\n");
    }
    assert_eq!(h.fake.calls(), 1, "spent once, never retried");
    let pending = h.text(
        Verb::Source,
        &format!("urn:iki:judge:pending:{Q}:form"),
        &[("items", &json!([{"content": HAMMY}]).to_string())],
    );
    assert!(pending.contains(" unparsed "), "{pending}");
}

#[test]
fn an_envelope_naming_another_model_is_refused_and_nothing_is_recorded() {
    let h = Host::new();
    *h.fake.model.lock().unwrap() = "some-other-model".into();
    let err = h.judge(HAMMY, &[]).unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
    assert_eq!(h.class(HAMMY, &[]), "pending\n");
    assert!(matches!(h.judge(HAMMY, &[]), Err(Error::RetryAfter { .. })));
    assert_eq!(
        h.fake.calls(),
        1,
        "a refused model answer is still a paid attempt"
    );
    let classifier = h.text(Verb::Source, &format!("urn:iki:judge:classifier:{Q}"), &[]);
    let digest = ikigai_judge::digest_hex(HAMMY.as_bytes());
    assert!(matches!(
        h.issue_as(
            &Capability::root(),
            Verb::Source,
            &format!("urn:iki:judge:model:{Q}:{classifier}:{digest}"),
            &[]
        ),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn the_judge_is_idempotent_and_writes_only_under_model() {
    let h = Host::new();
    h.judge(HAMMY, &[]).unwrap();
    let again = String::from_utf8(h.judge(HAMMY, &[]).unwrap().bytes).unwrap();
    assert_eq!(again, "unchanged spam model\n");
    assert_eq!(h.fake.calls(), 1);
    assert!(h.root.path().join("model").is_dir());
    assert!(!h.root.path().join("person").exists());
}

#[test]
fn a_digest_that_is_not_the_texts_is_refused() {
    let h = Host::new();
    let err = h
        .issue_as(
            &Capability::root(),
            Verb::Source,
            &class_iri("other"),
            &[("content", HAMMY)],
        )
        .unwrap_err();
    assert!(
        matches!(&err, Error::InvalidArgument { name, .. } if name == "content"),
        "{err}"
    );
}

#[test]
fn why_has_text_turtle_and_html_faces() {
    let h = Host::new();
    teach_bayes(&h);
    h.fake.answer("{\"label\": \"ham\"}");
    let text = "cheap words";
    let _ = h.judge(text, &[]);
    let iri = why_iri(text);
    let turtle = h.text(
        Verb::Source,
        &iri,
        &[("content", text), ("as", "text/turtle")],
    );
    assert!(
        turtle.contains(&format!("<{iri}> a prov:Entity")),
        "{turtle}"
    );
    assert!(!turtle.contains("_:"), "skolemized");
    let html = h.text(
        Verb::Source,
        &iri,
        &[("content", text), ("as", "text/html")],
    );
    assert!(html.starts_with("<!doctype html>"), "{html}");
    let plain = h.text(Verb::Source, &iri, &[("content", text)]);
    assert!(plain.starts_with("class "), "{plain}");
}

// --- concurrent corrections ---------------------------------------------------------------

/// The shared verdict's label, and whether bayes holds the same text under it: re-training
/// with the recorded label is `unchanged` only when bayes already agrees.
fn verdict_and_bayes_agree(h: &Host, content: &str) -> (String, bool) {
    let class = h.class(content, &[]);
    let label = class.split_whitespace().next().unwrap().to_string();
    assert_eq!(class, format!("{label} person-shared\n"));
    let trained = h.text(
        Verb::Sink,
        &format!("urn:bayes:{Q}:train"),
        &[("content", content), ("label", &label)],
    );
    (label, trained.starts_with("unchanged "))
}

/// Two curators re-correct one item at once with different labels. The first pauses after
/// its last-digest write (between the atom and the train); the second, naming no item, writes
/// its atom and trains in that gap. The shared verdict and bayes must still name one label.
#[test]
fn concurrent_recorrections_leave_the_verdict_and_bayes_agreeing() {
    let h = Host::with_yielding_person_last(question());
    let content = "a message two curators disagree about";
    let cap = h.curator("urn:example:curator");
    h.correct_as(&cap, content, "spam", &[("item", "urn:example:item:1")]);
    let iri = format!("urn:iki:judge:correct:{Q}");
    let first = request(
        Verb::Sink,
        &iri,
        &[
            ("content", content),
            ("label", "ham"),
            ("item", "urn:example:item:1"),
        ],
    );
    let second = request(Verb::Sink, &iri, &[("content", content), ("label", "spam")]);
    let (a, b) = futures::executor::block_on(futures::future::join(
        h.kernel.issue(first, &cap),
        h.kernel.issue(second, &cap),
    ));
    a.unwrap();
    b.unwrap();
    let (label, agree) = verdict_and_bayes_agree(&h, content);
    assert!(
        agree,
        "the shared verdict is {label}, but bayes holds the other label"
    );
}

/// The same race on real threads, many rounds: no round may leave them disagreeing.
#[test]
fn threaded_recorrections_leave_the_verdict_and_bayes_agreeing() {
    const ROUNDS: usize = 30;
    const CALLERS: usize = 8;
    let mut divergent = 0;
    let errors = std::sync::atomic::AtomicUsize::new(0);
    for round in 0..ROUNDS {
        let h = Host::with_yielding_person_last(question());
        let content = format!("a contested message {round}");
        let cap = h.curator("urn:example:curator");
        h.correct_as(&cap, &content, "spam", &[("item", "urn:example:item:1")]);
        let barrier = std::sync::Barrier::new(CALLERS);
        std::thread::scope(|scope| {
            for n in 0..CALLERS {
                let (h, cap, content, barrier, errors) = (&h, &cap, &content, &barrier, &errors);
                scope.spawn(move || {
                    let label = if n % 2 == 0 { "ham" } else { "spam" };
                    let item: &[(&str, &str)] = if n % 4 < 2 {
                        &[("item", "urn:example:item:1")]
                    } else {
                        &[]
                    };
                    let mut args = vec![("content", content.as_str()), ("label", label)];
                    args.extend_from_slice(item);
                    barrier.wait();
                    let iri = format!("urn:iki:judge:correct:{Q}");
                    if h.issue_as(cap, Verb::Sink, &iri, &args).is_err() {
                        errors.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                });
            }
        });
        if !verdict_and_bayes_agree(&h, &content).1 {
            divergent += 1;
        }
    }
    let errors = errors.into_inner();
    assert_eq!(
        (divergent, errors),
        (0, 0),
        "{divergent} of {ROUNDS} rounds diverged; {errors} corrections failed part-way"
    );
}
