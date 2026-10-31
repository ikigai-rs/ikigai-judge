//! `ikigai-judge`: judgments as resources.
//!
//! Scaffold. The module will bind `urn:iki:judge:*`: an item's class for a question is the
//! first recorded or computed verdict that answers, in order person ▸ mapping ▸ model ▸
//! bayes (when confident) ▸ the value `pending`. Only what cannot be recomputed is stored
//! (a model call, a person's correction); mapping and bayes are pure compositions. Where
//! two producers disagree, `urn:iki:judge:disagreements:{q}` says so, and a correction
//! resolves it. The design is ledger #2677.

#![forbid(unsafe_code)]
