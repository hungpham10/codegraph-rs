//! Binary graph runtime — dataset **riêng** cho symbol binary (pattern
//! `codegraph-docs`), chạy trên trait [`Storage`] của codegraph-graph nên hỗ trợ
//! mọi backend: sqlite (`.codegraph/binary.sqlite`), lmdb, redis (keyspace
//! `codegraph:binary`), in-memory. Không đụng bảng/keys của code index lẫn docs.
//!
//! Lazy: open chỉ mở storage (không load symbol nào vào RAM). Query đi qua:
//! - **Name trie** (`Search<u8>` trên cùng dataset — record index riêng bắt đầu
//!   từ [`RECORD_START`]): substring/prefix/exact search theo tên, persist.
//! - **Secondary index** trên record-meta stream (`set_meta`/`get_meta`, keyed
//!   bằng hash của tên key): `all` / `kind:{k}` / `flag:{f}` / `addr:{a}` /
//!   `ep` / `path:{p}` → danh sách symbol id (JSON). Mỗi danh sách chỉ được
//!   load lúc query, phân trang ở bước cuối.
//! - **Symbol JSON** qua `save_symbol`/`load_symbol`, chain qua
//!   `set_chain`/`get_chain`, call records qua `set_call_records`.

use crate::config::ExtractConfig;
use camino::Utf8Path;
use codegraph_core::{CallRecord, Error, Result, Symbol, SymbolKind};
use codegraph_graph::{
    open_keyspace_storage, ParseResult, Search, SearchError, Storage, StorageError,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Base id mặc định cho symbol binary graph — tránh dải docs (1e9/3e9) và
/// dải code index. Override bằng `[bingraph] bin_base`.
pub const DEFAULT_BIN_BASE: u64 = 2_000_000_000;

/// Sharding của name trie (GraphIndex dùng 64 cho chain engine).
const BIN_SHARDING: usize = 64;

/// Record index đầu tiên của name trie — các số nhỏ hơn là dải của secondary
/// index (hash key). Trie record tăng dần từ đây.
const RECORD_START: usize = 10_000;

type SharedStorage = Arc<RwLock<dyn Storage>>;

/// Flag chính của symbol binary (annotation → index key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BinFlag {
    Import,
    Export,
    Entrypoint,
    Jni,
}

impl BinFlag {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinFlag::Import => "import",
            BinFlag::Export => "export",
            BinFlag::Entrypoint => "entrypoint",
            BinFlag::Jni => "jni",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "import" => Some(BinFlag::Import),
            "export" => Some(BinFlag::Export),
            "entrypoint" => Some(BinFlag::Entrypoint),
            "jni" => Some(BinFlag::Jni),
            _ => None,
        }
    }
}

/// Tất cả flag của một symbol (mỗi annotation khớp một flag — một symbol có
/// thể nằm trong nhiều index, vd jni + export).
fn symbol_flags(sym: &Symbol) -> Vec<BinFlag> {
    let mut v = Vec::new();
    let has = |n: &str| sym.annotations.iter().any(|a| a.name == n);
    for (n, f) in [
        ("jni", BinFlag::Jni),
        ("entrypoint", BinFlag::Entrypoint),
        ("export", BinFlag::Export),
        ("import", BinFlag::Import),
    ] {
        if has(n) {
            v.push(f);
        }
    }
    v
}

/// Flag đại diện hiển thị (ưu tiên jni > entrypoint > export > import).
fn annotation_flag(sym: &Symbol) -> Option<BinFlag> {
    symbol_flags(sym).into_iter().next()
}

/// Một row symbol trả về từ query — decode từ `Symbol` (lazy theo id).
#[derive(Debug, Clone, Serialize)]
pub struct BinSymbolRow {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub addr: u64,
    pub end_addr: u64,
    pub path: String,
    pub flag: Option<String>,
    pub lib: Option<String>,
    pub signature: Option<String>,
}

impl From<Symbol> for BinSymbolRow {
    fn from(s: Symbol) -> Self {
        BinSymbolRow {
            id: s.id,
            flag: annotation_flag(&s).map(|f| f.as_str().to_string()),
            lib: s.type_name.clone().or_else(|| s.doc.clone()),
            kind: format!("{:?}", s.kind),
            name: s.name,
            addr: u64::from(s.line),
            end_addr: u64::from(s.end_line),
            path: s.file,
            signature: s.signature,
        }
    }
}

