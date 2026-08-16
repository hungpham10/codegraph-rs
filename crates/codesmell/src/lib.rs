//! CodeSmell — team convention linter for maintainable, LLM-friendly code.
//!
//! CodeSmell consumes the code facts produced by CodeGraph (symbols, kinds,
//! dependencies, call graph) and evaluates them against a team's engineering
//! policy. It runs like a linter (`codesmell check`): the LLM reads the
//! conventions pack before writing code (`codesmell guide`) and fixes every
//! reported violation afterwards.

pub mod engine;
pub mod glob;
pub mod guide;
pub mod index;
pub mod policy;
pub mod rules;
