# Accessibility

What `ikigai-judge` emits that people read, what of it is this repository's and what is a
model's, the rules that output follows, how it is tested, and how to report a barrier. It is not
a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA for the HTML this repository writes (the `why` page); its other output is text |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-judge/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-judge/security/advisories/new) |
| Last reviewed | 2026-11-06 |

## What it emits that people read

All of it is in `src/endpoints.rs`.

- **An item's class** (`urn:iki:judge:class:{q}:{digest}`): one line, the class and the
  producer that decided (`spam model`, `ham stale <old digest>`, or `pending`); also JSON.
- **Why** (`urn:iki:judge:why:{q}:{digest}`): the class, the digest, a `stale:` sentence when
  the verdict is on the item's previous text, every verdict ranked with its producer, label and
  provenance (`key=value` pairs), bayes's clues, and, while the item has no verdict from the
  current classifier, one sentence on its reserved model call (`model call failed at <time>:
  <kind>: <message>; charged: <yes|unknown>; retry at <time>`, `model call cleared by
  <principal> at <time>; the judge may ask again`, or `model call reserved at <time>, no
  verdict or failure recorded; a curator can clear it`), followed by the reservation's name.
  Four faces: text (`why_text`, the deciding
  verdict marked `*` and ending `(decided)`), **an HTML page** (`why_html`: `<html lang="en">`,
  a viewport meta, one `<main>` holding an `<h1>` naming the class, the model call's sentence
  in a `<p>`, an ordered list of verdicts under the heading `Verdicts, in rank order` with the
  deciding one ending `(decided)`, and a list of clues under `Clues from bayes`, each
  `probability <p> <token>`), Turtle and JSON.
- **Lists and counts**: `pending` (one `<digest> <pending|stale|unparsed|failed|reserved>
  <item>` line each; a `failed` line ends `model call failed, retry at <time>`, a `reserved`
  line `model call reserved, no verdict or failure recorded; a curator can clear it`),
  `disagreements`, `spent <n> of <budget>`, the question, prompt and classifier faces, and a
  reservation (`reservation`, `attempt`, `reserved` lines, then the call's sentence or
  `verdict recorded <name>`).
- **Write outcomes** from `judge`, `correct` and a curator's clear (`cleared attempt <n>; the
  judge may ask again`), and **refusals**: an input outside its form (a
  face, a label not in the question, a `source` that is not a segment, a `date` not
  `YYYY-MM-DD`, an `items` array that does not hold), a text past its bound (`refused, not
  truncated`), a digest that is not the text's, a day's budget spent (`the item stays
  pending`), a model envelope naming another model, no clock configured, an item whose model
  call failed (refused until its retry time, which the message names, or a curator's clear),
  an item with a reserved model call but no outcome recorded (refused to avoid paying again;
  the message says a curator can clear it), a clear by someone who is not a curator, and a
  clear of an item whose verdict is recorded.

**A failed call's error message is the failing endpoint's** (usually the model provider's,
wrapped in ikigai-core's `Display`): it is recorded and shown as written, newlines as `\n` on
the text face, at most 4096 bytes (past that, its length and digest instead).

**A model's raw answer is the model's, not this repository's.** When an answer names no label,
it is kept as an unparsed verdict and shown as written in the `why` provenance
(`unparsed=…`, newlines as `\n`, at most 4096 bytes). The prompt is the host's, the item's text
is the caller's, and the clue tokens come from that text through `ikigai-bayes`. Not this
repository's either: the sentence each error kind is wrapped in (ikigai-core's `Display`), and
any page a host puts the text faces in.

## The rules that output follows

- The class is a word and comes first on the text faces and the page; an item nothing has judged says
  `pending`, and a verdict on old text says `stale`.
- A bound refuses and says so; nothing is cut short. An invalid input is named, with the form it
  takes.
- The HTML page says its language (`lang="en"`), has a viewport meta, a `<title>`, one
  `<main>` and one `<h1>` naming the class, a heading before each list, the verdicts in an
  `<ol>` in rank order, and no style or script. Every value on it is escaped with
  `ikigai_fn::html::html_text`, the one escaping implementation, not a private one.
- Which verdict decided is said in words, `(decided)`, on the page and the text face, never by
  bold or a mark alone.
- No color is written.

## How it is tested

- `tests/judge.rs`: `why_has_text_turtle_and_html_faces` (the page starts `<!doctype html>`, the
  text face starts `class `), `a_text_past_the_bound_is_refused_not_truncated`,
  `past_the_budget_the_judge_refuses_until_tomorrow_and_the_item_stays_pending`,
  `a_digest_that_is_not_the_texts_is_refused`,
  `a_failed_verdict_write_never_pays_for_the_item_again` (the refusal names the retry time),
  and the producer tests that check each class line.
- `tests/retry.rs`: the `why` text sentence and the `pending` line for a failed call, word
  for word (`a_failed_call_is_reported_refused_until_its_retry_time_then_judged_once`), the
  same sentence on the HTML page and in JSON and Turtle (`every_why_face_says_the_call_failed`),
  the cleared and no-outcome sentences, and the refusals before the retry time and for a
  non-curator's clear.
- `tests/judge.rs`:
  `the_why_page_has_a_language_one_main_a_viewport_headings_and_says_which_verdict_decided`
  checks the page's shape (`lang`, the viewport, one `<main>`, one `<h1>`, both headings, one
  `(decided)` and no `<strong>`, a clue's number labeled `probability`) and the text face's one
  `(decided)`.
- Nothing checks the page beyond those strings: no HTML validator. **No automated accessibility
  checker, browser or assistive technology has been used on this output.**

## Known limitations

Found by reading the source on 2026-10-31:

- A clue's `probability` does not say which label it is the probability of.
- A verdict's provenance is a run of `key=value` pairs, and digests are long hex strings.
- All text is English, with no way to choose another language.

## Contributing

Read this file before changing any output listed above: the faces, the page or the refusals.
Keep meaning in words, never in bold, color or position alone. Say in the pull request what was
tested, with what, and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-judge/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your browser or terminal and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-judge/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-01 | A failed model call: its sentence in `why`, the `failed` and `reserved` lines in `pending`, the reservation resource, a curator's clear and their refusals. |
| 2026-11-06 | The `why` page: `lang`, a viewport, one `<main>`, a heading on each list, the deciding verdict said `(decided)` (also on the text face) instead of bold, a clue's number labeled `probability`, escaping through `ikigai_fn::html`. Its shape is tested. |
