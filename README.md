# ikigai-judge

Judgments as resources for [ikigai](https://github.com/ikigai-rs): classify items (feed
entries, contact-form enquiries, anything with text) as **recorded verdicts**, and say where
the producers disagree.

An item's class for a question is the first verdict that answers, in this order:

1. a **person's** correction: the caller's own corridor, then the shared (product) view;
2. a per-source **mapping** (a pure rule);
3. a recorded **model** verdict: the current classifier's, then each previous classifier's;
4. a **stale** verdict: the recorded verdict on the item's PREVIOUS text, after an edit;
5. a **bayes** score, once it is confident (not `unsure`);
6. otherwise the value `pending`.

A recorded verdict outranks a computed one, so a model verdict keeps deciding when bayes later
becomes confident the other way; that case is a **disagreement**, listed by
`urn:iki:judge:disagreements:{q}` with both answers and their provenance, and a **correction**
resolves it.

**Status:** 0.1.0, not published. The design is ledger #2677.

## Only what cannot be recomputed is stored

Mapping and bayes are pure, so the class, the why, the pending list and the disagreements are
cacheable compositions over them. A model call and a person's correction are not, so they are
**atoms**: Turtle files on a host state root that the host mounts with ikigai-fs
`cacheable_space`. Each atom is its own file, so its own golden thread: a write through the
kernel cuts exactly the compositions that read it, and a bayes training cuts exactly those that
scored on that model. Nothing here writes ikigai-store, where one write cuts every cached read.

```text
model/{q}/{classifier}/{digest}.ttl      a model verdict, written once (the judge)
model/{q}/last/{item}.ttl                the item's last digest the judge judged
model/{q}/spent/{source}/{date}.ttl      model calls spent that day
person/{principal}/{q}/{digest}.ttl      a person's latest correction (their corridor)
person/{principal}/{q}/last/{item}.ttl   the item's last digest that person corrected
```

`{item}` and `{principal}` are the bare-hex BLAKE3 digests of the item's and principal's IRIs.
The split is the **path ACL** the host grants on the state root: the judge's worker holds
`urn:cap:fs:write:<root>/model`, a reviewer `urn:cap:fs:write:<root>/person/<their key>`, and a
curator the product principal's key. Sub-requests run under the caller's capability, so this
is a second floor beside the judge's own capabilities.

## The resources

`{q}` is a question the host configured; `{digest}` is the bare-hex BLAKE3 digest of the judged
text (never a `b3:` colon inside a name). Every door is bound per question, so each declares its
own question's capability.

| resource | verb | what | capability |
|---|---|---|---|
| `urn:iki:judge:question:{q}` | Source | the labels as a SKOS concept scheme, the prompt IRI, provider, PINNED model, the bayes model, the daily budget, previous classifiers (Turtle, text) | `urn:cap:judge:read:{q}` |
| `urn:iki:judge:prompt:{q}` | Source | the prompt text | read |
| `urn:iki:judge:classifier:{q}` | Source | the current classifier: a digest over (provider, model, prompt digest, labels) | read |
| `urn:iki:judge:model:{q}:{classifier}:{digest}` | Source | the model verdict atom; NotFound until written | read |
| `urn:iki:judge:person:{q}:{digest}` | Source | the latest correction in the CALLER's corridor (a caller naming no principal reads the shared view) | read |
| `urn:iki:judge:class:{q}:{digest}` | Source | the class and the producer that decided (`spam model`, `ham stale <old digest>`, `pending`); a READ, never writes | read |
| `urn:iki:judge:why:{q}:{digest}` | Source | every verdict that exists, ranked, each with its provenance, and bayes's clues; text, Turtle, HTML, JSON | read |
| `urn:iki:judge:pending:{q}:{source}` | Source | of the items given, those `pending`, `stale` (queued for re-judging) or `unparsed` (needs a person) | read |
| `urn:iki:judge:disagreements:{q}` | Source | **the signal**: of the items given, every one where the deciding verdict and another producer disagree | read |
| `urn:iki:judge:spent:{q}:{source}:{date}` | Source | `spent <n> of <budget>` | read |
| `urn:iki:judge:judge:{q}` | Sink | judge one item with the pinned model (below) | `urn:cap:judge:judge:{q}` |
| `urn:iki:judge:correct:{q}` | Sink | **the resolution** (below) | `urn:cap:judge:correct:{q}` |

`class`, `why` and `judge` take the item as `content` (its text, at most 64 KiB, piped), with
an optional `item` (its IRI) and `source`; a `{digest}` that is not the content's is refused.
`pending` and `disagreements` take `items`, a JSON array of at most 1000
`{"content", "item"?, "source"?}` objects, so a source's feed is piped in.

### judge

Idempotent. If the class is not `pending` (and not stale), it answers the class and writes
nothing. A verdict already recorded for the current classifier, even an unparsed one, is never
asked again. Otherwise it checks the source's daily budget (past it: `OverBudget`, the item
stays `pending`, nothing is called and nothing is truncated), spends one call, asks
`urn:llm:<provider>:ask` (the pinned model, temperature 0, the label set in the system prompt,
the JSON envelope), and:

