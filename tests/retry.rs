//! A model call that failed: recorded beside its reservation, shown by `why` and `pending`,
//! refused until its retry time, then asked once more; a curator's clear allows a retry at
//! once. A fake `urn:llm` endpoint that fails when told to, never a real model.

mod common;

use std::time::Duration;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use serde_json::json;

const ITEM: &str = "can we meet on tuesday to review the draft";
const HOUR: Duration = Duration::from_secs(3600);

fn reservation_iri(content: &str) -> String {
    format!(
        "urn:iki:judge:reservation:{Q}:{}:{}",
        question().classifier().digest(),
        ikigai_judge::digest_hex(content.as_bytes())
    )
}

fn judged(h: &Host, content: &str) -> String {
    String::from_utf8(h.judge(content, &[]).unwrap().bytes).unwrap()
}

fn pending(h: &Host, content: &str) -> String {
    h.text(
        Verb::Source,
        &format!("urn:iki:judge:pending:{Q}:form"),
        &[("items", &json!([{"content": content}]).to_string())],
    )
}

fn why(h: &Host, content: &str) -> String {
    h.text(Verb::Source, &why_iri(content), &[("content", content)])
}

/// A curator who may clear a reservation: the clear capability and `/model/{q}/reserved`.
fn clearer(h: &Host, principal: &str) -> Capability {
    Capability::scoped([
        format!("urn:cap:judge:read:{Q}"),
        format!("urn:cap:judge:clear:{Q}"),
        h.fs("read", ""),
        h.fs("write", &format!("/model/{Q}/reserved")),
        ikigai_core::principal_scope(principal).unwrap(),
    ])
}

fn timeout() -> Error {
    Error::Timeout("the provider did not answer in 30s".into())
}

#[test]
fn a_failed_call_is_reported_refused_until_its_retry_time_then_judged_once() {
    let h = Host::new();
    h.fake.fail_next(timeout());
    let err = h.judge(ITEM, &[]).unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "{err}");
    assert_eq!(h.fake.calls(), 1);

    // The failure is recorded and said: when, which classifier, the error, whether charged.
    let why = why(&h, ITEM);
    assert!(why.starts_with("class pending\n"), "{why}");
    assert!(
        why.contains(
            "model call failed at 2026-10-31T10:50:17.123Z: timeout: timeout: the provider did \
             not answer in 30s; charged: unknown; retry at 2026-10-31T11:50:17.123Z"
        ),
        "{why}"
    );
    assert!(why.contains(&question().classifier().digest()), "{why}");
    let digest = ikigai_judge::digest_hex(ITEM.as_bytes());
    let classifier = question().classifier().digest();
    let failure = std::fs::read_to_string(h.root.path().join(format!(
        "model/{Q}/reserved/{classifier}/{digest}.failed.ttl"
    )))
    .unwrap();
    assert!(failure.contains("\"timeout\""), "{failure}");
    assert_eq!(
        pending(&h, ITEM),
        format!(
            "{digest} failed urn:iki:judge:text:{digest} model call failed, retry at \
             2026-10-31T11:50:17.123Z\n"
        )
    );

    // Refused before the delay, saying when.
    h.clock.advance(HOUR - Duration::from_secs(60));
    let err = h.judge(ITEM, &[]).unwrap_err();
    assert!(
        matches!(&err, Error::RetryAfter { delay: Some(d), detail }
            if *d == Duration::from_secs(60) && detail.contains("retry at 2026-10-31T11:50:17.123Z")),
        "{err}"
    );
    assert_eq!(h.fake.calls(), 1);

    // Judged once after it.
    h.clock.advance(Duration::from_secs(60));
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(judged(&h, ITEM), "unchanged spam model\n");
    assert_eq!(h.fake.calls(), 2, "exactly two model calls in total");
    assert_eq!(pending(&h, ITEM), "");
    assert!(
        !why_text_mentions_a_call(&h),
        "a verdict ends the call's story"
    );
}

fn why_text_mentions_a_call(h: &Host) -> bool {
    why(h, ITEM).contains("model call")
}

