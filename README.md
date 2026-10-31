# ikigai-judge

Judgments as resources for [ikigai](https://github.com/ikigai-rs): classify items (feed
entries, contact-form enquiries, anything with text) as **recorded verdicts**, and say where
the producers disagree.

An item's class for a question is the first verdict that answers, in this order:

1. a **person's** correction;
2. a per-source **mapping** (a pure rule);
3. a recorded **model** verdict;
4. a **bayes** score, once it is confident;
5. otherwise the value `pending`.

Only what cannot be recomputed is stored: a model call and a person's correction. Mapping
and bayes are pure compositions, cached on their inputs' threads. When the deciding verdict
and another producer disagree, `urn:iki:judge:disagreements:{q}` lists the item with both
answers and their provenance, and a correction resolves it.

**Status:** scaffold. The design is ledger #2677; the module is not built or published yet.

## License

MIT or Apache-2.0, at your option.
