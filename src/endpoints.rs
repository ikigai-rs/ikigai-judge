//! The resources: per question, its config atoms, its stored verdicts, the compositions over
//! them (class, why, pending, disagreements) and the two Sinks (judge, correct).
//!
//! Every door is bound for one question the host configured (`urn:iki:judge:class:contact:
//! {digest}`, not a template over `{q}`), so each declares that question's own capability and
//! the kernel enforces it. Sub-requests (the atoms through `urn:file:`, the bayes model, the
//! model endpoint) run under the CALLER's capability, so the path ACL on the state root is the
//! second floor: the judge's caller writes `/model`, a reviewer their own `/person/{key}`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use ikigai_core::{
    ArgRef, ArgSpec, AsyncFnEndpoint, Description, EndpointSpace, Error, Exact, FailureClass,
    Invocation, Iri, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use serde_json::{json, Value};

use crate::atoms::{self, escape, ModelRecord, Stored};
use crate::{
    decide, digest_hex, disagreements, is_segment, Classifier, Producer, Question, Verdict,
    MAX_ITEMS, MAX_RAW_BYTES, MAX_TEXT_BYTES,
};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const TEXT: &str = "text/plain";
const JSON: &str = "application/json";
const TURTLE: &str = "text/turtle";
const HTML: &str = "text/html";

const PLAIN_FACES: [&str; 2] = [TEXT, JSON];
const WHY_FACES: [&str; 4] = [TEXT, TURTLE, HTML, JSON];
const TURTLE_FACES: [&str; 2] = [TURTLE, TEXT];

/// The read capability of a question: every Source door.
pub fn read_cap(q: &str) -> String {
    format!("urn:cap:judge:read:{q}")
}

/// The capability of the judge Sink (it spends model calls).
pub fn judge_cap(q: &str) -> String {
    format!("urn:cap:judge:judge:{q}")
}

/// The capability of the correct Sink.
pub fn correct_cap(q: &str) -> String {
    format!("urn:cap:judge:correct:{q}")
}

fn iri(kind: &str, q: &str) -> String {
    format!("urn:iki:judge:{kind}:{q}")
}

/// The space: every question's doors. Host-named: the host chose the questions.
pub fn space<I>(questions: I) -> EndpointSpace
where
    I: IntoIterator<Item = Question>,
{
    let mut space = EndpointSpace::new();
    for q in questions {
        let q = Arc::new(q);
        let n = q.name.clone();
        let t = |s: String| UriTemplate::parse(s).expect("a judge template is valid");
        space = space
            .bind(Exact::new(iri("question", &n)), question_door(q.clone()))
            .bind(Exact::new(iri("prompt", &n)), prompt_door(q.clone()))
            .bind(
                Exact::new(iri("classifier", &n)),
                classifier_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{classifier}}:{{digest}}", iri("model", &n))),
                model_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{digest}}", iri("person", &n))),
                person_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{digest}}", iri("class", &n))),
                class_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{digest}}", iri("why", &n))),
                why_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{source}}", iri("pending", &n))),
                pending_door(q.clone()),
            )
            .bind(
                Exact::new(iri("disagreements", &n)),
                disagreements_door(q.clone()),
            )
            .bind(
                t(format!("{}:{{source}}:{{date}}", iri("spent", &n))),
                spent_door(q.clone()),
            )
            .bind(Exact::new(iri("judge", &n)), judge_door(q.clone()))
            .bind(Exact::new(iri("correct", &n)), correct_door(q));
    }
    space
}

// --- arguments ----------------------------------------------------------------------------

fn xsd(local: &str) -> String {
    format!("{XSD}{local}")
}

fn optional_str<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    match inv.request.args.get(name) {
        None => Ok(None),
        Some(_) => inv.inline_str(name).map(Some),
    }
}

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.into(),
        detail: detail.into(),
    }
}