#[test]
fn the_retry_delay_is_set_per_question() {
    let h = Host::with(question().retry_after(Duration::from_secs(90)));
    h.fake.fail_next(timeout());
    h.judge(ITEM, &[]).unwrap_err();
    assert!(why(&h, ITEM).contains("retry at 2026-10-31T10:51:47.123Z"));
    h.clock.advance(Duration::from_secs(89));
    assert!(matches!(h.judge(ITEM, &[]), Err(Error::RetryAfter { .. })));
    h.clock.advance(Duration::from_secs(1));
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(h.fake.calls(), 2);
}

#[test]
fn a_second_failure_waits_again_and_is_recorded_as_the_new_attempt() {
    let h = Host::new();
    h.fake.fail_next(timeout());
    h.fake
        .fail_next(Error::Unavailable("the provider is down".into()));
    h.judge(ITEM, &[]).unwrap_err();
    h.clock.advance(HOUR);
    let err = h.judge(ITEM, &[]).unwrap_err();
    assert!(matches!(err, Error::Unavailable(_)), "{err}");
    let why = why(&h, ITEM);
    assert!(
        why.contains("unavailable: unavailable: the provider is down"),
        "{why}"
    );
    assert!(why.contains("retry at 2026-10-31T12:50:17.123Z"), "{why}");
    assert!(matches!(h.judge(ITEM, &[]), Err(Error::RetryAfter { .. })));
    h.clock.advance(HOUR);
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(h.fake.calls(), 3);
}

#[test]
fn a_curator_clear_allows_an_immediate_retry_and_is_logged() {
    let h = Host::new();
    h.fake.fail_next(timeout());
    h.judge(ITEM, &[]).unwrap_err();
    let iri = reservation_iri(ITEM);
    let state = h.text(Verb::Source, &iri, &[]);
    assert!(state.contains("model call failed at "), "{state}");

    // Not a curator: refused, even holding the clear capability and the path.
    let err = h
        .issue_as(&clearer(&h, ALICE), Verb::Delete, &iri, &[])
        .unwrap_err();
    assert!(matches!(err, Error::Denied(_)), "{err}");
    // Without the clear capability: refused by the floor.
    assert!(h.issue_as(&h.worker(), Verb::Delete, &iri, &[]).is_err());
    assert!(matches!(h.judge(ITEM, &[]), Err(Error::RetryAfter { .. })));

    let curator = "urn:example:curator";
    let out = h.text_as(&clearer(&h, curator), Verb::Delete, &iri, &[]);
    assert_eq!(out, "cleared attempt 1; the judge may ask again\n");
    let why = why(&h, ITEM);
    assert!(
        why.contains("model call cleared by urn:example:curator at 2026-10-31T10:50:17.123Z"),
        "{why}"
    );
    let digest = ikigai_judge::digest_hex(ITEM.as_bytes());
    assert_eq!(
        pending(&h, ITEM),
        format!("{digest} pending urn:iki:judge:text:{digest}\n")
    );
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(h.fake.calls(), 2);

    // The clear is logged, with who, when and the failure it cleared.
    let classifier = question().classifier().digest();
    let log = std::fs::read_to_string(h.root.path().join(format!(
        "model/{Q}/reserved/{classifier}/{digest}.cleared.ttl"
    )))
    .unwrap();
    assert!(log.contains("<urn:example:curator>"), "{log}");
    assert!(log.contains("2026-10-31T10:50:17.123Z"), "{log}");
    assert!(log.contains("timeout"), "{log}");

    // Nothing left to clear once a verdict is recorded.
    let err = h
        .issue_as(&clearer(&h, curator), Verb::Delete, &iri, &[])
        .unwrap_err();
    assert!(matches!(err, Error::Conflict(_)), "{err}");
}

#[test]
fn clearing_an_item_never_reserved_is_not_found() {
    let h = Host::new();
    let err = h
        .issue_as(
            &clearer(&h, PRODUCT),
            Verb::Delete,
            &reservation_iri(ITEM),
            &[],
        )
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
}

