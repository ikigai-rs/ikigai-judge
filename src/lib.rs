//! `ikigai-judge`: judgments as resources.
//!
//! An item's class for a question is the first recorded or computed verdict that answers, in
//! order person ▸ mapping ▸ model ▸ stale (the item's previous text's recorded verdict) ▸
//! bayes (when confident) ▸ the value `pending`. Only what cannot be recomputed is stored (a
//! model call, a person's correction), as Turtle ATOMS reached through `urn:file:` (a host
//! mounts ikigai-fs `cacheable_space` over a state root); mapping and bayes are pure, so the
//! compositions over them are cacheable and hang from the atoms' and the bayes model's golden
//! threads. Where two producers disagree, `urn:iki:judge:disagreements:{q}` says so, and a
//! correction resolves it. The design is ledger #2677; the README states every resource.

#![forbid(unsafe_code)]

use std::sync::Arc;

use ikigai_core::ContentId;

mod atoms;
mod endpoints;

pub use endpoints::space;

/// The bound on an item's text: past it every door refuses (`InvalidArgument` on `content`),
/// never truncates. The same as ikigai-bayes's message bound, so a text the judge accepts is
/// one bayes can score.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The bound on the items a `pending` or `disagreements` composition takes in one call.
pub const MAX_ITEMS: usize = 1000;

/// The bound on a model's raw answer recorded in an unparsed verdict.
pub const MAX_RAW_BYTES: usize = 4096;

/// The bare-hex BLAKE3 digest of `bytes`: how a `{digest}` appears inside a resource name
/// (never a `b3:` colon inside a template).
pub fn digest_hex(bytes: &[u8]) -> String {
    bare(&ContentId::of(bytes))
}

fn bare(id: &ContentId) -> String {
    let s = id.to_string();
    s.strip_prefix("b3:").unwrap_or(&s).to_string()
}

/// A name usable as one resource-name segment: lowercase ASCII letters, digits and single
/// hyphens, starting with a letter, at most 40 characters (ikigai-bayes's rule for a label).
pub fn is_segment(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// One label of a question's closed set, as a SKOS concept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    /// The label's name (a segment, see [`is_segment`]).
    pub name: String,
    /// The concept IRI a verdict's `oa:hasBody` names.
    pub concept: String,
}

/// A pure per-source rule: given the item's source and IRI, the label it maps to, if any.
pub type Mapping = Arc<dyn Fn(&str, &str) -> Option<String> + Send + Sync>;

/// A model classifier: what a verdict was produced by. Its [`digest`](Classifier::digest) is
/// the `{classifier}` segment of a model verdict's name, so a new model or a new prompt is a
/// new name and an old verdict stays readable under its old one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classifier {
    /// The `urn:llm:<provider>:ask` backend.
    pub provider: String,
    /// The PINNED model id; an answer naming another is refused.
    pub model: String,
    /// The bare-hex digest of the prompt text.
    pub prompt_digest: String,
    /// The label names, in order.
    pub labels: Vec<String>,
}

impl Classifier {
    /// The bare-hex digest over (provider, model, prompt digest, labels).
    pub fn digest(&self) -> String {
        let mut text = format!(
            "ikigai-judge classifier 1\nprovider {}\nmodel {}\nprompt {}\n",
            self.provider, self.model, self.prompt_digest
        );
        for label in &self.labels {
            text.push_str(&format!("label {label}\n"));
        }
        digest_hex(text.as_bytes())
    }
}

/// A question: its closed label set, its prompt and pinned model, its bayes model, its budget
/// and who may correct the shared view.
#[derive(Clone)]
pub struct Question {
    pub(crate) name: String,
    pub(crate) labels: Vec<Label>,
    pub(crate) prompt: String,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) bayes: Option<String>,
    pub(crate) budget: u32,
    pub(crate) previous: Vec<Classifier>,
    pub(crate) mapping: Option<Mapping>,
    pub(crate) product: String,
    pub(crate) curators: Vec<String>,
}