fn face<'a>(inv: &Invocation<'_>, faces: &[&'a str]) -> Result<&'a str> {
    match optional_str(inv, "as")? {
        None => Ok(faces[0]),
        Some(asked) => faces.iter().copied().find(|f| *f == asked).ok_or_else(|| {
            invalid(
                "as",
                format!("`{asked}` is not one of {}", faces.join(", ")),
            )
        }),
    }
}

fn as_spec(faces: &[&str]) -> ArgSpec {
    ArgSpec::new("as")
        .summary("the representation to answer with")
        .class(xsd("string"))
        .one_of(faces.iter().copied())
        .default_value(faces[0])
}

fn binding(name: &str, summary: &str) -> ArgSpec {
    ArgSpec::new(name)
        .summary(summary)
        .class(xsd("string"))
        .binding()
}

fn content_spec() -> ArgSpec {
    ArgSpec::new("content")
        .summary(format!(
            "the item's text (piped), at most {MAX_TEXT_BYTES} bytes; its bare-hex BLAKE3 \
             digest is the item's `{{digest}}`"
        ))
        .class(xsd("string"))
}

fn item_spec() -> ArgSpec {
    ArgSpec::new("item")
        .summary(
            "the item's IRI: the annotation's target, and how a previous text's verdict is \
             found after an edit (the item's last judged digest)",
        )
        .class(xsd("anyURI"))
        .optional()
}

fn source_spec(required: bool) -> ArgSpec {
    let spec = ArgSpec::new("source")
        .summary("the item's source (a segment): what the mapping and the budget are per")
        .class(xsd("string"));
    if required {
        spec
    } else {
        spec.optional()
    }
}

fn items_spec() -> ArgSpec {
    ArgSpec::new("items")
        .summary(format!(
            "a JSON array of at most {MAX_ITEMS} objects `{{\"content\", \"item\"?, \
             \"source\"?}}` (piped)"
        ))
        .class(xsd("string"))
}

fn describe(
    name: &str,
    title: &str,
    summary: &str,
    verb: Verb,
    cap: Option<String>,
    inputs: Vec<ArgSpec>,
    faces: &[&str],
) -> Description {
    let mut d = Description::new(name)
        .title(title)
        .summary(summary)
        .verb(verb)
        .verb(Verb::Meta)
        // A wrong request or state (InvalidArgument, NotFound, Conflict), a stored atom that
        // does not parse (Unknown); the judge adds RetryAfter.
        .fails(FailureClass::Permanent)
        .fails(FailureClass::Unknown);
    if verb == Verb::Sink && name.ends_with("-judge") {
        d = d.fails(FailureClass::RetryAfter);
    }
    if let Some(cap) = cap {
        d = d.requires(cap);
    }
    for input in inputs {
        d = d.input(input);
    }
    for f in faces {
        d = d.output(*f);
    }
    d
}

fn answer(face: &str, body: String) -> Representation {
    let repr_type = if face == JSON {
        ReprType::new(JSON)
    } else {
        ReprType::new(face).with_param("charset", "utf-8")
    };
    Representation::new(repr_type, body.into_bytes())
}

fn bound(name: &str) -> impl Fn(&Invocation<'_>) -> Result<String> + '_ {
    move |inv| {
        inv.bindings
            .get(name)
            .map(String::from)
            .ok_or_else(|| Error::MissingArgument(name.into()))
    }
}

/// The item a call names: its text, digest, IRI (or the text's own name) and source.
struct Item {
    content: String,
    digest: String,
    item: Option<String>,
    source: Option<String>,
}

impl Item {
    fn target(&self) -> String {
        self.item
            .clone()
            .unwrap_or_else(|| format!("urn:iki:judge:text:{}", self.digest))
    }
}

fn check_item(content: &str, item: Option<&str>, source: Option<&str>) -> Result<Item> {
    if content.len() > MAX_TEXT_BYTES {
        return Err(invalid(
            "content",
            format!(
                "the text is {} bytes, past the bound of {MAX_TEXT_BYTES}: refused, not truncated",
                content.len()
            ),
        ));
    }
    if let Some(item) = item {
        Iri::parse(item).map_err(|e| invalid("item", e.to_string()))?;
    }
    if let Some(source) = source {
        if !is_segment(source) {
            return Err(invalid("source", format!("`{source}` is not a segment")));
        }
    }
    Ok(Item {
        digest: digest_hex(content.as_bytes()),
        content: content.into(),
        item: item.map(String::from),
        source: source.map(String::from),
    })
}

/// The item a door's own arguments name; with `digest` bound, it must be the text's.
fn item_of(inv: &Invocation<'_>, source_required: bool) -> Result<Item> {
    let content = inv.inline_str("content")?;
    let source = if source_required {
        Some(inv.inline_str("source")?)
    } else {
        optional_str(inv, "source")?
    };
    let item = check_item(content, optional_str(inv, "item")?, source)?;
    if let Some(named) = inv.bindings.get("digest") {
        if named != item.digest {
            return Err(invalid(
                "content",
                format!(
                    "the text's digest is {}, not the {named} this resource names",
                    item.digest
                ),
            ));
        }
    }
    Ok(item)
}

fn items_of(inv: &Invocation<'_>) -> Result<Vec<Item>> {
    let text = inv.inline_str("items")?;
    let parsed: Value =
        serde_json::from_str(text).map_err(|e| invalid("items", format!("not JSON: {e}")))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| invalid("items", "not a JSON array"))?;
    if list.len() > MAX_ITEMS {
        return Err(invalid(
            "items",
            format!(
                "{} items, past the bound of {MAX_ITEMS}: refused, not truncated",
                list.len()
            ),
        ));
    }
    let field = |o: &Value, k: &str| -> Result<Option<String>> {
        match o.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(invalid("items", format!("`{k}` is not a string"))),
        }
    };
    list.iter()
        .map(|o| {
            let content =
                field(o, "content")?.ok_or_else(|| invalid("items", "an item has no `content`"))?;
            check_item(
                &content,
                field(o, "item")?.as_deref(),
                field(o, "source")?.as_deref(),
            )
        })
        .collect()
}

// --- sub-requests -------------------------------------------------------------------------

fn request(verb: Verb, target: &str, args: &[(&str, &str)]) -> Result<Request> {
    let iri = Iri::parse(target).map_err(|e| Error::Endpoint(format!("<{target}>: {e}")))?;
    let mut request = Request::new(verb, iri);
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    Ok(request)
}

