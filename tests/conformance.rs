//! The module recipe as one test: `ikigai-conformance` walks every resource the judge binds
//! (its own, `judge-contact-*`; the state root, the bayes model and the fake model endpoint
//! the host composes are inherited and not walked) and reports every violation at once.

mod common;

use common::*;
use ikigai_conformance::{Expected, Fixture, Suite};
use ikigai_core::Verb;

const OURS: [&str; 13] = [
    "question",
    "prompt",
    "classifier",
    "model",
    "person",
    "class",
    "why",
    "pending",
    "disagreements",
    "spent",
    "reservation",
    "judge",
    "correct",
];

fn walk() -> ikigai_conformance::Report {
    let h = Host::new();
    let text = "x";
    let digest = ikigai_judge::digest_hex(text.as_bytes());
    let items = serde_json::json!([{"content": text}]).to_string();
    let mut suite = Suite::new()
        .host_named_space("ikigai_judge::space([question])", h.judge.clone())
        .body_space(h.root_space.clone())
        .required_on_mutating();
    for part in OURS {
        suite = suite.own(format!("judge-{Q}-{part}"));
    }
    for part in ["question", "prompt", "classifier"] {
        suite = suite.pure(format!("judge-{Q}-{part}"));
    }
    for part in [
        "model",
        "person",
        "class",
        "why",
        "pending",
        "disagreements",
        "spent",
        "reservation",
    ] {
        suite = suite.cacheable(format!("judge-{Q}-{part}"));
    }
    // The walk fires a mutating action only through a `content` pipe, which a clear has
    // none of; its outputs and refusals are `tests/retry.rs`.
    suite = suite.opt_out(
        format!("judge-{Q}-reservation"),
        Some(Verb::Delete),
        "a curator's clear takes no content; tests/retry.rs fires it",
    );
    h.judge(text, &[]).unwrap();
    h.correct_as(&ikigai_core::Capability::root(), text, "ham", &[]);
    let classifier = h.text(Verb::Source, &format!("urn:iki:judge:classifier:{Q}"), &[]);
    suite = suite
        .fixture(
            Fixture::new(format!("judge-{Q}-model"), Verb::Source)
                .binding("classifier", &classifier)
                .binding("digest", &digest),
        )
        .fixture(Fixture::new(format!("judge-{Q}-person"), Verb::Source).binding("digest", &digest))
        .fixture(
            Fixture::new(format!("judge-{Q}-class"), Verb::Source)
                .binding("digest", &digest)
                .arg("content", text),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-why"), Verb::Source)
                .binding("digest", &digest)
                .arg("content", text),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-pending"), Verb::Source)
                .binding("source", "form")
                .arg("items", &items),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-disagreements"), Verb::Source).arg("items", &items),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-spent"), Verb::Source)
                .binding("source", "form")
                .binding("date", "2026-10-31"),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-reservation"), Verb::Source)
                .binding("classifier", &classifier)
                .binding("digest", &digest),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-judge"), Verb::Sink)
                .arg("content", text)
                .arg("source", "form"),
        )
        .fixture(
            Fixture::new(format!("judge-{Q}-correct"), Verb::Sink)
                .arg("content", text)
                .arg("label", "ham"),
        );
    suite.run_blocking(&h.kernel)
}

#[test]
fn conforms() {
    let report = walk();
    println!("{report}");
    report.assert_clean_with(&Expected::new().nothing_unprobed());
}

#[test]
fn the_walk_reaches_every_resource_this_crate_binds() {
    let report = walk();
    let mut walked: Vec<&str> = report.walked.iter().map(String::as_str).collect();
    walked.sort_unstable();
    let mut expected: Vec<String> = OURS.iter().map(|p| format!("judge-{Q}-{p}")).collect();
    expected.sort_unstable();
    assert_eq!(walked, expected);
}