/// Mode search theo tên.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum NameMatch {
    Exact,
    Prefix,
    Suffix,
    /// Chứa ở giữa — đi qua name trie (radix DFS + KMP), không scan.
    #[default]
    Contains,
}

/// Sort order cho list.
#[derive(Debug, Clone, Copy, Default)]
pub enum ListOrder {
    #[default]
    Name,
    Addr,
    Id,
}

/// Một trang kết quả list/search.
#[derive(Debug, Clone, Serialize)]
pub struct BinPage {
    pub rows: Vec<BinSymbolRow>,
    pub total: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BinStats {
    pub symbols: u64,
    pub entrypoints: u64,
    pub imports: u64,
    pub exports: u64,
    pub binaries: u64,
}

// ── Secondary index trên record-meta stream ──

/// FNV-1a 64 — hash key secondary index thành record id trên meta stream.
/// Trie record (bắt đầu từ [`RECORD_START`]) và hash key có thể trùng số trong
/// lý thuyết nhưng xác suất ~0 (FNV phân bố đều trên 2^64).
fn kv_record(key: &str) -> usize {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h as usize
}

fn ids_json(ids: &[u64]) -> Result<Vec<u8>> {
    serde_json::to_vec(ids).map_err(|e| Error::Db(format!("encode ids: {e}")))
}

async fn meta_ids(storage: &SharedStorage, key: &str) -> Result<Vec<u64>> {
    let s = storage.read().await;
    let bytes = s.get_meta(kv_record(key)).await.map_err(db_err)?;
    Ok(bytes
        .and_then(|b| serde_json::from_slice::<Vec<u64>>(&b).ok())
        .unwrap_or_default())
}

/// Đọc ids theo record index thô (dùng cho meta của name-trie record).
async fn meta_ids_at(storage: &SharedStorage, record: usize) -> Result<Vec<u64>> {
    let s = storage.read().await;
    let bytes = s.get_meta(record).await.map_err(db_err)?;
    Ok(bytes
        .and_then(|b| serde_json::from_slice::<Vec<u64>>(&b).ok())
        .unwrap_or_default())
}

async fn meta_set_ids(storage: &SharedStorage, key: &str, ids: &[u64]) -> Result<()> {
    let mut s = storage.write().await;
    s.set_meta(kv_record(key), &ids_json(ids)?)
        .await
        .map_err(db_err)
}

async fn meta_set_ids_at(storage: &SharedStorage, record: usize, ids: &[u64]) -> Result<()> {
    let mut s = storage.write().await;
    s.set_meta(record, &ids_json(ids)?).await.map_err(db_err)
}

async fn meta_add_id(storage: &SharedStorage, key: &str, id: u64) -> Result<()> {
    let mut ids = meta_ids(storage, key).await?;
    if !ids.contains(&id) {
        ids.push(id);
        meta_set_ids(storage, key, &ids).await?;
    }
    Ok(())
}

async fn meta_remove_id(storage: &SharedStorage, key: &str, id: u64) -> Result<()> {
    let mut ids = meta_ids(storage, key).await?;
    let before = ids.len();
    ids.retain(|&x| x != id);
    if ids.len() != before {
        meta_set_ids(storage, key, &ids).await?;
    }
    Ok(())
}

// ── BinaryGraph ──

/// Binary graph — dataset riêng cho symbol binary trên trait [`Storage`].
pub struct BinaryGraph {
    storage: SharedStorage,
    /// Name trie (substring/prefix search) — record index riêng từ
    /// [`RECORD_START`], meta của record = danh sách symbol id mang tên đó
    /// (tên trùng nhiều symbol / nhiều binary).
    names: Arc<RwLock<Search<u8>>>,
    bin_base: u64,
}

impl BinaryGraph {
    /// Mở (hoặc tạo) binary graph. `dsn` dạng `sqlite://<path>`, `lmdb://<dir>`,
    /// `redis://<url>`; `None` → in-memory. Open là O(1): chỉ mở storage +
    /// Search (trie persist trong storage) — KHÔNG load symbol nào vào RAM.
    pub async fn open(dsn: Option<&str>, bin_base: u64) -> Result<Self> {
        let storage: SharedStorage = match dsn {
            Some(dsn) => open_keyspace_storage(dsn, "codegraph:binary").await?,
            None => Arc::new(RwLock::new(codegraph_graph::InMemoryStorage::default())),
        };
        Ok(Self {
            names: Arc::new(RwLock::new(Search::new(BIN_SHARDING, storage.clone()))),
            storage,
            bin_base,
        })
    }

