//! Quy kết quả RSS về **đúng cấu trúc dữ liệu nào** đang nuốt RAM.
//!
//! [`super::meminfo`] đo tổng RSS của process — nhưng con số đó không bảo được
//! `HashMap` nào cần sửa. Module này cộng **deep size** từng cấu trúc trong
//! `GraphIndex` để so `Σ accounted` với `RSS thực`.
//!
//! ## Độ chính xác
//!
//! - **Exact**: heap của `String`/`Vec` (dùng `capacity()` — đúng bytes đã cấp).
//! - **Ước lượng**: bucket array của `HashMap` dùng công thức hashbrown
//!   `capacity() × (size_of::<(K, V)>() + 1)` (1 byte control mỗi slot).
//! - **Không tính**: `HashMap` bên trong `Annotation::args` (nhỏ, và capacity
//!   không đọc được từ `&HashMap`), cùng overhead của allocator (jemalloc/malloc
//!   thường ~10–20% bytes đã cấp). Vì vậy `Σ accounted` **luôn nhỏ hơn RSS
//!   thực** — đó là bình thường, không phải bug.
//!
//! Dùng để **so tương đối** (cấu trúc nào phình theo quy mô), không dùng làm
//! ngưỡng cứng.

use std::collections::HashMap;
use std::hash::Hash;

use codegraph_core::{Annotation, CallSite, EdgeMeta, FileInfo, Symbol};

/// Một cấu trúc: số phần tử + bytes đã cấp (fixed + heap).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructureMem {
    /// Số phần tử (entry của map, phần tử của vec).
    pub entries: u64,
    /// Bytes cố định: bucket của `HashMap` hoặc `len × size_of::<T>()` của `Vec`.
    pub fixed_bytes: u64,
    /// Bytes cấp động: `String`/`Vec` heap bên trong phần tử.
    pub heap_bytes: u64,
}

impl StructureMem {
    pub fn total_bytes(&self) -> u64 {
        self.fixed_bytes.saturating_add(self.heap_bytes)
    }
}

/// Bytes của `String` (đã cấp trên heap).
///
/// Nhận `&String` chứ không phải `&str`: `str` không có `capacity()`.
#[allow(clippy::ptr_arg)] // xem `vec_bytes` — cần `capacity()` của `String`.
#[inline]
fn str_bytes(s: &String) -> u64 {
    s.capacity() as u64
}

/// Bytes của `Option<String>`.
#[inline]
fn opt_str_bytes(s: &Option<String>) -> u64 {
    s.as_ref().map_or(0, |x| x.capacity() as u64)
}

/// Bytes của `Vec<u64>`: buffer + phần tử.
#[allow(clippy::ptr_arg)] // xem `vec_bytes` — cần `capacity()` của `Vec`.
#[inline]
fn u64_vec_bytes(v: &Vec<u64>) -> u64 {
    (v.capacity() * size_of::<u64>()) as u64
}

/// Bucket array của `HashMap` theo công thức hashbrown.
#[inline]
fn map_bucket_bytes<K: Hash, V>(map: &HashMap<K, V>) -> u64 {
    // `(size_of::<(K, V)>() + 1)` — +1 là byte control/sentinel mỗi slot.
    let slot = size_of::<(K, V)>() as u64 + 1;
    (map.capacity() as u64).saturating_mul(slot)
}

/// Bytes của một `Vec<T>` (buffer đã cấp).
// `&Vec<T>` thay vì `&[T]`: `capacity()` là method riêng của `Vec`, cần biết
// bytes **đã cấp** chứ không phải `len` — mà slice không đọc được.
#[allow(clippy::ptr_arg)]
#[inline]
fn vec_bytes<T>(v: &Vec<T>) -> u64 {
    (v.capacity() * size_of::<T>()) as u64
}

// ── Deep size từng phần tử ──

/// Deep size của `Symbol` (chưa tính slot trong HashMap).
pub fn symbol_heap(sym: &Symbol) -> u64 {
    let annotations: u64 = sym
        .annotations
        .iter()
        .map(annotation_heap)
        .sum::<u64>()
        + vec_bytes::<Annotation>(&sym.annotations);
    str_bytes(&sym.name)
        + str_bytes(&sym.file)
        + opt_str_bytes(&sym.type_name)
        + opt_str_bytes(&sym.signature)
        + opt_str_bytes(&sym.doc)
        + annotations
}

