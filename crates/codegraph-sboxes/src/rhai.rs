//! Rhai mock environment.
//!
//! Mock contract: a `*.rhai` file under a configured mock dir declares functions
//! named after the callee, taking a single array argument and returning an `i64`
//! (abstract value), e.g.:
//!
//! ```rhai
//! // sandbox/mocks/order.rhai
//! fn validate_order(args) { 1 }
//! fn insert_order(args)   { 42 }
//! ```
//!
//! The sandbox runtime dispatches every external/unresolved call through
//! [`RhaiMockLib::call`]; a missing mock returns `Err(MockError::NotFound)` and
//! the runtime records the miss (still returning `0`) so the caller can see what
//! was not mocked.
//!
//! Per-call mock configuration is supported via [`RhaiMockLib::register`]
//! (name → Rhai body/full `fn` source). Inline mocks override file mocks of the
//! same name — used by the MCP sandbox tool so a caller can mock specific
//! functions instead of seeing a missing-mock error.

use rhai::{Array, AST, Dynamic, Engine, Scope};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Why a mock could not run.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    /// No `fn <name>` found in any loaded mock file.
    #[error("no rhai mock for `{0}`")]
    NotFound(String),
    /// The mock script itself failed.
    #[error("rhai mock `{0}` failed: {1}")]
    Script(String, String),
}

/// Convenience alias.
pub type MockResult<T> = Result<T, MockError>;

/// A loaded set of Rhai mocks (one shared `Engine` + one merged `AST`).
///
/// Loaded once per sandbox; reused across runs (each run gets its own `Scope`).
pub struct RhaiMockLib {
    engine: Engine,
    ast: AST,
    names: HashSet<String>,
    /// Per-name override mocks (registered at run-request time). Kept separate
    /// from `ast` so an inline mock replaces a file mock deterministically.
    inline: HashMap<String, AST>,
}

impl RhaiMockLib {
    /// Load all `*.rhai` files under `dirs` (relative to `root`).
    pub fn load(root: &Path, dirs: &[String]) -> Self {
        let engine = Engine::new();
        let mut ast = AST::empty();
        let mut names = HashSet::new();

        for dir in dirs {
            let abs = root.join(dir);
            let Ok(entries) = std::fs::read_dir(&abs) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("rhai") {
                    continue;
                }
                if let Ok(script) = std::fs::read_to_string(&path) {
                if let Ok(compiled) = engine.compile(&script) {
                    for sig in compiled.iter_functions() {
                        names.insert(sig.name.to_string());
                    }
                    ast = ast.merge(&compiled);
                }
                }
            }
        }
        Self {
            engine,
            ast,
            names,
            inline: HashMap::new(),
        }
    }

    /// Load mock files, then overlay inline per-function mocks (`name → rhai
    /// source`). Inline mocks win over file mocks with the same name.
    pub fn load_with_mocks(root: &Path, dirs: &[String], inline: &[(String, String)]) -> Self {
        let mut lib = Self::load(root, dirs);
        for (name, src) in inline {
            let _ = lib.register(name, src); // bad source: skip, `call` reports it
        }
        lib
    }

    /// Register (or replace) one mock by name. `src` is either a full
    /// `fn <name>(args) { … }` script or just the function body, which is
    /// wrapped into `fn <name>(args) { <src> }`.
    pub fn register(&mut self, name: &str, src: &str) -> MockResult<()> {
        let script = if src.trim_start().starts_with("fn ") {
            src.to_string()
        } else {
            format!("fn {name}(args) {{ {src} }}")
        };
        let compiled = self
            .engine
            .compile(&script)
            .map_err(|e| MockError::Script(name.to_string(), e.to_string()))?;
        self.names.insert(name.to_string());
        self.inline.insert(name.to_string(), compiled);
        Ok(())
    }

    /// Empty mock library (every call misses).
    pub fn empty() -> Self {
        Self {
            engine: Engine::new(),
            ast: AST::empty(),
            names: HashSet::new(),
            inline: HashMap::new(),
        }
    }

    /// Whether a mock for `name` is loaded (file or inline).
    pub fn has(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Invoke the mock for `name` with abstract `args` (an array, per the
    /// contract above). Returns the mock's `i64` result. Inline mocks are
    /// preferred; fall back to the merged file AST.
    pub fn call(&mut self, name: &str, args: &[i64]) -> MockResult<i64> {
        let mut scope = Scope::new();
        let arr: Array = args.iter().copied().map(Dynamic::from).collect();
        let arg = Dynamic::from(arr);
        if let Some(ast) = self.inline.get(name) {
            return self
                .engine
                .call_fn::<i64>(&mut scope, ast, name, (arg,))
                .map_err(|e| MockError::Script(name.to_string(), e.to_string()));
        }
        if !self.names.contains(name) {
            return Err(MockError::NotFound(name.to_string()));
        }
        self.engine
            .call_fn::<i64>(&mut scope, &self.ast, name, (arg,))
            .map_err(|e| MockError::Script(name.to_string(), e.to_string()))
    }
}

impl Default for RhaiMockLib {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_call() {
        let dir = std::env::temp_dir().join("codegraph-sboxes-rhai-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("order.rhai"),
            "fn validate_order(args) { 7 }\nfn insert_order(args) { args[0] * 2 }\n",
        )
        .unwrap();
        let mut lib = RhaiMockLib::load(
            std::env::temp_dir().as_path(),
            &["codegraph-sboxes-rhai-test".to_string()],
        );
        assert!(lib.has("validate_order"));
        assert!(!lib.has("nope"));
        assert_eq!(lib.call("validate_order", &[]).unwrap(), 7);
        assert_eq!(lib.call("insert_order", &[21]).unwrap(), 42);
        assert!(matches!(
            lib.call("nope", &[]),
            Err(MockError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Inline mock: body-only được wrap thành `fn <name>(args)`, override file
    /// mock cùng tên, và chưa có trong file vẫn chạy được.
    #[test]
    fn inline_mocks_override_and_add() {
        let dir = std::env::temp_dir().join("codegraph-sboxes-rhai-inline");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("order.rhai"),
            "fn get_stock(args) { 1 }\nfn send_email(args) { 2 }\nfn ship(args) { 3 }\n",
        )
        .unwrap();
        let inline = vec![
            ("get_stock".to_string(), "99".to_string()),
            ("insert_order".to_string(), "args[0] * 10".to_string()),
            (
                "send_email".to_string(),
                "fn send_email(args) { args.len() }".to_string(),
            ),
        ];
        let mut lib = RhaiMockLib::load_with_mocks(
            std::env::temp_dir().as_path(),
            &["codegraph-sboxes-rhai-inline".to_string()],
            &inline,
        );
        // Inline override file mock.
        assert_eq!(lib.call("get_stock", &[]).unwrap(), 99);
        // Body-only inline.
        assert_eq!(lib.call("insert_order", &[4]).unwrap(), 40);
        // Full-fn inline (with different body) override file mock.
        assert_eq!(lib.call("send_email", &[]).unwrap(), 0);
        // Không inline → rơi về file mock.
        assert_eq!(lib.call("ship", &[]).unwrap(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Inline mock source lỗi → register trả Script error, không crash.
    #[test]
    fn bad_inline_mock_reports_script_error() {
        let mut lib = RhaiMockLib::empty();
        assert!(lib.register("oops", "let x = ").is_err());
        assert!(!lib.has("oops"));
    }
}