    /// Mở theo config `[bingraph]` — dùng chung cho CLI và MCP.
    /// Backend không khai báo dsn → in-memory + warn.
    pub async fn open_from_config(root: &Utf8Path) -> Result<Self> {
        let cfg = ExtractConfig::load(root);
        if !cfg.bingraph.is_enabled() {
            return Err(Error::Db(
                "[bingraph] bị tắt trong .codegraph/config.toml".to_string(),
            ));
        }
        let dsn = cfg.bingraph_dsn(root);
        if dsn.is_none() {
            tracing::warn!(
                "[bingraph] không có DSN hợp lệ — dùng in-memory \
                 (override bằng [bingraph.storage] dsn)"
            );
        }
        Self::open(dsn.as_deref(), cfg.bin_base()).await
    }

    /// Base id đang dùng cho symbol binary graph.
    pub fn bin_base(&self) -> u64 {
        self.bin_base
    }

    // ------------------------------------------------------------------
    // Ingest
    // ------------------------------------------------------------------

    /// Ingest một `ParseResult` binary (language = "binary"): remap id sang dải
    /// `bin_base`, lưu symbols + secondary index + name trie + chains + calls.
    /// Idempotent per path — index/symbol của path cũ bị gỡ trước khi ghi.
    pub async fn ingest(&self, parsed: &ParseResult, bin_base: u64) -> Result<()> {
        // 1. Gỡ index của path cũ (re-index thay thế).
        let path_key = format!("path:{}", parsed.path);
        let old_ids = meta_ids(&self.storage, &path_key).await?;
        for old in &old_ids {
            if let Some(sym) = self.load_symbol(*old).await? {
                meta_remove_id(&self.storage, "all", *old).await?;
                meta_remove_id(&self.storage, &format!("kind:{:?}", sym.kind), *old).await?;
                for f in symbol_flags(&sym) {
                    meta_remove_id(&self.storage, &format!("flag:{}", f.as_str()), *old).await?;
                }
                meta_remove_id(&self.storage, &format!("addr:{}", sym.line), *old).await?;
                if symbol_flags(&sym).contains(&BinFlag::Entrypoint) {
                    meta_remove_id(&self.storage, "ep", *old).await?;
                }
                self.name_index_remove(&sym.name, *old).await?;
            }
        }

        // 2. Lưu symbol + secondary index mới.
        let mut new_ids = Vec::with_capacity(parsed.symbols.len());
        for sym in &parsed.symbols {
            let mut stored = sym.clone();
            stored.id = bin_base + sym.id;
            new_ids.push(stored.id);
            self.storage
                .write()
                .await
                .save_symbol(&stored)
                .await
                .map_err(db_err)?;
            meta_add_id(&self.storage, "all", stored.id).await?;
            meta_add_id(&self.storage, &format!("kind:{:?}", sym.kind), stored.id).await?;
            for f in symbol_flags(sym) {
                meta_add_id(&self.storage, &format!("flag:{}", f.as_str()), stored.id).await?;
                if f == BinFlag::Entrypoint {
                    meta_add_id(&self.storage, "ep", stored.id).await?;
                }
            }
            meta_add_id(&self.storage, &format!("addr:{}", sym.line), stored.id).await?;
        }
        meta_add_id(&self.storage, "paths", kv_record(&path_key) as u64).await?;
        meta_set_ids(&self.storage, &path_key, &new_ids).await?;

        // 3. Name trie: mỗi tên distinct một record; meta record = ids.
        let mut names_map: HashMap<&str, Vec<u64>> = HashMap::new();
        for sym in &parsed.symbols {
            names_map
                .entry(sym.name.as_str())
                .or_default()
                .push(bin_base + sym.id);
        }
        let mut next_record: usize = {
            let ids = meta_ids(&self.storage, "next_record").await?;
            ids.first().copied().unwrap_or(RECORD_START as u64) as usize
        };
        for (name, ids) in &names_map {
            let existing = self.name_record_lookup(name).await?;
            match existing {
                Some(record) => {
                    let mut current = meta_ids_at(&self.storage, record).await?;
                    for id in ids {
                        if !current.contains(id) {
                            current.push(*id);
                        }
                    }
                    meta_set_ids_at(&self.storage, record, &current).await?;
                }
                None => {
                    let metas: Vec<Option<&[u8]>> = vec![None; name.len()];
                    self.names
                        .write()
                        .await
                        .insert_chain(next_record, name.as_bytes(), &metas)
                        .await
                        .map_err(|e| match e {
                            SearchError::Duplicated => {
                                Error::Db("name trie duplicated".to_string())
                            }
                            other => Error::Db(format!("name trie insert: {other}")),
                        })?;
                    meta_set_ids_at(&self.storage, next_record, ids).await?;
                    next_record += 1;
                }
            }
        }
        meta_set_ids(&self.storage, "next_record", &[next_record as u64]).await?;

        // 4. Chains (u64 native) + call records (JSON).
        for (local_id, chain) in &parsed.chains {
            let global: Vec<u64> = chain.iter().map(|v| bin_base + v).collect();
            self.storage
                .write()
                .await
                .set_chain((bin_base + local_id) as usize, &global)
                .await
                .map_err(db_err)?;
        }
        for call in &parsed.calls {
            let mut recs = self
                .storage
                .read()
                .await
                .get_call_records(bin_base + call.caller_id)
                .await
                .map_err(db_err)?
                .and_then(|b| serde_json::from_slice::<Vec<CallRecord>>(&b).ok())
                .unwrap_or_default();
            let mut rec = call.clone();
            rec.caller_id = bin_base + call.caller_id;
            recs.push(rec);
            let blob = serde_json::to_vec(&recs).map_err(|e| Error::Db(e.to_string()))?;
            self.storage
                .write()
                .await
                .set_call_records(bin_base + call.caller_id, &blob)
                .await
                .map_err(db_err)?;
        }
        Ok(())
    }