/// Deep size của `Annotation` (chưa tính buffer `Vec` chứa nó).
fn annotation_heap(a: &Annotation) -> u64 {
    // `args` là HashMap — không đọc được capacity từ `&HashMap`, bỏ qua.
    str_bytes(&a.name)
}

/// Deep size của `CallSite` (chưa tính slot trong HashMap).
pub fn call_site_heap(site: &CallSite) -> u64 {
    str_bytes(&site.call_name)
        + opt_str_bytes(&site.condition)
        + vec_bytes::<String>(&site.arg_exprs)
        + site.arg_exprs.iter().map(str_bytes).sum::<u64>()
}

/// Deep size của `EdgeMeta` (chưa tính slot trong HashMap).
pub fn edge_meta_heap(m: &EdgeMeta) -> u64 {
    opt_str_bytes(&m.condition) + opt_str_bytes(&m.effect_desc) + u64_vec_bytes(&m.arg_ids)
}

// ── Tổng hợp theo cấu trúc ──

/// RAM của `symbols: HashMap<u64, Symbol>`.
pub fn symbols_mem(m: &HashMap<u64, Symbol>) -> StructureMem {
    StructureMem {
        entries: m.len() as u64,
        fixed_bytes: map_bucket_bytes(m)
            + (m.len() as u64).saturating_mul(size_of::<Symbol>() as u64),
        heap_bytes: m.values().map(symbol_heap).sum(),
    }
}

/// RAM của `chains_map: HashMap<u64, Vec<u64>>`.
pub fn chains_map_mem(m: &HashMap<u64, Vec<u64>>) -> StructureMem {
    StructureMem {
        entries: m.len() as u64,
        fixed_bytes: map_bucket_bytes(m),
        heap_bytes: m.values().map(u64_vec_bytes).sum(),
    }
}

/// RAM của `call_names: HashMap<String, Vec<CallSite>>`.
pub fn call_names_mem(m: &HashMap<String, Vec<CallSite>>) -> StructureMem {
    StructureMem {
        entries: m.len() as u64,
        fixed_bytes: map_bucket_bytes(m)
            + (m.len() as u64).saturating_mul(size_of::<Vec<CallSite>>() as u64),
        heap_bytes: m
            .iter()
            .map(|(k, v)| {
                str_bytes(k) + vec_bytes::<CallSite>(v) + v.iter().map(call_site_heap).sum::<u64>()
            })
            .sum(),
    }
}

/// RAM của `edges: HashMap<(u64, u64), EdgeMeta>`.
pub fn edges_mem(m: &HashMap<(u64, u64), EdgeMeta>) -> StructureMem {
    StructureMem {
        entries: m.len() as u64,
        fixed_bytes: map_bucket_bytes(m),
        heap_bytes: m.values().map(edge_meta_heap).sum(),
    }
}

/// RAM của `name_index: HashMap<String, Vec<u64>>` (key + buffer, không tính
/// phần tử `u64` vì đã nằm trong `u64_vec_bytes` của từng value).
pub fn name_index_mem(m: &HashMap<String, Vec<u64>>) -> StructureMem {
    StructureMem {
        entries: m.len() as u64,
        fixed_bytes: map_bucket_bytes(m),
        heap_bytes: m.iter().map(|(k, v)| str_bytes(k) + u64_vec_bytes(v)).sum(),
    }
}

/// RAM của `scope_index: HashMap<u64, Vec<u64>>`.
pub fn scope_index_mem(m: &HashMap<u64, Vec<u64>>) -> StructureMem {
    chains_map_mem(m)
}

/// RAM của `name_records: Vec<String>` + `sorted_name_keys: Vec<String>`.
#[allow(clippy::ptr_arg)] // xem `vec_bytes` — cần `capacity()` của `Vec`.
pub fn name_keys_mem(records: &Vec<String>, sorted: &Vec<String>) -> StructureMem {
    StructureMem {
        entries: (records.len() + sorted.len()) as u64,
        fixed_bytes: vec_bytes::<String>(records) + vec_bytes::<String>(sorted),
        heap_bytes: records.iter().map(str_bytes).sum::<u64>()
            + sorted.iter().map(str_bytes).sum::<u64>(),
    }
}