/// Read an atom; `None` when it was never written.
async fn read_atom(inv: &Invocation<'_>, path: &str) -> Result<Option<String>> {
    match inv.issue(request(Verb::Source, path, &[])?).await {
        Ok(rep) => String::from_utf8(rep.bytes)
            .map(Some)
            .map_err(|_| Error::Endpoint(format!("<{path}> is not UTF-8"))),
        Err(Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

async fn write_atom(inv: &Invocation<'_>, path: &str, body: &str) -> Result<()> {
    inv.issue(request(Verb::Sink, path, &[("content", body)])?)
        .await
        .map(|_| ())
}

fn now(inv: &Invocation<'_>) -> Result<(String, String)> {
    let t = inv.now().ok_or_else(|| Error::Unsupported {
        verb: None,
        detail: "the host configured no clock, and a verdict records its time".into(),
    })?;
    Ok(atoms::date_time(t.as_millis()))
}

/// The principal whose view a call reads and writes: the caller's, or (for a caller naming
/// none, such as the host's root) the product principal's shared view.
fn principal<'a>(inv: &'a Invocation<'_>, q: &'a Question) -> &'a str {
    inv.capability.principal().unwrap_or(&q.product)
}

// --- gathering the verdicts ---------------------------------------------------------------

fn model_name(q: &str, classifier: &str, digest: &str) -> String {
    format!("{}:{classifier}:{digest}", iri("model", q))
}

fn person_name(q: &str, digest: &str) -> String {
    format!("{}:{digest}", iri("person", q))
}

fn classifiers(q: &Question) -> Vec<Classifier> {
    std::iter::once(q.classifier())
        .chain(q.previous.iter().cloned())
        .collect()
}

fn stored_verdict(q: &Question, producer: Producer, stored: Stored, resource: String) -> Verdict {
    let label = stored
        .concept
        .as_deref()
        .and_then(|c| q.label_of_concept(c))
        .map(|l| l.name.clone());
    let mut provenance = vec![
        ("resource".to_string(), resource),
        ("agent".to_string(), stored.agent),
        ("created".to_string(), stored.created),
    ];
    if let Some(raw) = stored.raw {
        provenance.push(("unparsed".into(), raw));
    }
    Verdict {
        producer,
        answers: label.is_some(),
        label,
        provenance,
    }
}

/// The recorded verdicts for one digest: the person corridors, then each classifier.
async fn recorded(
    inv: &Invocation<'_>,
    q: &Question,
    digest: &str,
    who: &str,
) -> Result<Vec<Verdict>> {
    let mut out = Vec::new();
    let mut corridors = Vec::new();
    if who != q.product {
        corridors.push((who, false));
    }
    corridors.push((q.product.as_str(), true));
    for (principal, shared) in corridors {
        let path = atoms::person_path(principal, &q.name, digest);
        if let Some(text) = read_atom(inv, &path).await? {
            let name = person_name(&q.name, digest);
            let stored = atoms::parse_verdict(&text, &name)?;
            out.push(stored_verdict(q, Producer::Person { shared }, stored, path));
        }
    }
    for (generation, classifier) in classifiers(q).iter().enumerate() {
        let c = classifier.digest();
        let path = atoms::model_path(&q.name, &c, digest);
        if let Some(text) = read_atom(inv, &path).await? {
            let stored = atoms::parse_verdict(&text, &model_name(&q.name, &c, digest))?;
            let mut v = stored_verdict(q, Producer::Model { generation }, stored, path);
            v.provenance
                .push(("model".into(), classifier.model.clone()));
            v.provenance.push(("classifier".into(), c));
            out.push(v);
        }
    }
    Ok(out)
}

/// Bayes's clues and score, when the question has a model.
struct BayesScore {
    verdict: Verdict,
    clues: Vec<(String, f64)>,
}

async fn bayes(inv: &Invocation<'_>, q: &Question, content: &str) -> Result<Option<BayesScore>> {
    let Some(model) = &q.bayes else {
        return Ok(None);
    };
    let target = format!("urn:bayes:{model}:score");
    let rep = inv
        .issue(request(
            Verb::Source,
            &target,
            &[("content", content), ("as", JSON)],
        )?)
        .await?;
    let v: Value = serde_json::from_slice(&rep.bytes)
        .map_err(|e| Error::Endpoint(format!("<{target}> answered no JSON: {e}")))?;
    let class = v["class"].as_str().unwrap_or("unsure").to_string();
    let label = q.label(&class).map(|l| l.name.clone());
    let clues = v["clues"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .map(|c| {
                    (
                        c["token"].as_str().unwrap_or("").to_string(),
                        c["probability"].as_f64().unwrap_or(0.5),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(BayesScore {
        verdict: Verdict {
            producer: Producer::Bayes,
            answers: label.is_some(),
            label,
            provenance: vec![
                ("resource".into(), target),
                ("class".into(), class),
                (
                    "score".into(),
                    format!("{:.6}", v["score"].as_f64().unwrap_or(0.5)),
                ),
                (
                    "modelDigest".into(),
                    v["modelDigest"].as_str().unwrap_or("").to_string(),
                ),
            ],
        },
        clues,
    }))
}

/// Everything that exists for an item, unranked, and bayes's clues.
struct Gathered {
    verdicts: Vec<Verdict>,
    clues: Vec<(String, f64)>,
    /// The previous digest whose verdict answers as stale, if one does.
    stale_from: Option<String>,
}

async fn gather(inv: &Invocation<'_>, q: &Question, item: &Item) -> Result<Gathered> {
    let who = principal(inv, q).to_string();
    let mut verdicts = recorded(inv, q, &item.digest, &who).await?;
    if let (Some(mapping), Some(source)) = (&q.mapping, &item.source) {
        if let Some(label) = mapping(source, &item.target()).filter(|l| q.label(l).is_some()) {
            verdicts.push(Verdict {
                producer: Producer::Mapping,
                label: Some(label),
                answers: true,
                provenance: vec![("source".into(), source.clone())],
            });
        }
    }
    let mut stale_from = None;
    if let Some(iri) = &item.item {
        // The item's last judged digest: the reader's own correction, then the shared one,
        // then the judge's. A digest equal to the current one is no previous text.
        let mut lasts = Vec::new();
        if who != q.product {
            lasts.push(atoms::person_last_path(&who, &q.name, iri));
        }
        lasts.push(atoms::person_last_path(&q.product, &q.name, iri));
        lasts.push(atoms::model_last_path(&q.name, iri));
        for path in lasts {
            let Some(text) = read_atom(inv, &path).await? else {
                continue;
            };
            let previous = atoms::parse_last(&text, &path)?;
            if previous == item.digest {
                continue;
            }
            let old = recorded(inv, q, &previous, &who).await?;
            if let Some(v) = decide(&old) {
                let mut v = v.clone();
                v.provenance
                    .insert(0, ("staleFrom".into(), previous.clone()));
                v.provenance
                    .insert(1, ("was".into(), v.producer.name().into()));
                v.producer = Producer::Stale;
                verdicts.push(v);
                stale_from = Some(previous);
                break;
            }
        }
    }
    let mut clues = Vec::new();
    if let Some(score) = bayes(inv, q, &item.content).await? {
        verdicts.push(score.verdict);
        clues = score.clues;
    }
    verdicts.sort_by(|a, b| a.producer.cmp(&b.producer));
    Ok(Gathered {
        verdicts,
        clues,
        stale_from,
    })
}

fn class_of(g: &Gathered) -> (&str, Option<&Verdict>) {
    match decide(&g.verdicts) {
        Some(v) => (v.label.as_deref().unwrap_or("pending"), Some(v)),
        None => ("pending", None),
    }
}

fn verdict_json(v: &Verdict) -> Value {
    let provenance: serde_json::Map<String, Value> = v
        .provenance
        .iter()
        .map(|(k, val)| (k.clone(), Value::String(val.clone())))
        .collect();
    json!({
        "answers": v.answers,
        "label": v.label,
        "producer": v.producer.name(),
        "provenance": provenance,
    })
}

fn provenance_text(v: &Verdict) -> String {
    v.provenance
        .iter()
        .map(|(k, val)| format!("{k}={}", val.replace('\n', "\\n")))
        .collect::<Vec<_>>()
        .join(" ")
}

// --- config doors -------------------------------------------------------------------------

fn question_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-question"),
        &format!("The `{n}` question"),
        "The question as configured: its labels as a SKOS concept scheme, its prompt, its \
         provider and PINNED model (temperature 0), its bayes model, its daily budget and \
         its previous classifiers. Fixed for the life of the host.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![as_spec(&TURTLE_FACES)],
        &TURTLE_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-question"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &TURTLE_FACES)?;
            let me = iri("question", &q.name);
            let body = if face == TURTLE {
                let mut out = format!(
                    "{}<{me}> a skos:ConceptScheme ;\n  skos:prefLabel \"{}\" ;\n  \
                     prov:used <{}> ;\n  prov:wasAssociatedWith <urn:llm:{}:ask> ;\n  \
                     schema:instrument \"{}\" ;\n  schema:version \"{}\" ;\n  \
                     schema:maxValue \"{}\"^^xsd:integer",
                    atoms::PREFIXES,
                    q.name,
                    iri("prompt", &q.name),
                    q.provider,
                    escape(&q.model),
                    q.classifier().digest(),
                    q.budget,
                );
                if let Some(b) = &q.bayes {
                    out.push_str(&format!(" ;\n  dcterms:requires <urn:bayes:{b}:score>"));
                }
                for c in &q.previous {
                    out.push_str(&format!(
                        " ;\n  dcterms:replaces \"{}\"",
                        c.digest()
                    ));
                }
                out.push_str(" .\n");
                for l in &q.labels {
                    out.push_str(&format!(
                        "\n<{}> a skos:Concept ;\n  skos:inScheme <{me}> ;\n  skos:prefLabel \"{}\" .\n",
                        l.concept, l.name
                    ));
                }
                out
            } else {
                let mut out = format!(
                    "question {}\nlabels {}\nprovider {}\nmodel {}\nclassifier {}\nbudget {}\n",
                    q.name,
                    q.labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>().join(" "),
                    q.provider,
                    q.model,
                    q.classifier().digest(),
                    q.budget
                );
                if let Some(b) = &q.bayes {
                    out.push_str(&format!("bayes {b}\n"));
                }
                for c in &q.previous {
                    out.push_str(&format!("previous {}\n", c.digest()));
                }
                out
            };
            Ok(answer(face, body).cacheable())
        })
    })
    .with_description(d)
}

