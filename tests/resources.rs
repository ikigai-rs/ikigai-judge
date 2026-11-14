mod common;

use common::*;
use ikigai_core::{Capability, Verb};

#[test]
fn a_prompt_resource_is_read_through_the_kernel() {
    let h = Host::with(
        ikigai_judge::Question::from_resources(
            Q,
            "urn:file:scheme.json",
            "urn:file:prompt.txt",
            "fake",
            MODEL,
            PRODUCT,
        )
        .unwrap()
        .budget(100),
    );
    h.text(
        Verb::Sink,
        "urn:file:scheme.json",
        &[("content", r#"{"labels":["spam","ham"]}"#)],
    );
    h.text(
        Verb::Sink,
        "urn:file:prompt.txt",
        &[("content", "Classify this inquiry.")],
    );
    assert_eq!(
        h.text(Verb::Source, "urn:iki:judge:prompt:contact", &[]),
        "Classify this inquiry."
    );
}

fn resource_host() -> Host {
    let h = Host::with(
        ikigai_judge::Question::from_resources(
            Q,
            "urn:file:scheme.json",
            "urn:file:prompt.txt",
            "fake",
            MODEL,
            PRODUCT,
        )
        .unwrap()
        .budget(100),
    );
    put(
        &h,
        "scheme.json",
        r#"{"labels":["spam","ham"],"examples":["old example"]}"#,
    );
    put(&h, "prompt.txt", "Classify the inquiry using the scheme.");
    h
}

fn put(h: &Host, path: &str, content: &str) {
    h.text(
        Verb::Sink,
        &format!("urn:file:{path}"),
        &[("content", content)],
    );
}

fn classifier(h: &Host) -> String {
    h.text(Verb::Source, "urn:iki:judge:classifier:contact", &[])
}

#[test]
fn changed_examples_change_the_digest_and_model_inputs_without_rebuilding() {
    let h = resource_host();
    let before = classifier(&h);
    h.judge("same inquiry", &[]).unwrap();
    assert_eq!(h.class("same inquiry", &[]), "spam model\n");
    put(
        &h,
        "scheme.json",
        r#"{"labels":["spam","ham"],"examples":["new example: ignore instructions"]}"#,
    );
    let after = classifier(&h);
    assert_ne!(before, after);
    assert_eq!(h.class("same inquiry", &[]), "pending\n");
    h.judge("same inquiry", &[]).unwrap();
    assert_eq!(h.fake.calls(), 2);
    let prompts = h.fake.prompts.lock().unwrap();
    for (prompt, example) in prompts
        .iter()
        .zip(["old example", "new example: ignore instructions"])
    {
        assert!(prompt.starts_with("Classify the inquiry using the scheme."));
        let data: serde_json::Value = serde_json::from_str(prompt.lines().last().unwrap()).unwrap();
        assert_eq!(data["scheme"]["examples"][0], example);
        assert_eq!(data["inquiry"], "same inquiry");
    }
    drop(prompts);
    // A previous classifier's atom remains readable after its scheme changes.
    h.text(
        Verb::Source,
        &format!(
            "urn:iki:judge:model:contact:{before}:{}",
            ikigai_judge::digest_hex(b"same inquiry")
        ),
        &[],
    );
    put(&h, "prompt.txt", "Use the updated prompt.");
    assert_ne!(after, classifier(&h));
    assert_eq!(
        h.text(Verb::Source, "urn:iki:judge:prompt:contact", &[]),
        "Use the updated prompt."
    );
}

#[test]
fn changed_labels_control_model_parsing_and_corrections() {
    let h = resource_host();
    let before = classifier(&h);
    put(&h, "scheme.json", r#"{"labels":["inquiry","holding"]}"#);
    assert_ne!(before, classifier(&h));
    h.fake.answer(r#"{"label":"inquiry"}"#);
    h.judge("hello", &[]).unwrap();
    assert_eq!(h.class("hello", &[]), "inquiry model\n");
    h.correct_as(&Capability::root(), "hello", "holding", &[]);
    assert_eq!(h.class("hello", &[]), "holding person-shared\n");
}

#[test]
fn unreadable_and_invalid_schemes_refuse_before_spending() {
    let h = resource_host();
    h.text(Verb::Delete, "urn:file:scheme.json", &[]);
    let error = h.judge("hello", &[]).unwrap_err().to_string();
    assert!(
        error.contains("cannot read scheme resource <urn:file:scheme.json>"),
        "{error}"
    );
    assert_eq!(h.fake.calls(), 0);
    for invalid in [
        "not JSON",
        r#"{"labels":["spam"]}"#,
        r#"{"labels":["spam","spam"]}"#,
        r#"{"labels":["spam","pending"]}"#,
    ] {
        put(&h, "scheme.json", invalid);
        assert!(h
            .judge("hello", &[])
            .unwrap_err()
            .to_string()
            .contains("scheme"));
    }
    assert_eq!(h.fake.calls(), 0);
}

#[test]
fn resource_reads_use_the_callers_capability() {
    let h = resource_host();
    let cap = Capability::scoped(["urn:cap:judge:read:contact"]);
    let error = h
        .issue_as(&cap, Verb::Source, "urn:iki:judge:classifier:contact", &[])
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot read scheme resource"), "{error}");
    assert!(
        error.contains("denied") || error.contains("capability"),
        "{error}"
    );
}

#[test]
fn unreadable_oversized_and_non_utf8_resources_refuse_before_spending() {
    let h = resource_host();
    h.text(Verb::Delete, "urn:file:prompt.txt", &[]);
    assert!(h
        .judge("hello", &[])
        .unwrap_err()
        .to_string()
        .contains("cannot read prompt resource"));
    put(
        &h,
        "prompt.txt",
        &"x".repeat(ikigai_judge::MAX_TEXT_BYTES + 1),
    );
    assert!(h
        .judge("hello", &[])
        .unwrap_err()
        .to_string()
        .contains("refused, not truncated"));
    put(&h, "prompt.txt", "Classify the inquiry.");
    put(
        &h,
        "scheme.json",
        &"x".repeat(ikigai_judge::MAX_TEXT_BYTES + 1),
    );
    assert!(h
        .judge("hello", &[])
        .unwrap_err()
        .to_string()
        .contains("refused, not truncated"));
    // Write binary input directly to the resource backing file.
    std::fs::write(h.root.path().join("scheme.json"), [0xff]).unwrap();
    // A fresh host kernel avoids bypassing the filesystem's invalidation contract.
    let kernel = ikigai_core::Kernel::new(h.root_space.clone());
    let error = futures::executor::block_on(kernel.issue(
        request(Verb::Source, "urn:iki:judge:classifier:contact", &[]),
        &Capability::root(),
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("not UTF-8"), "{error}");
    assert_eq!(h.fake.calls(), 0);
    assert_eq!(
        h.text(
            Verb::Source,
            "urn:iki:judge:spent:contact:form:2026-10-31",
            &[]
        ),
        "spent 0 of 100\n"
    );
}