    /// Tìm record của tên trong trie (exact match trên key).
    async fn name_record_lookup(&self, name: &str) -> Result<Option<usize>> {
        let trie = self.names.read().await;
        let hits = trie
            .search_prefix(name.as_bytes())
            .await
            .map_err(|e| Error::Db(format!("name trie lookup: {e}")))?;
        Ok(hits
            .into_iter()
            .find(|(key, _)| key.as_slice() == name.as_bytes())
            .map(|(_, record)| record))
    }

    /// Gỡ một symbol id khỏi meta của name record.
    async fn name_index_remove(&self, name: &str, id: u64) -> Result<()> {
        if let Some(record) = self.name_record_lookup(name).await? {
            let mut ids = meta_ids_at(&self.storage, record).await?;
            ids.retain(|&x| x != id);
            meta_set_ids_at(&self.storage, record, &ids).await?;
        }
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        self.storage
            .read()
            .await
            .load_symbol(id)
            .await
            .map_err(db_err)
    }

    /// Load symbols theo danh sách id, lọc kind/flag/path, sort theo order,
    /// phân trang.
    async fn load_page(
        &self,
        ids: &[u64],
        kind: Option<&SymbolKind>,
        flag: Option<&BinFlag>,
        path: Option<&str>,
        order: ListOrder,
        offset: u64,
        limit: u64,
    ) -> Result<BinPage> {
        let mut rows: Vec<BinSymbolRow> = Vec::new();
        for id in ids {
            if let Some(sym) = self.load_symbol(*id).await? {
                if let Some(k) = kind {
                    if sym.kind != *k {
                        continue;
                    }
                }
                if let Some(f) = flag {
                    if !symbol_flags(&sym).contains(f) {
                        continue;
                    }
                }
                if let Some(p) = path {
                    if sym.file != p {
                        continue;
                    }
                }
                rows.push(sym.into());
            }
        }
        match order {
            ListOrder::Name => rows.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id))),
            ListOrder::Addr => rows.sort_by(|a, b| a.addr.cmp(&b.addr).then(a.id.cmp(&b.id))),
            ListOrder::Id => rows.sort_by_key(|r| r.id),
        }
        let total = rows.len() as u64;
        let rows = rows
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect();
        Ok(BinPage {
            rows,
            total,
            offset,
        })
    }

    // ------------------------------------------------------------------
    // Lazy queries
    // ------------------------------------------------------------------

    /// List symbol theo kind/flag/path — phân trang, không load hết vào RAM
    /// trừ khi không có filter nào (ids từ secondary index).
    pub async fn list(
        &self,
        kind: Option<SymbolKind>,
        flag: Option<BinFlag>,
        path: Option<&str>,
        order: ListOrder,
        offset: u64,
        limit: u64,
    ) -> Result<BinPage> {
        // Chọn index đơn hẹp nhất có sẵn, phần còn lại lọc khi load symbol.
        let ids = if let Some(k) = &kind {
            meta_ids(&self.storage, &format!("kind:{k:?}")).await?
        } else if let Some(f) = &flag {
            meta_ids(&self.storage, &format!("flag:{}", f.as_str())).await?
        } else if let Some(p) = path {
            meta_ids(&self.storage, &format!("path:{p}")).await?
        } else {
            meta_ids(&self.storage, "all").await?
        };
        self.load_page(
            &ids,
            kind.as_ref(),
            flag.as_ref(),
            path,
            order,
            offset,
            limit,
        )
        .await
    }

    /// Search theo tên + mode. Contains/suffix đi qua name trie (substring);
    /// exact/prefix đi qua prefix lookup của trie — mọi mode đều không scan.
    pub async fn search_name(
        &self,
        pattern: &str,
        mode: NameMatch,
        kind: Option<SymbolKind>,
        flag: Option<BinFlag>,
        offset: u64,
        limit: u64,
    ) -> Result<BinPage> {
        if pattern.is_empty() {
            return Ok(BinPage {
                rows: Vec::new(),
                total: 0,
                offset,
            });
        }
        let mut ids: Vec<u64> = Vec::new();
        match mode {
            NameMatch::Contains | NameMatch::Suffix => {
                let page = self
                    .names
                    .read()
                    .await
                    .search_resumable(pattern.as_bytes(), None, None, None)
                    .await
                    .map_err(|e| Error::Db(format!("name trie search: {e}")))?;
                for record in page.record_ids {
                    ids.extend(meta_ids_at(&self.storage, record).await?);
                }
            }
            NameMatch::Exact | NameMatch::Prefix => {
                let hits = self
                    .names
                    .read()
                    .await
                    .search_prefix(pattern.as_bytes())
                    .await
                    .map_err(|e| Error::Db(format!("name trie prefix: {e}")))?;
                for (key, record) in hits {
                    if mode == NameMatch::Exact && key.as_slice() != pattern.as_bytes() {
                        continue;
                    }
                    ids.extend(meta_ids_at(&self.storage, record).await?);
                }
            }
        }
        self.load_page(
            &ids,
            kind.as_ref(),
            flag.as_ref(),
            None,
            ListOrder::Name,
            offset,
            limit,
        )
        .await
    }

    /// Tra cứu theo địa chỉ (secondary index `addr:{a}`) — điểm bắt đầu
    /// phân tích binary.
    pub async fn by_addr(&self, addr: u64, limit: u64) -> Result<Vec<BinSymbolRow>> {
        let ids = meta_ids(&self.storage, &format!("addr:{addr}")).await?;
        let page = self
            .load_page(&ids, None, None, None, ListOrder::Name, 0, limit)
            .await?;
        Ok(page.rows)
    }

    /// Danh sách entry point (toàn bộ hoặc lọc theo binary path) — điểm bắt
    /// đầu phân tích thay cho grep với code.
    pub async fn entrypoints(
        &self,
        path: Option<&str>,
        limit: u64,
    ) -> Result<Vec<(String, String)>> {
        let ids = meta_ids(&self.storage, "ep").await?;
        let mut eps: Vec<(u64, String, String)> = Vec::new();
        for id in ids {
            if let Some(sym) = self.load_symbol(id).await? {
                if let Some(p) = path {
                    if sym.file != p {
                        continue;
                    }
                }
                eps.push((u64::from(sym.line), sym.file, sym.name));
            }
        }
        eps.sort();
        Ok(eps
            .into_iter()
            .take(limit as usize)
            .map(|(_, path, name)| (path, name))
            .collect())
    }

    /// Lấy symbol đầy đủ theo id — lazy hydrate.
    pub async fn get_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        self.load_symbol(id).await
    }

    /// Chain (flow) của một symbol id — native u64 trên Storage.
    pub async fn get_chain(&self, id: u64) -> Result<Option<Vec<u64>>> {
        self.storage
            .read()
            .await
            .get_chain(id as usize)
            .await
            .map_err(db_err)
    }

    /// Call records của một caller id.
    pub async fn get_calls(
        &self,
        caller: u64,
    ) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
        let blob = self
            .storage
            .read()
            .await
            .get_call_records(caller)
            .await
            .map_err(db_err)?;
        let recs: Vec<CallRecord> = blob
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(recs
            .into_iter()
            .map(|c| (c.position as i64, Some(c.call_name), c.condition))
            .collect())
    }

    /// Thống kê — đếm từ secondary index (chỉ đọc danh sách id, không load
    /// symbol).
    pub async fn stats(&self) -> Result<BinStats> {
        Ok(BinStats {
            symbols: meta_ids(&self.storage, "all").await?.len() as u64,
            entrypoints: meta_ids(&self.storage, "ep").await?.len() as u64,
            imports: meta_ids(&self.storage, "flag:import").await?.len() as u64,
            exports: meta_ids(&self.storage, "flag:export").await?.len() as u64,
            binaries: meta_ids(&self.storage, "paths").await?.len() as u64,
        })
    }
}