- an envelope naming **another model** is refused (`Conflict`), and nothing is recorded;
- an answer naming a label from the **closed set** (`{"label": "spam"}` or the bare label) is
  recorded as the model verdict;
- **anything else** is recorded as an unparsed verdict with its raw text (`oa:bodyValue`): it
  was spent, it answers nothing, and the judge does not ask again.

The model verdict as stored is an `oa:Annotation` with `oa:motivatedBy oa:classifying`,
`oa:hasTarget` the item, `oa:hasBody` the label concept, `dcterms:identifier` the content hash,
`dcterms:created`, `prov:wasAssociatedWith` the provider endpoint and `prov:wasGeneratedBy` the
run, which is a `prov:Activity` recording the model id (`schema:instrument`), the prompt it
used (`prov:used`) and the prompt digest (`schema:version`). Published terms only.

A new model or prompt is a new classifier, so a new name: old verdicts stay readable. List the
old classifier with `Question::previous` and its verdicts keep answering after the current
one's.

### correct

A **curator** (the product principal, a caller naming no principal such as the host, or a
principal the host names with `Question::curator`) writes the SHARED person atom and trains the
shared bayes model with the same text and label: a fork of two Sinks. Any **other** principal's
correction writes only their own corridor and trains nothing, so one reader cannot move every
reader's scores. A correction outranks everything, so it resolves a disagreement, which then
drops out of `disagreements`. The person atom is the same annotation shape, with
`dcterms:creator` the principal.

### Stale verdicts after an edit

When an item's text changes, its new digest has no verdict. The class then answers the
PREVIOUS text's verdict, marked stale (`why` says so), rather than `pending`, and `pending`
lists the item as `stale`, queued for re-judging; it spends budget only when the judge runs.

**How the previous digest is found:** by the item's IRI. Each `judge` and `correct` that names
an `item` records that item's last judged digest (`model/{q}/last/{item}.ttl`, and
`person/{principal}/{q}/last/{item}.ttl` in the corrector's corridor). The class reads the
caller's own record, then the shared one, then the judge's, and the first whose digest differs
from the current text and has a verdict answers as stale. An item read without an `item` IRI
has no previous text.

## Hosting it

```rust,ignore
let contact = ikigai_judge::Question::new(
    "contact", ["spam", "ham"], PROMPT, "ollama", "llama3.2:3b", "urn:example:product",
)?
.bayes("contact")          // urn:bayes:contact:* (ikigai-bayes 0.3, the same two labels)
.budget(50);               // calls per source per day; starts at 0, set from measured volume
let judge = ikigai_judge::space([contact]);
// beside it: ikigai_fs::cacheable_space(state_root), ikigai_bayes::space(&bayes),
// ikigai_llm's urn:llm:<provider>:ask, and a clock (Kernel::with_clock): verdicts record time.
```

The judge links none of fs, bayes or llm: it reaches each through the kernel by name, so a
host composes whichever it has. The space is host-named (the host chose the questions).

## Bounds

Every bound refuses; none truncates.

| what | bound | past it |
|---|---|---|
| an item's text | 64 KiB (`MAX_TEXT_BYTES`, bayes's message bound) | `InvalidArgument` on `content` |
| items per `pending` / `disagreements` call | 1000 (`MAX_ITEMS`) | `InvalidArgument` on `items` |
| model calls per source per day | the question's budget | `OverBudget` |
| raw answer kept in an unparsed verdict | 4096 bytes | kept as its length and digest instead |

The only recursive parse of request input is `items` and the model's envelope, by
`serde_json`, which refuses nesting past 128 levels.

⚠ The budget check reads then writes the day's count; two judges racing on one source can
both pass the last call. Run one worker per question (the worker loop does).

## Tests

`tests/judge.rs` drives a host with ikigai-fs over a scratch root, an in-memory bayes model and
a FAKE `urn:llm:fake:ask` (never a real model): each producer alone and in combination, a
model-vs-confident-bayes disagreement that leaves the model deciding and a correction that
resolves it and trains bayes, a mapping-vs-model and a stale-vs-bayes disagreement, a stale
verdict after an edit and its re-judging, two principals' corrections that do not see each
other and a reader correction that leaves the shared model digest unchanged, the budget,
an unparsed answer that is never retried, a wrong-model envelope, and the caching (measured
with `Kernel::is_cached`). `tests/conformance.rs` runs
[`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance) 0.12 over every door:
zero findings, nothing unprobed. CI also lints the lib on `wasm32-unknown-unknown`.

## Accessibility

The text faces, the `why` HTML page and the refusals this crate writes, the rules they follow, how they are tested and their known limitations are in [ACCESSIBILITY.md](ACCESSIBILITY.md). Report a barrier with an issue carrying the `accessibility` label.

## License

MIT or Apache-2.0, at your option.