fn prompt_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-prompt"),
        &format!("The `{n}` question's prompt"),
        "The prompt text the model is asked with. Fixed for the life of the host.",
        Verb::Source,
        Some(read_cap(&n)),
        Vec::new(),
        &[TEXT],
    );
    AsyncFnEndpoint::new(format!("judge-{n}-prompt"), move |_inv| {
        let q = q.clone();
        Box::pin(async move { Ok(answer(TEXT, q.prompt.clone()).cacheable()) })
    })
    .with_description(d)
}

fn classifier_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-classifier"),
        &format!("The `{n}` question's current classifier"),
        "The bare-hex BLAKE3 digest over (provider, model, prompt digest, labels): the \
         `{classifier}` segment of a model verdict's name. A pure function of the config.",
        Verb::Source,
        Some(read_cap(&n)),
        Vec::new(),
        &[TEXT],
    );
    AsyncFnEndpoint::new(format!("judge-{n}-classifier"), move |_inv| {
        let q = q.clone();
        Box::pin(async move { Ok(answer(TEXT, q.classifier().digest()).cacheable()) })
    })
    .with_description(d)
}

// --- atom doors ---------------------------------------------------------------------------

fn hex_binding(inv: &Invocation<'_>, name: &str) -> Result<String> {
    let v = bound(name)(inv)?;
    if v.len() != 64
        || !v
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Err(invalid(
            name,
            format!("`{v}` is not a bare lowercase-hex BLAKE3 digest"),
        ));
    }
    Ok(v)
}

fn model_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-model"),
        &format!("A recorded model verdict for `{n}`"),
        "The model verdict atom (Turtle, an oa:Annotation) recorded for a classifier and a \
         text digest; NotFound until the judge writes it. Cacheable on the atom's thread.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![
            binding("classifier", "the classifier's bare-hex digest"),
            binding("digest", "the text's bare-hex digest"),
        ],
        &[TURTLE],
    );
    AsyncFnEndpoint::new(format!("judge-{n}-model"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let c = hex_binding(inv, "classifier")?;
            let digest = hex_binding(inv, "digest")?;
            let path = atoms::model_path(&q.name, &c, &digest);
            match read_atom(inv, &path).await? {
                Some(text) => Ok(answer(TURTLE, text).cacheable()),
                None => Err(Error::NotFound(model_name(&q.name, &c, &digest))),
            }
        })
    })
    .with_description(d)
}

fn person_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-person"),
        &format!("A person's correction for `{n}`, in the caller's corridor"),
        "The latest correction (Turtle, an oa:Annotation with its principal and time) in the \
         CALLER's own corridor; a caller naming no principal reads the shared (product) view. \
         NotFound until written. Cacheable on the atom's thread.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![binding("digest", "the text's bare-hex digest")],
        &[TURTLE],
    );
    AsyncFnEndpoint::new(format!("judge-{n}-person"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let digest = hex_binding(inv, "digest")?;
            let path = atoms::person_path(principal(inv, &q), &q.name, &digest);
            match read_atom(inv, &path).await? {
                Some(text) => Ok(answer(TURTLE, text).cacheable()),
                None => Err(Error::NotFound(person_name(&q.name, &digest))),
            }
        })
    })
    .with_description(d)
}