fn db_err(e: StorageError) -> Error {
    Error::Db(format!("binary graph: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{Annotation, EffectType};
    use std::collections::HashMap;

    fn sym(
        id: u64,
        name: &str,
        kind: SymbolKind,
        line: u32,
        annotations: Vec<Annotation>,
    ) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind,
            scope: codegraph_core::ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: "/tmp/fake.so".to_string(),
            line,
            end_line: line,
            signature: None,
            doc: None,
            annotations,
            language: "binary".to_string(),
        }
    }

    fn ann(name: &str) -> Annotation {
        Annotation {
            name: name.to_string(),
            args: HashMap::new(),
            line: 0,
        }
    }

    fn sample() -> ParseResult {
        ParseResult {
            path: "/tmp/fake.so".to_string(),
            language: "binary".to_string(),
            bytes: 0,
            lines: 0,
            symbols: vec![
                sym(
                    1,
                    "entry0",
                    SymbolKind::Function,
                    4096,
                    vec![ann("entrypoint")],
                ),
                sym(2, "foo", SymbolKind::Function, 4200, vec![ann("export")]),
                sym(3, "memcpy", SymbolKind::Function, 100, vec![ann("import")]),
                sym(4, "local_fn", SymbolKind::Function, 5000, Vec::new()),
                sym(5, "str:6000", SymbolKind::Constant, 6000, Vec::new()),
            ],
            chains: HashMap::from([(1u64, vec![1u64, 3u64])]),
            calls: vec![CallRecord {
                caller_id: 1,
                call_name: "memcpy".to_string(),
                position: 1,
                arg_exprs: Vec::new(),
                line: 4100,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            }],
        }
    }

    async fn mem_graph() -> BinaryGraph {
        BinaryGraph::open(None, DEFAULT_BIN_BASE).await.unwrap()
    }

    #[tokio::test]
    async fn ingest_and_lazy_queries() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();

        // list theo flag — import/export/entrypoint.
        let page = g
            .list(None, Some(BinFlag::Export), None, ListOrder::Name, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].name, "foo");

        let eps = g
            .list(
                None,
                Some(BinFlag::Entrypoint),
                None,
                ListOrder::Name,
                0,
                50,
            )
            .await
            .unwrap();
        assert_eq!(eps.rows.len(), 1);
        assert_eq!(eps.rows[0].name, "entry0");

        // list theo kind — Constant chỉ có str:6000.
        let page = g
            .list(
                Some(SymbolKind::Constant),
                None,
                None,
                ListOrder::Name,
                0,
                50,
            )
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].name, "str:6000");

        // by_addr.
        let rows = g.by_addr(4200, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "foo");

        // entrypoints listing.
        let eps = g.entrypoints(Some("/tmp/fake.so"), 10).await.unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0].1, "entry0");

        // get_symbol lazy hydrate + chain (u64 native trên Storage).
        let s = g.get_symbol(DEFAULT_BIN_BASE + 2).await.unwrap().unwrap();
        assert_eq!(s.name, "foo");
        let chain = g.get_chain(DEFAULT_BIN_BASE + 1).await.unwrap().unwrap();
        assert_eq!(chain, vec![DEFAULT_BIN_BASE + 1, DEFAULT_BIN_BASE + 3]);
        let calls = g.get_calls(DEFAULT_BIN_BASE + 1).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.as_deref(), Some("memcpy"));

        // stats.
        let stats = g.stats().await.unwrap();
        assert_eq!(stats.symbols, 5);
        assert_eq!(stats.entrypoints, 1);
        assert_eq!(stats.imports, 1);
        assert_eq!(stats.exports, 1);
        assert_eq!(stats.binaries, 1);
    }

    #[tokio::test]
    async fn reingest_replaces_path() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        let mut updated = sample();
        updated.symbols = vec![sym(9, "only_one", SymbolKind::Function, 1, Vec::new())];
        g.ingest(&updated, DEFAULT_BIN_BASE).await.unwrap();
        let stats = g.stats().await.unwrap();
        assert_eq!(stats.symbols, 1, "re-ingest cùng path phải thay thế index");
        assert_eq!(stats.binaries, 1, "path cũ vẫn là 1 binary");
    }

    #[tokio::test]
    async fn pagination() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        let page = g
            .list(None, None, None, ListOrder::Name, 0, 2)
            .await
            .unwrap();
        assert_eq!(page.total, 5);
        assert_eq!(page.rows.len(), 2);
        let page2 = g
            .list(None, None, None, ListOrder::Name, 2, 2)
            .await
            .unwrap();
        assert_eq!(page2.rows.len(), 2);
        assert_ne!(page.rows[0].id, page2.rows[0].id);
    }

    #[tokio::test]
    async fn contains_and_suffix_via_trie() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        // contains "cpy" khớp "memcpy" qua trie (substring DFS).
        let page = g
            .search_name("cpy", NameMatch::Contains, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].name, "memcpy");
        // suffix "oo" trả foo.
        let page = g
            .search_name("oo", NameMatch::Suffix, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].name, "foo");
        // prefix qua trie.
        let page = g
            .search_name("mem", NameMatch::Prefix, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        // contains không khớp gì → trang rỗng.
        let page = g
            .search_name("zzz", NameMatch::Contains, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 0);
    }

    #[tokio::test]
    async fn reingest_does_not_return_stale_names() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        let mut updated = sample();
        updated.symbols = vec![sym(9, "only_one", SymbolKind::Function, 1, Vec::new())];
        g.ingest(&updated, DEFAULT_BIN_BASE).await.unwrap();
        // "memcpy" đã bị gỡ khỏi index của path cũ → contains không trả row.
        let page = g
            .search_name("cpy", NameMatch::Contains, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 0);
        let page = g
            .search_name("only", NameMatch::Contains, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 1);
    }

    #[tokio::test]
    async fn duplicate_name_across_binaries() {
        let g = mem_graph().await;
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        // Binary thứ 2 cũng có symbol "memcpy" — cùng tên, khác path.
        let mut other = sample();
        other.path = "/tmp/other.so".to_string();
        let mut sym_other = sym(7, "memcpy", SymbolKind::Function, 200, vec![ann("import")]);
        sym_other.file = other.path.clone();
        other.symbols = vec![sym_other];
        g.ingest(&other, DEFAULT_BIN_BASE).await.unwrap();
        // contains vẫn chỉ 1 record tên "memcpy" nhưng trả 2 symbol id.
        let page = g
            .search_name("memcpy", NameMatch::Exact, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 2, "2 binary cùng tên → 2 row");
        assert!(page.rows.iter().any(|r| r.path == "/tmp/other.so"));
    }

    #[tokio::test]
    async fn persists_on_sqlite_backend() {
        // Backend sqlite qua trait Storage — persist qua các lần open, dataset
        // riêng (binary.sqlite) không đụng db.sqlite/docs.sqlite.
        let dir = tempfile::tempdir().unwrap();
        let dsn = format!(
            "sqlite://{}",
            dir.path().join("binary.sqlite").to_str().unwrap()
        );
        let g = BinaryGraph::open(Some(&dsn), DEFAULT_BIN_BASE)
            .await
            .unwrap();
        g.ingest(&sample(), DEFAULT_BIN_BASE).await.unwrap();
        assert!(dir.path().join("binary.sqlite").exists());
        let g2 = BinaryGraph::open(Some(&dsn), DEFAULT_BIN_BASE)
            .await
            .unwrap();
        let stats = g2.stats().await.unwrap();
        assert_eq!(stats.symbols, 5, "persist qua các lần open");
        let page = g2
            .search_name("cpy", NameMatch::Contains, None, None, 0, 50)
            .await
            .unwrap();
        assert_eq!(page.total, 1, "name trie persist trên storage");
    }
}