/// Why a [`Question`] was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ikigai-judge config: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Question {
    /// A question named `name` (a segment) over the closed set `labels` (at least two
    /// segments, distinct), asked of `urn:llm:<provider>:ask` with the pinned `model`.
    /// `product` is the product principal's IRI: its corridor is the shared view, and it is a
    /// curator. The daily budget starts at 0 (set it from measured volume, [`budget`]).
    ///
    /// [`budget`]: Question::budget
    pub fn new<I, S>(
        name: &str,
        labels: I,
        prompt: &str,
        provider: &str,
        model: &str,
        product: &str,
    ) -> Result<Question, ConfigError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if !is_segment(name) {
            return Err(ConfigError(format!("question `{name}` is not a segment")));
        }
        if !is_segment(provider) {
            return Err(ConfigError(format!(
                "provider `{provider}` is not a segment"
            )));
        }
        if model.is_empty() || model.chars().any(|c| c.is_control()) {
            return Err(ConfigError("the model id is empty or has a control".into()));
        }
        if ikigai_core::principal_scope(product).is_err() {
            return Err(ConfigError(format!("`{product}` is not a principal IRI")));
        }
        let mut out: Vec<Label> = Vec::new();
        for label in labels {
            let label = label.into();
            if !is_segment(&label) || label == "pending" {
                return Err(ConfigError(format!(
                    "label `{label}` is not a segment, or is the reserved `pending`"
                )));
            }
            if out.iter().any(|l| l.name == label) {
                return Err(ConfigError(format!("label `{label}` is listed twice")));
            }
            out.push(Label {
                concept: format!("urn:iki:judge:label:{name}:{label}"),
                name: label,
            });
        }
        if out.len() < 2 {
            return Err(ConfigError("a question needs at least two labels".into()));
        }
        Ok(Question {
            name: name.into(),
            labels: out,
            prompt: prompt.into(),
            provider: provider.into(),
            model: model.into(),
            bayes: None,
            budget: 0,
            previous: Vec::new(),
            mapping: None,
            curators: Vec::new(),
            product: product.into(),
        })
    }

    /// The ikigai-bayes model (`urn:bayes:{model}:*`) scoring this question; its two labels
    /// must be in the question's set.
    pub fn bayes(mut self, model: &str) -> Self {
        self.bayes = Some(model.into());
        self
    }

    /// Model calls allowed per source per day.
    pub fn budget(mut self, calls_per_day: u32) -> Self {
        self.budget = calls_per_day;
        self
    }

    /// A previous classifier whose verdicts still answer, after the current one's.
    pub fn previous(mut self, classifier: Classifier) -> Self {
        self.previous.push(classifier);
        self
    }

    /// The per-source mapping rule (pure).
    pub fn mapping(mut self, mapping: Mapping) -> Self {
        self.mapping = Some(mapping);
        self
    }

    /// Another curator: a principal whose corrections write the SHARED view and train the
    /// shared bayes model. Any other principal's corrections stay in their own corridor.
    pub fn curator(mut self, principal: &str) -> Self {
        self.curators.push(principal.into());
        self
    }

    /// The question's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The current classifier.
    pub fn classifier(&self) -> Classifier {
        Classifier {
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_digest: digest_hex(self.prompt.as_bytes()),
            labels: self.labels.iter().map(|l| l.name.clone()).collect(),
        }
    }

    pub(crate) fn label(&self, name: &str) -> Option<&Label> {
        self.labels.iter().find(|l| l.name == name)
    }

    pub(crate) fn label_of_concept(&self, concept: &str) -> Option<&Label> {
        self.labels.iter().find(|l| l.concept == concept)
    }

    pub(crate) fn is_curator(&self, principal: &str) -> bool {
        principal == self.product || self.curators.iter().any(|c| c == principal)
    }
}

/// Who produced a verdict, in rank order (a lower rank answers first).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Producer {
    /// A person's correction: the reader's own corridor (`shared: false`) ranks above the
    /// shared view (the product principal's).
    Person {
        /// Whether this is the shared (product) view.
        shared: bool,
    },
    /// The per-source mapping.
    Mapping,
    /// A recorded model verdict: `generation` 0 is the current classifier, then each previous
    /// one in the order the question lists them.
    Model {
        /// Which classifier, 0 the current.
        generation: usize,
    },
    /// A recorded verdict (person or model) on the item's PREVIOUS text.
    Stale,
    /// A bayes score; it answers only when confident.
    Bayes,
}

impl Producer {
    /// The producer's name, as `why` and `disagreements` report it.
    pub fn name(&self) -> &'static str {
        match self {
            Producer::Person { shared: false } => "person",
            Producer::Person { shared: true } => "person-shared",
            Producer::Mapping => "mapping",
            Producer::Model { .. } => "model",
            Producer::Stale => "stale",
            Producer::Bayes => "bayes",
        }
    }
}