fn spent_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-spent"),
        &format!("Model calls spent for `{n}`"),
        "Model calls spent for a source on a UTC date (`YYYY-MM-DD`), and the daily budget: \
         `spent <n> of <budget>`. 0 until the judge spends one. Cacheable on the atom's thread.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![
            binding("source", "the source (a segment)"),
            binding("date", "the UTC date, YYYY-MM-DD"),
        ],
        &[TEXT],
    );
    AsyncFnEndpoint::new(format!("judge-{n}-spent"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let source = bound("source")(inv)?;
            let date = bound("date")(inv)?;
            if !is_segment(&source) {
                return Err(invalid("source", format!("`{source}` is not a segment")));
            }
            let ok_date = date.len() == 10
                && date.char_indices().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        c == '-'
                    } else {
                        c.is_ascii_digit()
                    }
                });
            if !ok_date {
                return Err(invalid("date", format!("`{date}` is not YYYY-MM-DD")));
            }
            let spent = spent(inv, &q, &source, &date).await?;
            Ok(answer(TEXT, format!("spent {spent} of {}\n", q.budget)).cacheable())
        })
    })
    .with_description(d)
}

fn spent_name(q: &str, source: &str, date: &str) -> String {
    format!("{}:{source}:{date}", iri("spent", q))
}

async fn spent(inv: &Invocation<'_>, q: &Question, source: &str, date: &str) -> Result<u32> {
    match read_atom(inv, &atoms::spent_path(&q.name, source, date)).await? {
        Some(text) => atoms::parse_spent(&text, &spent_name(&q.name, source, date)),
        None => Ok(0),
    }
}

// --- compositions -------------------------------------------------------------------------

fn item_inputs(source_required: bool) -> Vec<ArgSpec> {
    vec![content_spec(), item_spec(), source_spec(source_required)]
}

fn class_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let mut inputs = vec![binding(
        "digest",
        "the text's bare-hex digest; must be the content's",
    )];
    inputs.extend(item_inputs(false));
    inputs.push(as_spec(&PLAIN_FACES));
    let d = describe(
        &format!("judge-{n}-class"),
        &format!("An item's class for `{n}`"),
        "The first verdict that answers: person (the caller's corridor, then the shared view) \
         ▸ mapping ▸ model (the current classifier, then previous ones) ▸ the previous text's \
         recorded verdict, marked stale ▸ bayes when confident ▸ the value `pending`. A READ: \
         it never writes. Cacheable on the atoms' and the bayes model's threads.",
        Verb::Source,
        Some(read_cap(&n)),
        inputs,
        &PLAIN_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-class"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &PLAIN_FACES)?;
            let item = item_of(inv, false)?;
            let g = gather(inv, &q, &item).await?;
            let (class, by) = class_of(&g);
            let stale = by.is_some_and(|v| v.producer == Producer::Stale);
            let body = if face == JSON {
                json!({
                    "class": class,
                    "digest": item.digest,
                    "producer": by.map(|v| v.producer.name()),
                    "question": q.name,
                    "stale": stale,
                    "staleFrom": if stale { g.stale_from.clone() } else { None },
                })
                .to_string()
            } else {
                match by {
                    Some(_) if stale => format!(
                        "{class} stale {}\n",
                        g.stale_from.as_deref().unwrap_or_default()
                    ),
                    Some(v) => format!("{class} {}\n", v.producer.name()),
                    None => "pending\n".to_string(),
                }
            };
            Ok(answer(face, body).cacheable())
        })
    })
    .with_description(d)
}

fn why_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let mut inputs = vec![binding(
        "digest",
        "the text's bare-hex digest; must be the content's",
    )];
    inputs.extend(item_inputs(false));
    inputs.push(as_spec(&WHY_FACES));
    let d = describe(
        &format!("judge-{n}-why"),
        &format!("Why an item has its class for `{n}`"),
        "Every verdict that exists for the item, ranked, each with its provenance, which one \
         decided (or `pending`), whether it is stale, and bayes's strongest clues. Faces: \
         text, Turtle (skolemized), HTML, JSON. Cacheable like the class.",
        Verb::Source,
        Some(read_cap(&n)),
        inputs,
        &WHY_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-why"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &WHY_FACES)?;
            let item = item_of(inv, false)?;
            let g = gather(inv, &q, &item).await?;
            let (class, by) = class_of(&g);
            let body = match face {
                JSON => json!({
                    "class": class,
                    "clues": g.clues.iter().map(|(t, p)| json!({"probability": p, "token": t})).collect::<Vec<_>>(),
                    "digest": item.digest,
                    "producer": by.map(|v| v.producer.name()),
                    "verdicts": g.verdicts.iter().map(verdict_json).collect::<Vec<_>>(),
                })
                .to_string(),
                TURTLE => why_turtle(&q, &item, &g, class),
                HTML => why_html(&item, &g, class),
                _ => why_text(&item, &g, class),
            };
            Ok(answer(face, body).cacheable())
        })
    })
    .with_description(d)
}

