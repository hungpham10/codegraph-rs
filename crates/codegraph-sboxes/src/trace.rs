//! Observable behavior trace produced by a sandbox run.
//!
//! Piece 1 records the *unobservable black-box* of a function as the sequence of
//! mocked calls it makes. The `Trace` below is the "observed behavior" — the
//! input later Pieces (spec/invariant compare) will verify against.

use serde::{Deserialize, Serialize};

/// Which kind of control-flow marker drove a condition decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CondKind {
    If,
    Loop,
    Switch,
}

impl CondKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CondKind::If => "if",
            CondKind::Loop => "loop",
            CondKind::Switch => "switch",
        }
    }
}

/// One dispatched call to an (external/unresolved) callee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockEvent {
    /// Callee name (resolved symbol name or raw call name).
    pub callee: String,
    /// Abstract arg values passed by the compiled code.
    pub args: Vec<i64>,
    /// Value returned by the mock (or fallback `0`).
    pub result: i64,
}

/// One condition decision made by the policy during a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CondEvent {
    pub kind: CondKind,
    /// Index into the module's condition table.
    pub idx: u64,
    pub result: bool,
}

/// One entry in the interleaved run log (the *order* between a condition
/// decision and the mock call it gates is observable behavior).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TraceEvent {
    Mock(MockEvent),
    Cond(CondEvent),
}

/// The observed behavior of a run: ordered mock calls + control-flow decisions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trace {
    pub mocks: Vec<MockEvent>,
    pub conds: Vec<CondEvent>,
    /// Interleaved log of both, in execution order.
    pub events: Vec<TraceEvent>,
    /// Callee names that were dispatched but had no mock (file or inline) — the
    /// run fell back to `0`. Lets a caller see what still needs to be mocked.
    pub missing: Vec<String>,
}

impl Trace {
    pub fn mock_names(&self) -> Vec<&str> {
        self.mocks.iter().map(|m| m.callee.as_str()).collect()
    }

    /// Count how many times a mock was invoked.
    pub fn count(&self, callee: &str) -> usize {
        self.mocks.iter().filter(|m| m.callee == callee).count()
    }

    /// The invocation order, as a list of "kind/name" tokens.
    pub fn sequence(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.events {
            match e {
                TraceEvent::Cond(c) => {
                    out.push(format!("{}:{}", c.kind.as_str(), if c.result { 1 } else { 0 }));
                }
                TraceEvent::Mock(m) => out.push(format!("call:{}", m.callee)),
            }
        }
        out
    }
}