/// One verdict that exists for an item.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// Who produced it.
    pub producer: Producer,
    /// The label, or `None` for an unparsed model answer (spent, never answers) or an
    /// unsure bayes score.
    pub label: Option<String>,
    /// Whether the verdict ANSWERS (a label from a producer that may decide: a bayes score
    /// only when it is confident).
    pub answers: bool,
    /// Where it came from: `(key, value)` pairs, the resource first.
    pub provenance: Vec<(String, String)>,
}

/// The deciding verdict: the first, in rank order, that answers. `None` is `pending`.
pub fn decide(verdicts: &[Verdict]) -> Option<&Verdict> {
    let mut ranked: Vec<&Verdict> = verdicts.iter().collect();
    ranked.sort_by(|a, b| a.producer.cmp(&b.producer));
    ranked.into_iter().find(|v| v.answers)
}

/// The verdicts that disagree with the deciding one: another producer's label that differs.
/// A correction (a person verdict deciding) resolves every disagreement, so then there are
/// none; a bayes score counts only when confident.
pub fn disagreements(verdicts: &[Verdict]) -> Vec<&Verdict> {
    let Some(deciding) = decide(verdicts) else {
        return Vec::new();
    };
    if matches!(deciding.producer, Producer::Person { .. }) {
        return Vec::new();
    }
    verdicts
        .iter()
        .filter(|v| v.answers && v.producer != deciding.producer && v.label != deciding.label)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(producer: Producer, label: Option<&str>, answers: bool) -> Verdict {
        Verdict {
            producer,
            label: label.map(String::from),
            answers,
            provenance: Vec::new(),
        }
    }

    #[test]
    fn rank_order_is_person_mapping_model_stale_bayes() {
        let all = [
            v(Producer::Bayes, Some("e"), true),
            v(Producer::Stale, Some("d"), true),
            v(Producer::Model { generation: 1 }, Some("c2"), true),
            v(Producer::Model { generation: 0 }, Some("c"), true),
            v(Producer::Mapping, Some("b"), true),
            v(Producer::Person { shared: true }, Some("a2"), true),
            v(Producer::Person { shared: false }, Some("a"), true),
        ];
        let order = ["a", "a2", "b", "c", "c2", "d", "e"];
        for (n, want) in order.iter().enumerate() {
            let rest: Vec<Verdict> = all
                .iter()
                .filter(|x| !order[..n].contains(&x.label.as_deref().unwrap()))
                .cloned()
                .collect();
            assert_eq!(decide(&rest).unwrap().label.as_deref(), Some(*want));
        }
        assert!(decide(&[]).is_none(), "nothing answers: pending");
    }

    #[test]
    fn an_unsure_bayes_and_an_unparsed_model_do_not_answer() {
        let vs = vec![
            v(Producer::Model { generation: 0 }, None, false),
            v(Producer::Bayes, None, false),
        ];
        assert!(decide(&vs).is_none());
    }

    #[test]
    fn a_correction_resolves_every_disagreement() {
        let mut vs = vec![
            v(Producer::Model { generation: 0 }, Some("yes"), true),
            v(Producer::Bayes, Some("no"), true),
        ];
        assert_eq!(disagreements(&vs).len(), 1);
        assert_eq!(decide(&vs).unwrap().producer.name(), "model");
        vs.push(v(Producer::Person { shared: true }, Some("no"), true));
        assert!(disagreements(&vs).is_empty());
    }

    #[test]
    fn classifier_digest_moves_with_each_part() {
        let q = Question::new("q", ["yes", "no"], "p", "fake", "m1", "urn:example:p").unwrap();
        let a = q.classifier();
        let mut b = a.clone();
        b.model = "m2".into();
        let mut c = a.clone();
        c.prompt_digest = digest_hex(b"other");
        let mut d = a.clone();
        d.labels.reverse();
        let ds = [a.digest(), b.digest(), c.digest(), d.digest()];
        for i in 0..4 {
            assert_eq!(ds[i].len(), 64);
            for j in 0..i {
                assert_ne!(ds[i], ds[j]);
            }
        }
    }

    #[test]
    fn segments() {
        assert!(is_segment("contact"));
        assert!(is_segment("a-b1"));
        for bad in ["", "A", "1a", "a--b", "a-", "a:b", "a/b"] {
            assert!(!is_segment(bad), "{bad}");
        }
    }
}
