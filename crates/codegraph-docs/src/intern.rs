use std::collections::HashMap;

/// String interner: maps a raw string to a stable `u64` id and back.
///
/// Keeps `DocToken` payloads small (≤ 56 bits) and avoids storing `&str`
/// inside the radix key.
#[derive(Debug, Default)]
pub struct Interner {
    strings: HashMap<String, u64>,
    reverse: Vec<String>,
    next_id: u64,
}

impl Interner {
    pub fn new() -> Self {
        Self {
            strings: HashMap::new(),
            reverse: Vec::new(),
            next_id: 1,
        }
    }

    /// Khôi phục interner từ danh sách string đã persist (id = vị trí + 1).
    pub fn with_strings(strings: Vec<String>) -> Self {
        let mut this = Self::new();
        for s in strings {
            this.intern(s);
        }
        this
    }

    /// Toàn bộ string theo thứ tự id — dùng để persist.
    pub fn strings(&self) -> Vec<String> {
        self.reverse.clone()
    }

    /// Return the interned id for `s`, inserting if absent.
    pub fn intern(&mut self, s: String) -> u64 {
        if let Some(&id) = self.strings.get(&s) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.reverse.push(s.clone());
        self.strings.insert(s, id);
        id
    }

    pub fn get(&self, s: &str) -> Option<u64> {
        self.strings.get(s).copied()
    }

    pub fn resolve(&self, id: u64) -> Option<&str> {
        self.reverse.get(id as usize).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