#[test]
fn concurrent_judges_after_the_retry_time_still_make_one_call() {
    let h = Host::new();
    h.fake.fail_next(timeout());
    h.judge(ITEM, &[]).unwrap_err();
    h.clock.advance(HOUR);
    let cap = h.worker();
    let iri = format!("urn:iki:judge:judge:{Q}");
    let requests: Vec<_> = (0..8)
        .map(|n| {
            request(
                Verb::Sink,
                &iri,
                &[
                    ("content", ITEM),
                    ("source", if n % 2 == 0 { "form" } else { "other" }),
                ],
            )
        })
        .collect();
    let results = futures::executor::block_on(futures::future::join_all(
        requests.into_iter().map(|r| h.kernel.issue(r, &cap)),
    ));
    let mut judged = 0;
    for result in results {
        let out = String::from_utf8(result.unwrap().bytes).unwrap();
        judged += usize::from(out == "judged spam model\n");
    }
    assert_eq!(judged, 1);
    assert_eq!(h.fake.calls(), 2, "the failed call and one retry");
}

#[test]
fn a_failed_verdict_write_is_recorded_as_charged() {
    let h = Host::new();
    let cap = Capability::scoped([
        format!("urn:cap:judge:read:{Q}"),
        format!("urn:cap:judge:judge:{Q}"),
        h.fs("read", ""),
        h.fs("write", &format!("/model/{Q}/spent")),
        h.fs("write", &format!("/model/{Q}/reserved")),
        format!("urn:cap:bayes:read:{Q}"),
    ]);
    h.issue_as(
        &cap,
        Verb::Sink,
        &format!("urn:iki:judge:judge:{Q}"),
        &[("content", ITEM), ("source", "form")],
    )
    .unwrap_err();
    let why = why(&h, ITEM);
    assert!(why.contains("charged: yes"), "{why}");
    assert!(why.contains(": denied: "), "{why}");
    h.clock.advance(HOUR);
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(h.fake.calls(), 2);
}

#[test]
fn a_call_with_no_recorded_outcome_waits_for_a_curator() {
    let h = Host::new();
    let (_release, gate) = futures::channel::oneshot::channel();
    *h.fake.gate.lock().unwrap() = Some(gate);
    let cap = h.worker();
    futures::executor::block_on(async {
        let attempt = h.kernel.issue(
            request(
                Verb::Sink,
                &format!("urn:iki:judge:judge:{Q}"),
                &[("content", ITEM), ("source", "form")],
            ),
            &cap,
        );
        futures::pin_mut!(attempt);
        assert!(futures::poll!(&mut attempt).is_pending());
        // Dropped while the model call is outstanding: no outcome can be recorded.
    });
    let digest = ikigai_judge::digest_hex(ITEM.as_bytes());
    assert_eq!(
        pending(&h, ITEM),
        format!(
            "{digest} reserved urn:iki:judge:text:{digest} model call reserved, no verdict or \
             failure recorded; a curator can clear it\n"
        )
    );
    // No retry time is ever reached without a recorded failure.
    h.clock.advance(HOUR * 48);
    let err = h.judge(ITEM, &[]).unwrap_err();
    assert!(
        matches!(&err, Error::Conflict(d) if d.contains("a curator can clear it")),
        "{err}"
    );
    h.text_as(
        &clearer(&h, PRODUCT),
        Verb::Delete,
        &reservation_iri(ITEM),
        &[],
    );
    assert_eq!(judged(&h, ITEM), "judged spam model\n");
    assert_eq!(h.fake.calls(), 2);
}

#[test]
fn every_why_face_says_the_call_failed() {
    let h = Host::new();
    h.fake.fail_next(timeout());
    h.judge(ITEM, &[]).unwrap_err();
    let face = |as_: &str| {
        h.text(
            Verb::Source,
            &why_iri(ITEM),
            &[("content", ITEM), ("as", as_)],
        )
    };
    let html = face("text/html");
    assert!(
        html.contains("<p>model call failed at 2026-10-31T10:50:17.123Z: timeout: "),
        "{html}"
    );
    let json: serde_json::Value = serde_json::from_str(&face("application/json")).unwrap();
    let call = &json["modelCall"];
    assert_eq!(call["state"], "failed");
    assert_eq!(call["error"], "timeout");
    assert_eq!(call["charged"], "unknown");
    assert_eq!(call["retryAt"], "2026-10-31T11:50:17.123Z");
    assert_eq!(call["reservation"], reservation_iri(ITEM));
    let turtle = face("text/turtle");
    assert!(
        turtle.contains("schema:actionStatus \"failed\""),
        "{turtle}"
    );
    assert!(!turtle.contains("_:"), "skolemized");
}