fn why_text(item: &Item, g: &Gathered, class: &str) -> String {
    let deciding = decide(&g.verdicts);
    let mut out = format!("class {class}\ndigest {}\n", item.digest);
    if let Some(from) = deciding
        .filter(|v| v.producer == Producer::Stale)
        .and(g.stale_from.as_ref())
    {
        out.push_str(&format!(
            "stale: the text changed; this is the verdict on {from}, queued for re-judging\n"
        ));
    }
    for (rank, v) in g.verdicts.iter().enumerate() {
        let mark = if deciding == Some(v) { "*" } else { " " };
        out.push_str(&format!(
            "{mark}{} {} {} {}\n",
            rank + 1,
            v.producer.name(),
            v.label.as_deref().unwrap_or("-"),
            provenance_text(v)
        ));
    }
    for (token, p) in &g.clues {
        out.push_str(&format!("clue {p:.6} {token}\n"));
    }
    out
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn why_html(item: &Item, g: &Gathered, class: &str) -> String {
    let deciding = decide(&g.verdicts);
    let mut out = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>class {c}</title></head><body>\n\
         <h1>class {c}</h1>\n<p>digest <code>{}</code></p>\n",
        item.digest,
        c = html_escape(class)
    );
    if let Some(from) = deciding
        .filter(|v| v.producer == Producer::Stale)
        .and(g.stale_from.as_ref())
    {
        out.push_str(&format!(
            "<p>stale: the verdict on <code>{from}</code>, queued for re-judging</p>\n"
        ));
    }
    out.push_str("<ol>\n");
    for v in &g.verdicts {
        let open = if deciding == Some(v) { "<strong>" } else { "" };
        let close = if deciding == Some(v) { "</strong>" } else { "" };
        out.push_str(&format!(
            "<li>{open}{} {}{close} <small>{}</small></li>\n",
            v.producer.name(),
            html_escape(v.label.as_deref().unwrap_or("-")),
            html_escape(&provenance_text(v))
        ));
    }
    out.push_str("</ol>\n");
    if !g.clues.is_empty() {
        out.push_str("<ul>\n");
        for (token, p) in &g.clues {
            out.push_str(&format!(
                "<li>{p:.6} <code>{}</code></li>\n",
                html_escape(token)
            ));
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</body></html>\n");
    out
}

/// Skolemized: the why is `urn:iki:judge:why:{q}:{digest}`, each verdict that plus `:{rank}`.
fn why_turtle(q: &Question, item: &Item, g: &Gathered, class: &str) -> String {
    let me = format!("{}:{}", iri("why", &q.name), item.digest);
    let deciding = decide(&g.verdicts);
    let mut out = format!(
        "{}<{me}> a prov:Entity ;\n  dcterms:subject <{}> ;\n  dcterms:identifier \"b3:{}\" ;\n  schema:result ",
        atoms::PREFIXES,
        item.target(),
        item.digest
    );
    match q.label(class) {
        Some(l) => out.push_str(&format!("<{}>", l.concept)),
        None => out.push_str("\"pending\""),
    }
    for (rank, _) in g.verdicts.iter().enumerate() {
        out.push_str(&format!(" ;\n  prov:hadMember <{me}:{}>", rank + 1));
    }
    out.push_str(" .\n");
    for (rank, v) in g.verdicts.iter().enumerate() {
        out.push_str(&format!(
            "\n<{me}:{}> a prov:Entity ;\n  schema:position \"{}\"^^xsd:integer ;\n  \
             schema:agent \"{}\" ;\n  schema:result {} ;\n  schema:description \"{}\"",
            rank + 1,
            rank + 1,
            v.producer.name(),
            match v.label.as_deref().and_then(|l| q.label(l)) {
                Some(l) => format!("<{}>", l.concept),
                None => "\"none\"".into(),
            },
            escape(&provenance_text(v)),
        ));
        if deciding == Some(v) {
            out.push_str(&format!(" ;\n  schema:isPartOf <{me}>"));
        }
        out.push_str(" .\n");
    }
    out
}

fn pending_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-pending"),
        &format!("A source's items that need judging for `{n}`"),
        "Of the items given (a JSON array, piped), those whose class is `pending`, and those \
         answered by a STALE verdict (queued for re-judging): one `<digest> <pending|stale|\
         unparsed> <item>` line each. An item with an unparsed model verdict needs a person: \
         the judge never retries it. Cacheable like the class.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![
            binding("source", "the source (a segment)"),
            items_spec(),
            as_spec(&PLAIN_FACES),
        ],
        &PLAIN_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-pending"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &PLAIN_FACES)?;
            let source = bound("source")(inv)?;
            if !is_segment(&source) {
                return Err(invalid("source", format!("`{source}` is not a segment")));
            }
            let mut rows = Vec::new();
            for mut item in items_of(inv)? {
                item.source = Some(source.clone());
                let g = gather(inv, &q, &item).await?;
                let state = match decide(&g.verdicts) {
                    Some(v) if v.producer == Producer::Stale => "stale",
                    Some(_) => continue,
                    None if g.verdicts.iter().any(|v| {
                        v.producer == Producer::Model { generation: 0 } && !v.answers
                    }) =>
                    {
                        "unparsed"
                    }
                    None => "pending",
                };
                rows.push((item.digest.clone(), state, item.target()));
            }
            let body = if face == JSON {
                json!({
                    "pending": rows.iter().map(|(d, s, i)| json!({"digest": d, "item": i, "state": s})).collect::<Vec<_>>(),
                    "question": q.name,
                    "source": source,
                })
                .to_string()
            } else {
                rows.iter()
                    .map(|(d, s, i)| format!("{d} {s} {i}\n"))
                    .collect()
            };
            Ok(answer(face, body).cacheable())
        })
    })
    .with_description(d)
}

fn disagreements_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let d = describe(
        &format!("judge-{n}-disagreements"),
        &format!("Where producers disagree for `{n}`: the signal"),
        "Of the items given (a JSON array, piped), every one where the deciding verdict and \
         another producer disagree (a model against a confident bayes, a stale verdict against \
         bayes on the new text, a mapping against a model), each with both answers and their \
         provenance. A correction decides, so it resolves the item and it drops out. \
         Cacheable on the atoms' and the bayes model's threads.",
        Verb::Source,
        Some(read_cap(&n)),
        vec![items_spec(), as_spec(&PLAIN_FACES)],
        &PLAIN_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-disagreements"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &PLAIN_FACES)?;
            let mut rows = Vec::new();
            for item in items_of(inv)? {
                let g = gather(inv, &q, &item).await?;
                let Some(deciding) = decide(&g.verdicts) else {
                    continue;
                };
                for other in disagreements(&g.verdicts) {
                    rows.push((
                        item.target(),
                        item.digest.clone(),
                        deciding.clone(),
                        other.clone(),
                    ));
                }
            }
            let body = if face == JSON {
                json!({
                    "disagreements": rows.iter().map(|(i, d, a, b)| json!({
                        "deciding": verdict_json(a),
                        "digest": d,
                        "item": i,
                        "other": verdict_json(b),
                    })).collect::<Vec<_>>(),
                    "question": q.name,
                })
                .to_string()
            } else {
                rows.iter()
                    .map(|(i, d, a, b)| {
                        format!(
                            "{d} {i}\n  deciding {} {} {}\n  other {} {} {}\n",
                            a.producer.name(),
                            a.label.as_deref().unwrap_or("-"),
                            provenance_text(a),
                            b.producer.name(),
                            b.label.as_deref().unwrap_or("-"),
                            provenance_text(b)
                        )
                    })
                    .collect()
            };
            Ok(answer(face, body).cacheable())
        })
    })
    .with_description(d)
}

