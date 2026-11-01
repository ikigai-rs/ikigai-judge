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
| Last reviewed | 2026-10-31 |

## What it emits that people read

All of it is in `src/endpoints.rs`.

- **An item's class** (`urn:iki:judge:class:{q}:{digest}`): one line, the class and the
  producer that decided (`spam model`, `ham stale <old digest>`, or `pending`); also JSON.
- **Why** (`urn:iki:judge:why:{q}:{digest}`): the class, the digest, a `stale:` sentence when
  the verdict is on the item's previous text, every verdict ranked with its producer, label and
  provenance (`key=value` pairs), and bayes's clues. Four faces: text (`why_text`, the deciding
  verdict marked `*`), **an HTML page** (`why_html`: an `<h1>` naming the class, an ordered
  list of verdicts with the deciding one in `<strong>`, a list of clues), Turtle and JSON.
- **Lists and counts**: `pending` (one `<digest> <pending|stale|unparsed> <item>` line each),
  `disagreements`, `spent <n> of <budget>`, and the question, prompt and classifier faces.
- **Write outcomes** from `judge` and `correct`, and **refusals**: an input outside its form (a
  face, a label not in the question, a `source` that is not a segment, a `date` not
  `YYYY-MM-DD`, an `items` array that does not hold), a text past its bound (`refused, not
  truncated`), a digest that is not the text's, a day's budget spent (`the item stays
  pending`), a model envelope naming another model, no clock configured.

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
- The HTML page has a `<title>` and one `<h1>` naming the class, the verdicts in an `<ol>` in
  rank order, every value from a verdict or a token escaped, and no style or script.
- No color is written.

## How it is tested

- `tests/judge.rs`: `why_has_text_turtle_and_html_faces` (the page starts `<!doctype html>`, the
  text face starts `class `), `a_text_past_the_bound_is_refused_not_truncated`,
  `past_the_budget_the_judge_refuses_until_tomorrow_and_the_item_stays_pending`,
  `a_digest_that_is_not_the_texts_is_refused`, and the producer tests that check each class line.
- Nothing tests the HTML page's structure beyond its first line. **No automated accessibility
  checker, browser or assistive technology has been used on this output.**

## Known limitations

Found by reading the source on 2026-10-31:

- The `why` page's `<html>` has no `lang`, there is no `<main>`, and there is no viewport meta,
  so it is not laid out for a phone.
- On the `why` page, which verdict decided is shown only by bold (`<strong>`), which a screen
  reader does not announce; on the text face only by a `*`. Neither says "decided" in words.
- The page's two lists have no heading saying what they are, and each clue is a bare
  probability and a token, with nothing saying what the number means.
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