/// RAM của `files: Vec<FileInfo>`.
#[allow(clippy::ptr_arg)] // xem `vec_bytes` — cần `capacity()` của `Vec`.
pub fn files_mem(files: &Vec<FileInfo>) -> StructureMem {
    StructureMem {
        entries: files.len() as u64,
        fixed_bytes: vec_bytes::<FileInfo>(files),
        heap_bytes: files.iter().map(|f| str_bytes(&f.path)).sum(),
    }
}

/// Toàn bộ breakdown của một `GraphIndex`.
#[derive(Debug, Clone, Default)]
pub struct MemBreakdown {
    pub symbols: StructureMem,
    pub chains_map: StructureMem,
    pub call_names: StructureMem,
    pub edges: StructureMem,
    pub name_index: StructureMem,
    pub scope_index: StructureMem,
    pub name_keys: StructureMem,
    pub files: StructureMem,
    /// Occupancy của LRU cache phía trên storage: `(tên, số entry)`.
    pub caches: Vec<(String, usize)>,
}

impl MemBreakdown {
    /// Tổng bytes đã tính được (chưa gồm overhead allocator).
    pub fn accounted_total(&self) -> u64 {
        [
            self.symbols.total_bytes(),
            self.chains_map.total_bytes(),
            self.call_names.total_bytes(),
            self.edges.total_bytes(),
            self.name_index.total_bytes(),
            self.scope_index.total_bytes(),
            self.name_keys.total_bytes(),
            self.files.total_bytes(),
        ]
        .into_iter()
        .sum()
    }

    /// Các cấu trúc xếp theo tổng bytes giảm dần — thứ tự nên tối ưu.
    pub fn ranked(&self) -> Vec<(&'static str, StructureMem)> {
        let mut v = vec![
            ("symbols", self.symbols),
            ("chains_map", self.chains_map),
            ("call_names", self.call_names),
            ("edges", self.edges),
            ("name_index", self.name_index),
            ("scope_index", self.scope_index),
            ("name_keys", self.name_keys),
            ("files", self.files),
        ];
        // `sort_unstable_by` + tiebreaker theo tên → thứ tự **hoàn toàn xác định**
        // (không phụ thuộc thứ tự ban đầu khi hai cấu trúc bằng bytes).
        v.sort_unstable_by(|a, b| b.1.total_bytes().cmp(&a.1.total_bytes()).then(a.0.cmp(b.0)));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_heap_counts_strings() {
        let sym = Symbol {
            id: 100,
            name: "foo".into(),
            kind: codegraph_core::SymbolKind::Function,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "a.rs".into(),
            line: 1,
            end_line: 1,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "rust".into(),
        };
        let mem = symbols_mem(&HashMap::from([(100, sym)]));
        assert_eq!(mem.entries, 1);
        assert!(
            mem.heap_bytes >= 6,
            "phải tính name + file: {}",
            mem.heap_bytes
        );
    }

    #[test]
    fn call_names_heap_includes_args() {
        let site = CallSite {
            caller_id: 1,
            call_name: "lib::helper_0".into(),
            line: 3,
            condition: Some("x > 0".into()),
            is_loop_body: false,
            arg_exprs: vec!["a".into(), "b".into()],
        };
        let mem = call_names_mem(&HashMap::from([("lib::helper_0".to_string(), vec![site])]));
        assert_eq!(mem.entries, 1);
        // key + call_name + condition + 2 args + Vec buffer.
        assert!(mem.heap_bytes > 30, "heap quá nhỏ: {}", mem.heap_bytes);
    }

    #[test]
    fn ranked_is_sorted_desc() {
        let b = MemBreakdown {
            symbols: StructureMem {
                entries: 1,
                fixed_bytes: 0,
                heap_bytes: 100,
            },
            edges: StructureMem {
                entries: 1,
                fixed_bytes: 0,
                heap_bytes: 10,
            },
            ..Default::default()
        };
        let r = b.ranked();
        assert_eq!(r[0].0, "symbols");
        assert_eq!(r[1].0, "edges");
        assert_eq!(b.accounted_total(), 110);
    }
}