// --- the judge ----------------------------------------------------------------------------

fn instructions(q: &Question) -> String {
    format!(
        "Classify the text with exactly one label from this closed set: {}. Answer with JSON \
         only, {{\"label\": \"<label>\"}}, and nothing else.",
        q.labels
            .iter()
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The label a model's answer text names, if it names one from the closed set.
fn parse_label<'a>(q: &'a Question, text: &str) -> Option<&'a crate::Label> {
    let t = text.trim();
    let named = match serde_json::from_str::<Value>(t) {
        Ok(v) => v.get("label").and_then(Value::as_str).map(String::from),
        Err(_) => Some(t.to_string()),
    }?;
    q.label(named.trim())
}

type ReservationLock = futures::lock::Mutex<()>;
type ReservationLockMap = HashMap<(String, String), Weak<ReservationLock>>;

/// Endpoint-local reservation locks, keyed by (source, UTC date) for budgets or by
/// (classifier digest, content digest) for items. Weak entries release inactive keys.
#[derive(Default)]
struct ReservationLocks(Mutex<ReservationLockMap>);

impl ReservationLocks {
    fn for_key(&self, first: &str, second: &str) -> Arc<ReservationLock> {
        let mut locks = self.0.lock().expect("reservation lock registry poisoned");
        locks.retain(|_, lock| lock.strong_count() > 0);
        let key = (first.to_string(), second.to_string());
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(futures::lock::Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }
}

fn judge_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let budget_locks = Arc::new(ReservationLocks::default());
    let item_locks = Arc::new(ReservationLocks::default());
    let n = q.name.clone();
    let mut inputs = item_inputs(true);
    inputs.push(as_spec(&PLAIN_FACES));
    let d = describe(
        &format!("judge-{n}-judge"),
        &format!("Judge an item for `{n}` with the pinned model"),
        "Idempotent: if the item's class is not `pending` (and not stale), answers it and writes \
         nothing; a model verdict already recorded for the current classifier (even unparsed) \
         is never asked again. A reserved call without a recorded verdict is refused (Conflict) \
         to avoid paying again after a failed call or write. Otherwise checks the source's \
         daily budget (RetryAfter past it; the item stays pending), asks `urn:llm:<provider>:ask` (the pinned model, temperature \
         0, JSON), refuses an answer naming another model (Conflict), and records the model \
         verdict atom: the label, or an unparsed verdict with the raw text.",
        Verb::Sink,
        Some(judge_cap(&n)),
        inputs,
        &PLAIN_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-judge"), move |inv| {
        let q = q.clone();
        let budget_locks = budget_locks.clone();
        let item_locks = item_locks.clone();
        Box::pin(async move {
            let face = face(inv, &PLAIN_FACES)?;
            let item = item_of(inv, true)?;
            let source = item.source.clone().unwrap_or_default();
            // Lock the model atom's identity, independent of source or item IRI. Keep it
            // through both writes so a waiter re-reads the completed verdict before it
            // reserves any budget. Always take the item lock before the budget lock.
            let c = q.classifier();
            let classifier = c.digest();
            let item_lock = item_locks.for_key(&classifier, &item.digest);
            let _item_reservation = item_lock.lock().await;
            let g = gather(inv, &q, &item).await?;
            let reply = |change: &str, label: Option<&str>, by: &str| {
                let body = if face == JSON {
                    json!({"change": change, "class": label.unwrap_or("pending"), "digest": item.digest, "producer": by})
                        .to_string()
                } else {
                    format!("{change} {} {by}\n", label.unwrap_or("pending"))
                };
                answer(face, body)
            };
            if let Some(v) = decide(&g.verdicts).filter(|v| v.producer != Producer::Stale) {
                return Ok(reply("unchanged", v.label.as_deref(), v.producer.name()));
            }
            if g
                .verdicts
                .iter()
                .any(|v| v.producer == Producer::Model { generation: 0 })
            {
                return Ok(reply("unchanged", None, "unparsed"));
            }
            let reservation_path = atoms::reservation_path(&q.name, &classifier, &item.digest);
            if read_atom(inv, &reservation_path).await?.is_some() {
                return Err(Error::Conflict(format!(
                    "a model call for item {} was already reserved, but no verdict is recorded; \
                     refused another call (reservation <{reservation_path}>)",
                    item.digest
                )));
            }
            let (date, created) = now(inv)?;
            // Reserve under an async lock: no caller of this endpoint can observe the
            // same count before its predecessor's increment has been persisted. This is
            // endpoint-local; separate processes sharing atoms need storage-level CAS.
            let budget_lock = budget_locks.for_key(&source, &date);
            let reservation = budget_lock.lock().await;
            let used = spent(inv, &q, &source, &date).await?;
            if used >= q.budget {
                return Err(Error::RetryAfter {
                    delay: inv.now().map(|t| {
                        Duration::from_millis(86_400_000 - t.as_millis() % 86_400_000)
                    }),
                    detail: format!(
                        "source `{source}` has spent {used} of its {} model calls for {date}; the \
                         item stays pending",
                        q.budget
                    ),
                });
            }
            // Spend before asking: a call that fails or answers off-label is still spent.
            write_atom(
                inv,
                &atoms::spent_path(&q.name, &source, &date),
                &atoms::spent_turtle(&spent_name(&q.name, &source, &date), used + 1),
            )
            .await?;
            // Persist the item reservation before the paid side effect. Retain it on
            // failure: without a verdict we cannot know whether retrying would pay twice.
            let name = model_name(&q.name, &classifier, &item.digest);
            write_atom(
                inv,
                &reservation_path,
                &atoms::reservation_turtle(&reservation_path, &name, &created),
            )
            .await?;
            drop(reservation);
            let endpoint = format!("urn:llm:{}:ask", q.provider);
            let prompt = format!("{}\n\n{}", q.prompt, item.content);
            let system = instructions(&q);
            let rep = inv
                .issue(request(
                    Verb::Source,
                    &endpoint,
                    &[
                        ("prompt", &prompt),
                        ("system", &system),
                        ("model", &q.model),
                        ("temperature", "0"),
                        ("as", JSON),
                    ],
                )?)
                .await?;
            let envelope: Value = serde_json::from_slice(&rep.bytes)
                .map_err(|e| Error::Endpoint(format!("<{endpoint}> answered no JSON envelope: {e}")))?;
            let answered_by = envelope["model"].as_str().unwrap_or("");
            if answered_by != q.model {
                return Err(Error::Conflict(format!(
                    "<{endpoint}> answered as model `{answered_by}`, not the pinned `{}`: refused, \
                     no verdict recorded",
                    q.model
                )));
            }
            let text = envelope["text"].as_str().unwrap_or("");
            let label = parse_label(&q, text);
            let raw = if text.len() > MAX_RAW_BYTES {
                format!("({} bytes, b3:{})", text.len(), digest_hex(text.as_bytes()))
            } else {
                text.to_string()
            };
            let target = item.target();
            let prompt_iri = iri("prompt", &q.name);
            let atom = atoms::model_turtle(&ModelRecord {
                name: &name,
                target: &target,
                concept: label.map(|l| l.concept.as_str()),
                raw: if label.is_some() { None } else { Some(&raw) },
                digest: &item.digest,
                created: &created,
                endpoint: &endpoint,
                model: &q.model,
                prompt: &prompt_iri,
                prompt_digest: &c.prompt_digest,
            });
            write_atom(inv, &atoms::model_path(&q.name, &classifier, &item.digest), &atom).await?;
            if let Some(it) = &item.item {
                let path = atoms::model_last_path(&q.name, it);
                write_atom(inv, &path, &atoms::last_turtle(&path, it, &item.digest)).await?;
            }
            Ok(match label {
                Some(l) => reply("judged", Some(&l.name), "model"),
                None => reply("unparsed", None, "model"),
            })
        })
    })
    .with_description(d)
}

