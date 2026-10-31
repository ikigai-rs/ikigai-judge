//! A host for the tests: the judge's space, ikigai-fs `cacheable_space` over a scratch state
//! root, an in-memory ikigai-bayes model, and a FAKE `urn:llm:fake:ask` (never a real model)
//! whose answer and call count the test controls.
#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_bayes::{Bayes, ModelConfig};
use ikigai_core::{
    principal_scope, ArgRef, ArgSpec, Capability, Description, EndpointSpace, Exact, Fallback,
    FixedClock, FnEndpoint, Iri, Kernel, ReprType, Representation, Request, Result, Space, Verb,
};
use ikigai_judge::Question;

pub const Q: &str = "contact";
pub const MODEL: &str = "fake-model-1";
pub const PRODUCT: &str = "urn:example:product";
pub const ALICE: &str = "urn:example:alice";
pub const BOB: &str = "urn:example:bob";
/// 2026-10-31T10:50:17.123Z
pub const NOW: u64 = 1_793_443_817_123;

/// What the fake model answers, and how often it was asked.
#[derive(Default)]
pub struct Fake {
    pub text: Mutex<String>,
    pub model: Mutex<String>,
    pub calls: AtomicUsize,
}

impl Fake {
    pub fn answer(&self, text: &str) {
        *self.text.lock().unwrap() = text.into();
    }
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

pub fn fake_llm(fake: Arc<Fake>) -> FnEndpoint {
    FnEndpoint::new("fake-llm-ask", move |inv| {
        fake.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(inv.inline_str("temperature")?, "0", "temperature 0");
        assert_eq!(
            inv.inline_str("model")?,
            MODEL,
            "the pinned model is asked for"
        );
        assert_eq!(inv.inline_str("as")?, "application/json");
        let body = serde_json::json!({
            "model": *fake.model.lock().unwrap(),
            "text": *fake.text.lock().unwrap(),
            "usage": {"prompt_tokens": 1, "completion_tokens": 1},
        });
        Ok(Representation::new(
            ReprType::new("application/json"),
            body.to_string(),
        ))
    })
    .with_description(
        Description::new("fake-llm-ask")
            .verb(Verb::Source)
            .input(ArgSpec::new("prompt").class("http://www.w3.org/2001/XMLSchema#string"))
            .output("application/json"),
    )
}

pub fn question() -> Question {
    Question::new(
        Q,
        ["spam", "ham"],
        "Is this contact-form message spam?",
        "fake",
        MODEL,
        PRODUCT,
    )
    .unwrap()
    .bayes(Q)
    .budget(100)
    .curator("urn:example:curator")
}

pub struct Host {
    pub kernel: Kernel,
    pub fake: Arc<Fake>,
    pub root: Scratch,
    pub judge: Arc<EndpointSpace>,
    pub root_space: Arc<Fallback>,
}

impl Host {
    pub fn new() -> Host {
        Host::with(question())
    }

    pub fn with(q: Question) -> Host {
        let root = Scratch::new();
        let fake = Arc::new(Fake::default());
        *fake.model.lock().unwrap() = MODEL.into();
        fake.answer("{\"label\": \"spam\"}");
        let bayes = Bayes::in_memory([ModelConfig::new(Q)]).unwrap();
        let judge = Arc::new(ikigai_judge::space([q]));
        let spaces: Vec<Arc<dyn Space>> = vec![
            judge.clone(),
            Arc::new(ikigai_fs::cacheable_space(root.path())),
            Arc::new(ikigai_bayes::space(&bayes)),
            Arc::new(
                EndpointSpace::new().bind(Exact::new("urn:llm:fake:ask"), fake_llm(fake.clone())),
            ),
        ];
        let root_space = Arc::new(Fallback::new(spaces));
        let kernel =
            Kernel::with_meta_renderer(root_space.clone(), Arc::new(ikigai_vocab::TurtleRenderer))
                .with_clock(Arc::new(FixedClock::at(NOW)));
        Host {
            kernel,
            fake,
            root,
            judge,
            root_space,
        }
    }

    pub fn fs(&self, action: &str, sub: &str) -> String {
        format!("urn:cap:fs:{action}:{}{sub}", self.root.path().display())
    }

    /// A reader acting as `principal`: reads everything, writes only its own corridor.
    pub fn reader(&self, principal: &str) -> Capability {
        let key = ikigai_judge::digest_hex(principal.as_bytes());
        Capability::scoped([
            format!("urn:cap:judge:read:{Q}"),
            format!("urn:cap:judge:correct:{Q}"),
            self.fs("read", ""),
            self.fs("write", &format!("/person/{key}")),
            format!("urn:cap:bayes:read:{Q}"),
            // Held, so the test shows the module (not the floor) keeps a reader from training.
            format!("urn:cap:bayes:train:{Q}"),
            principal_scope(principal).unwrap(),
        ])
    }