// --- the resolution -----------------------------------------------------------------------

fn correct_door(q: Arc<Question>) -> AsyncFnEndpoint {
    let n = q.name.clone();
    let mut inputs = vec![
        content_spec(),
        ArgSpec::new("label")
            .summary("the correct label")
            .class(xsd("string"))
            .one_of(q.labels.iter().map(|l| l.name.clone())),
        item_spec(),
    ];
    inputs.push(as_spec(&PLAIN_FACES));
    let d = describe(
        &format!("judge-{n}-correct"),
        &format!("Correct an item's class for `{n}`"),
        "The resolution. A CURATOR's correction (the product principal, a caller naming none, \
         or a curator the host named) writes the shared person atom AND trains the shared bayes \
         model with the same text and label; any other principal's correction writes only that \
         principal's own corridor and trains nothing, so one reader cannot move every reader's \
         scores. A correction outranks every other verdict, so it resolves a disagreement.",
        Verb::Sink,
        Some(correct_cap(&n)),
        inputs,
        &PLAIN_FACES,
    );
    AsyncFnEndpoint::new(format!("judge-{n}-correct"), move |inv| {
        let q = q.clone();
        Box::pin(async move {
            let face = face(inv, &PLAIN_FACES)?;
            let item = item_of(inv, false)?;
            let label_text = inv.inline_str("label")?;
            let label = q.label(label_text.trim()).ok_or_else(|| {
                invalid("label", format!("`{label_text}` is not one of the question's labels"))
            })?;
            let who = principal(inv, &q).to_string();
            let curator = q.is_curator(&who);
            let corridor = if curator { q.product.clone() } else { who.clone() };
            let (_, created) = now(inv)?;
            let target = item.target();
            let atom = atoms::person_turtle(
                &person_name(&q.name, &item.digest),
                &target,
                &label.concept,
                &item.digest,
                &created,
                &who,
            );
            write_atom(inv, &atoms::person_path(&corridor, &q.name, &item.digest), &atom).await?;
            if let Some(it) = &item.item {
                let path = atoms::person_last_path(&corridor, &q.name, it);
                write_atom(inv, &path, &atoms::last_turtle(&path, it, &item.digest)).await?;
            }
            let trained = match (&q.bayes, curator) {
                (Some(model), true) => {
                    let train = format!("urn:bayes:{model}:train");
                    match inv
                        .issue(request(
                            Verb::Sink,
                            &train,
                            &[("content", &item.content), ("label", &label.name)],
                        )?)
                        .await
                    {
                        Ok(_) => "trained",
                        // A label the two-way bayes model does not have teaches it nothing.
                        Err(Error::InvalidArgument { name, .. }) if name == "label" => "not-a-bayes-label",
                        Err(e) => return Err(e),
                    }
                }
                (Some(_), false) => "reader-not-trained",
                (None, _) => "no-bayes",
            };
            let view = if curator { "shared" } else { "personal" };
            let body = if face == JSON {
                json!({"bayes": trained, "digest": item.digest, "label": label.name, "principal": who, "view": view})
                    .to_string()
            } else {
                format!("corrected {} {view} bayes {trained}\n", label.name)
            };
            Ok(answer(face, body))
        })
    })
    .with_description(d)
}