    /// A curator acting as `principal`: writes the shared corridor and trains bayes.
    pub fn curator(&self, principal: &str) -> Capability {
        let key = ikigai_judge::digest_hex(PRODUCT.as_bytes());
        Capability::scoped([
            format!("urn:cap:judge:read:{Q}"),
            format!("urn:cap:judge:correct:{Q}"),
            self.fs("read", ""),
            self.fs("write", &format!("/person/{key}")),
            format!("urn:cap:bayes:read:{Q}"),
            format!("urn:cap:bayes:train:{Q}"),
            principal_scope(principal).unwrap(),
        ])
    }

    /// The judge's worker: writes `/model` only.
    pub fn worker(&self) -> Capability {
        Capability::scoped([
            format!("urn:cap:judge:read:{Q}"),
            format!("urn:cap:judge:judge:{Q}"),
            self.fs("read", ""),
            self.fs("write", "/model"),
            format!("urn:cap:bayes:read:{Q}"),
        ])
    }

    pub fn issue_as(
        &self,
        cap: &Capability,
        verb: Verb,
        iri: &str,
        args: &[(&str, &str)],
    ) -> Result<Representation> {
        block_on(self.kernel.issue(request(verb, iri, args), cap))
    }

    pub fn text_as(
        &self,
        cap: &Capability,
        verb: Verb,
        iri: &str,
        args: &[(&str, &str)],
    ) -> String {
        let rep = self
            .issue_as(cap, verb, iri, args)
            .unwrap_or_else(|e| panic!("{verb:?} {iri}: {e}"));
        String::from_utf8(rep.bytes).unwrap()
    }

    pub fn text(&self, verb: Verb, iri: &str, args: &[(&str, &str)]) -> String {
        self.text_as(&Capability::root(), verb, iri, args)
    }

    pub fn class_as(&self, cap: &Capability, content: &str, extra: &[(&str, &str)]) -> String {
        let mut args = vec![("content", content)];
        args.extend_from_slice(extra);
        self.text_as(cap, Verb::Source, &class_iri(content), &args)
    }

    pub fn class(&self, content: &str, extra: &[(&str, &str)]) -> String {
        self.class_as(&Capability::root(), content, extra)
    }

    pub fn judge(&self, content: &str, extra: &[(&str, &str)]) -> Result<Representation> {
        let mut args = vec![("content", content), ("source", "form")];
        args.extend_from_slice(extra);
        self.issue_as(
            &self.worker(),
            Verb::Sink,
            &format!("urn:iki:judge:judge:{Q}"),
            &args,
        )
    }

    pub fn correct_as(
        &self,
        cap: &Capability,
        content: &str,
        label: &str,
        extra: &[(&str, &str)],
    ) -> String {
        let mut args = vec![("content", content), ("label", label)];
        args.extend_from_slice(extra);
        self.text_as(
            cap,
            Verb::Sink,
            &format!("urn:iki:judge:correct:{Q}"),
            &args,
        )
    }

    pub fn train_bayes(&self, content: &str, label: &str) {
        self.text(
            Verb::Sink,
            &format!("urn:bayes:{Q}:train"),
            &[("content", content), ("label", label)],
        );
    }

    pub fn bayes_digest(&self) -> String {
        let stats = self.text(Verb::Source, &format!("urn:bayes:{Q}:stats"), &[]);
        stats
            .lines()
            .find_map(|l| l.strip_prefix("modelDigest "))
            .unwrap()
            .to_string()
    }

    pub fn disagreements(&self, items: &serde_json::Value) -> String {
        let items = items.to_string();
        self.text(
            Verb::Source,
            &format!("urn:iki:judge:disagreements:{Q}"),
            &[("items", &items)],
        )
    }
}

pub fn class_iri(content: &str) -> String {
    format!(
        "urn:iki:judge:class:{Q}:{}",
        ikigai_judge::digest_hex(content.as_bytes())
    )
}

pub fn why_iri(content: &str) -> String {
    format!(
        "urn:iki:judge:why:{Q}:{}",
        ikigai_judge::digest_hex(content.as_bytes())
    )
}

pub fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

/// A fresh scratch directory under the system temp dir, removed on drop.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ikigai-judge-{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(std::fs::canonicalize(dir).unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
