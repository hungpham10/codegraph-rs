//! Graph index theo kiến trúc semgraph: SymbolRegistry + chain engine + name engine.
//!
//! Mọi symbol có một **id global** (registry monotonic, bắt đầu từ `SYMBOL_BASE`,
//! persist `next_id`); call chain của một hàm là chuỗi `u64` gồm **marker**
//! (luồng điều khiển, id `< SYMBOL_BASE`) và **symbol id** của callee:
//!
//! ```text
//! chain(F) = [F, m1, calleeA, m2, calleeB, ...]        // calleeA bị điều kiện m1
//! ```
//!
//! Chain bắt đầu bằng chính func id (vị trí 0). Edge `(caller, callee)` suy từ
//! chain: mỗi symbol element là một callee.
//!
//! ## Hai engine trên 1 file
//!
//! - **Chain engine** `Search<u64>`: key = chain (u64 element), record = func id.
//!   `callers(F)` = substring search `&[F]` (KMP trên shortcuts); `callees(F)` =
//!   đọc chain, skip marker/self/0. Persistent — dùng chung storage với entity
//!   store (`rt_*` radix tables + `sg_*` entity tables trong cùng sqlite).
//! - **Name engine** `Search<u8>`: key = tên symbol (lowercase bytes), record =
//!   synthetic id (1-based vào `name_records`). Luôn **in-memory** (như
//!   `SearchIndex` của semgraph) — rebuild từ `name_index` khi open/ingest.
//!
//! `name_index: HashMap<lowercase name, Vec<id>>` là nguồn mở rộng symbol trùng
//! tên: radix chỉ lưu mỗi tên khác nhau 1 lần (insert_chain trả `Duplicated` với
//! key trùng), search trả về record → tên → toàn bộ id trùng tên.
//!
//! ## Pipeline ingest (2 phase, như semgraph)
//!
//! `ingest(parse_results)` = full re-index: clear entity + engines → register
//! toàn bộ symbol (id global) + remap (idMap bỏ `0` — placeholder phải giữ 0) →
//! `resolve_calls` (thay placeholder 0 trong chain bằng id thật: structural hint
//! → exact name → short name → best-candidate: @Override +10 / has-chain +5 /
//! same-file +3) → `build_edges_from_calls` (edge = chain[position], CallSite +
//! var-type alias, gom SaveCallRecords) → files → rebuild engines → bump version.

use crate::search::Search;
use crate::storage::InMemoryStorage;
use codegraph_core::{
    is_marker, marker_name, CallRecord, CallSite, CallSiteResult, ClassInfo, Dependency,
    DependenciesReport, EdgeMeta, EffectType, Error, FileInfo, FlowCall, FlowResult, FunctionScope,
    MemberInfo, ResolveResult, SearchFlowResult, SemgraphStats, Symbol, SymbolKind, SymbolMatch,
    SYMBOL_BASE,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

mod radix;
mod search;
mod storage;

mod shared;

pub use shared::SharedGraphIndex;

/// Số shard mặc định cho chain engine (`element % sharding`).
const CHAIN_SHARDING: usize = 64;

/// Kiểu `Result` của crate — alias từ `codegraph_core`.
pub type Result<T> = codegraph_core::Result<T>;

/// Map `StorageError` → `Error::Search`.
fn serr(e: crate::storage::StorageError) -> Error {
    Error::Search(e.to_string())
}

/// Map `search::Error` → `Error::Search`.
fn serr_search(e: crate::search::Error) -> Error {
    Error::Search(e.to_string())
}

/// Kết quả parse một file — input của `GraphIndex::ingest` (full re-index).
///
/// Mọi id trong `symbols`/`chains`/`calls` là **local per-file** (bắt đầu từ
/// `SYMBOL_BASE`, unique trong file); `ingest` remap sang id global (registry
/// monotonic) giống `processFileResult` của semgraph. `0` là placeholder — được
/// giữ nguyên (idMap bỏ `0`), `resolve_calls` thay bằng id thật sau.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParseResult {
    pub path: String,
    pub language: String,
    pub bytes: u64,
    pub lines: u32,
    /// Symbols của file (id local ≥ `SYMBOL_BASE`).
    pub symbols: Vec<Symbol>,
    /// Chain của từng func (key = func id local; chain bắt đầu bằng chính func
    /// id ở vị trí 0, giống semgraph `[funcID, call1, Loop, ...]`).
    pub chains: HashMap<u64, Vec<u64>>,
    /// Call records (caller_id local; `position` trỏ placeholder `0` trong chain).
    pub calls: Vec<CallRecord>,
}

// ==================== GraphIndex ====================

/// Index chính (semgraph-style): registry + 2 engine + inverted indexes.
///
/// `storage` giữ cả entity store (`sg_*` / InMemory maps) lẫn radix của chain
/// engine (`rt_*`); `chains` (Search) đọc ghi qua chính `storage` đó. Name
/// engine `names` luôn chạy trên storage in-memory riêng — không persist.
pub struct GraphIndex {
    /// Entity + chain engine storage.
    storage: Arc<RwLock<dyn crate::storage::Storage>>,
    /// Chain engine: key = chain, record = func id.
    chains: Search<u64>,
    /// Name engine: key = tên lowercase (bytes), record = 1-based vào
    /// `name_records`.
    names: Search<u8>,
    /// `record - 1` → tên (song song với thứ tự insert name engine).
    name_records: Vec<String>,
    /// symbol id → Symbol (registry — nguồn chân lý in-memory).
    symbols: HashMap<u64, Symbol>,
    /// tên (lowercase) → symbol ids (mở rộng trùng tên khi search/resolve).
    name_index: HashMap<String, Vec<u64>>,
    /// scope id → symbol ids (scope query).
    scope_index: HashMap<u64, Vec<u64>>,
    /// func id → chain (nguồn chân lý; engine + storage phái sinh).
    chains_map: HashMap<u64, Vec<u64>>,
    /// call name (lowercase, kèm alias type-qualified) → call sites.
    call_names: HashMap<String, Vec<CallSite>>,
    /// `(caller, callee)` → edge meta (last-wins, rebuild từ chains + records).
    edges: HashMap<(u64, u64), EdgeMeta>,
    /// Files trong graph.
    files: Vec<FileInfo>,
    /// next_id của registry.
    next_id: u64,
    /// index version (bump mỗi lần ingest — SharedGraphIndex dò stale).
    version: u64,
}

impl GraphIndex {
    /// Index in-memory (test/dev, không persist).
    pub fn in_memory() -> Self {
        let storage = Arc::new(RwLock::new(InMemoryStorage::default()))
            as Arc<RwLock<dyn crate::storage::Storage>>;
        Self::new_with_storage(storage)
    }

    /// Mở index từ file sqlite (feature `sqlite`) — rebuild từ entity store.
    #[cfg(feature = "sqlite")]
    pub async fn open(path: &str) -> Result<Self> {
        let storage = crate::storage::sqlite::SqliteStorage::open(path)
            .await
            .map_err(serr)?;
        let storage = Arc::new(RwLock::new(storage)) as Arc<RwLock<dyn crate::storage::Storage>>;
        let mut idx = Self::new_with_storage(storage);
        idx.rebuild().await?;
        Ok(idx)
    }

    fn new_with_storage(storage: Arc<RwLock<dyn crate::storage::Storage>>) -> Self {
        // Name engine luôn in-memory (như semgraph SearchIndex) — storage riêng
        // để record id (1..N) không đụng record của chain engine (func ids).
        let name_storage = Arc::new(RwLock::new(InMemoryStorage::default()))
            as Arc<RwLock<dyn crate::storage::Storage>>;
        Self {
            chains: Search::new(CHAIN_SHARDING, storage.clone()),
            names: Search::new(CHAIN_SHARDING, name_storage),
            storage,
            name_records: Vec::new(),
            symbols: HashMap::new(),
            name_index: HashMap::new(),
            scope_index: HashMap::new(),
            chains_map: HashMap::new(),
            call_names: HashMap::new(),
            edges: HashMap::new(),
            files: Vec::new(),
            next_id: SYMBOL_BASE,
            version: 0,
        }
    }

    // ── Build / rebuild ──

    /// Rebuild toàn bộ index từ entity store trong storage (open/reopen).
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))] // chỉ open() dùng (sqlite)
    async fn rebuild(&mut self) -> Result<()> {
        self.next_id = self
            .storage
            .read()
            .await
            .load_next_id()
            .await
            .map_err(serr)?;
        let symbols = self
            .storage
            .read()
            .await
            .load_all_symbols()
            .await
            .map_err(serr)?;
        let chains_raw = self
            .storage
            .read()
            .await
            .all_chains()
            .await
            .map_err(serr)?;
        let call_names_raw = self
            .storage
            .read()
            .await
            .all_call_name_indexes()
            .await
            .map_err(serr)?;
        let call_records_raw = self
            .storage
            .read()
            .await
            .all_call_records()
            .await
            .map_err(serr)?;
        self.files = self
            .storage
            .read()
            .await
            .load_all_files()
            .await
            .map_err(serr)?;
        self.version = self.storage.read().await.version().await.map_err(serr)?;

        // Registry (scope ids trong entity đã là global — persist sau remap).
        self.symbols.clear();
        self.name_index.clear();
        self.scope_index.clear();
        for sym in symbols {
            self.index_symbol(sym);
        }

        // Chains.
        self.chains_map.clear();
        for (func_id, bytes) in chains_raw {
            self.chains_map
                .insert(func_id, crate::storage::decode_chain(&bytes));
        }

        // Call-name index.
        self.call_names.clear();
        for (name, bytes) in call_names_raw {
            if let Ok(sites) = serde_json::from_slice::<Vec<CallSite>>(&bytes) {
                self.call_names.insert(name, sites);
            }
        }

        // Edges — rebuild từ chains + call records (không persist riêng).
        let mut recs: HashMap<u64, Vec<CallRecord>> = HashMap::new();
        for (func, bytes) in call_records_raw {
            if let Ok(r) = serde_json::from_slice::<Vec<CallRecord>>(&bytes) {
                recs.insert(func, r);
            }
        }
        self.rebuild_edges(&recs);

        // Engines.
        self.rebuild_chain_engine().await?;
        self.rebuild_name_engine().await?;
        Ok(())
    }

    /// Insert symbol vào registry + index (scope id đã global — path rebuild).
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))] // chỉ rebuild() dùng
    fn index_symbol(&mut self, sym: Symbol) {
        let id = sym.id;
        if !sym.name.is_empty() {
            self.name_index
                .entry(sym.name.to_lowercase())
                .or_default()
                .push(id);
        }
        if sym.scope_id != 0 {
            self.scope_index.entry(sym.scope_id).or_default().push(id);
        }
        self.symbols.insert(id, sym);
    }

    /// Rebuild scope index từ symbols hiện tại (sau khi remap scope id).
    fn rebuild_scope_index(&mut self) {
        self.scope_index.clear();
        for (&id, sym) in &self.symbols {
            if sym.scope_id != 0 {
                self.scope_index.entry(sym.scope_id).or_default().push(id);
            }
        }
    }

    /// Rebuild edges từ chains + call records (nhanh — chỉ dùng khi reopen).
    #[cfg_attr(not(feature = "sqlite"), allow(dead_code))] // chỉ rebuild() dùng
    fn rebuild_edges(&mut self, recs: &HashMap<u64, Vec<CallRecord>>) {
        self.edges.clear();
        for (&func_id, chain) in &self.chains_map {
            let rec_by_pos: HashMap<usize, &CallRecord> = recs
                .get(&func_id)
                .map(|r| r.iter().map(|c| (c.position, c)).collect())
                .unwrap_or_default();
            for (i, &e) in chain.iter().enumerate() {
                // Vị trí 0 = owner — skip như build_edges_from_calls (ingest).
                if i == 0 || is_marker(e) || e == 0 {
                    continue;
                }
                let rec = rec_by_pos.get(&i);
                self.edges.insert(
                    (func_id, e),
                    EdgeMeta {
                        caller_id: func_id,
                        callee_id: e,
                        position: i,
                        condition: rec.and_then(|r| r.condition.clone()),
                        effect: rec.map(|r| r.effect).unwrap_or_default(),
                        effect_desc: rec.and_then(|r| r.effect_desc.clone()),
                        arg_ids: Vec::new(),
                        is_loop_body: rec.map(|r| r.is_loop_body).unwrap_or(false),
                        is_recursive: e == func_id,
                    },
                );
            }
        }
    }

    /// Rebuild chain engine từ `chains_map` (clear + insert tuần tự).
    async fn rebuild_chain_engine(&mut self) -> Result<()> {
        self.chains.clear().await.map_err(serr_search)?;
        let mut funcs: Vec<u64> = self.chains_map.keys().copied().collect();
        funcs.sort_unstable();
        for func_id in funcs {
            let chain = &self.chains_map[&func_id];
            // Mọi element meta = None → không ghi node stream (record = func id
            // dùng trực tiếp, không cần indirection như CallIndex cũ).
            let metas: Vec<Option<&[u8]>> = vec![None; chain.len()];
            self.chains
                .insert_chain(func_id as usize, chain, &metas)
                .await
                .map_err(serr_search)?;
        }
        Ok(())
    }

    /// Rebuild name engine từ `name_index` (clear + insert mỗi tên distinct).
    async fn rebuild_name_engine(&mut self) -> Result<()> {
        self.names.clear().await.map_err(serr_search)?;
        self.name_records.clear();
        let mut distinct: Vec<&String> = self.name_index.keys().collect();
        distinct.sort();
        let mut record = 0usize;
        for name in distinct {
            record += 1;
            let metas: Vec<Option<&[u8]>> = vec![None; name.len()];
            self.names
                .insert_chain(record, name.as_bytes(), &metas)
                .await
                .map_err(serr_search)?;
            self.name_records.push(name.clone());
        }
        Ok(())
    }

    // ── Ingest (full re-index — pipeline 2 phase như semgraph) ──

    /// Ingest toàn bộ parse results — **full re-index**: xoá dữ liệu cũ, register
    /// symbol (id global) + remap, resolve placeholder 0, build edges + call-name
    /// index, persist + bump version.
    pub async fn ingest(&mut self, results: &[ParseResult]) -> Result<()> {
        // ── Reset ──
        self.storage
            .write()
            .await
            .clear_entities()
            .await
            .map_err(serr)?;
        self.chains.clear().await.map_err(serr_search)?;
        self.names.clear().await.map_err(serr_search)?;
        self.symbols.clear();
        self.name_index.clear();
        self.scope_index.clear();
        self.chains_map.clear();
        self.call_names.clear();
        self.edges.clear();
        self.files.clear();
        self.name_records.clear();
        self.next_id = SYMBOL_BASE;

        // ── Phase 1: register + remap ──
        let mut all_calls: Vec<CallRecord> = Vec::new();
        for result in results {
            let mut id_map: HashMap<u64, u64> = HashMap::new();
            for sym in &result.symbols {
                let new_id = self.register(sym.clone()).await?;
                if sym.id != 0 {
                    id_map.insert(sym.id, new_id);
                }
            }
            // Remap scope_id + type_ref (id local → global) trên bản đã lưu.
            for sym in &result.symbols {
                let new_id = id_map.get(&sym.id).copied().unwrap_or(sym.id);
                self.remap_scope_type(new_id, &id_map).await?;
            }
            // Chains — remap từng element (0 giữ nguyên — placeholder).
            for (func_id, chain) in &result.chains {
                let nf = id_map.get(func_id).copied().unwrap_or(*func_id);
                let mut nchain: Vec<u64> = chain
                    .iter()
                    .map(|e| id_map.get(e).copied().unwrap_or(*e))
                    .collect();
                if nchain.is_empty() {
                    nchain.push(nf);
                }
                self.chains_map.insert(nf, nchain);
            }
            // Calls — remap caller_id (position giữ nguyên — đã trỏ đúng chain).
            for c in &result.calls {
                let mut c2 = c.clone();
                if let Some(&nid) = id_map.get(&c.caller_id) {
                    c2.caller_id = nid;
                }
                all_calls.push(c2);
            }
        }
        // Scope index chỉ rebuild sau khi toàn bộ scope id đã là global.
        self.rebuild_scope_index();

        // ── Phase 2: resolve placeholder 0 trong chains ──
        self.resolve_calls(&all_calls);

        // ── Phase 3: build edges + call records + call-name index ──
        self.build_edges_from_calls(&all_calls).await?;

        // ── Phase 4: files ──
        for result in results {
            let f = FileInfo {
                path: result.path.clone(),
                language: result.language.clone(),
                bytes: result.bytes,
                lines: result.lines,
            };
            self.storage
                .write()
                .await
                .upsert_file(&f)
                .await
                .map_err(serr)?;
            self.files.push(f);
        }

        // ── Phase 5: engines + version bump ──
        self.rebuild_chain_engine().await?;
        self.rebuild_name_engine().await?;
        self.version += 1;
        self.storage
            .write()
            .await
            .set_version(self.version)
            .await
            .map_err(serr)?;
        Ok(())
    }

    /// Gán id global cho symbol, lưu storage + index tên. Không đụng scope index
    /// — scope id còn local, `rebuild_scope_index` chạy sau khi remap.
    async fn register(&mut self, mut sym: Symbol) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        sym.id = id;
        {
            let mut st = self.storage.write().await;
            st.save_symbol(&sym).await.map_err(serr)?;
            st.save_next_id(self.next_id).await.map_err(serr)?;
        }
        if !sym.name.is_empty() {
            self.name_index
                .entry(sym.name.to_lowercase())
                .or_default()
                .push(id);
        }
        self.symbols.insert(id, sym);
        Ok(id)
    }

    /// Remap scope_id/type_ref của symbol (đã lưu) sang id global, rồi **ghi lại
    /// storage** — nếu không, reopen đọc phải scope_id local cũ (sai khi multi-file,
    /// vì id local trùng nhau giữa các file).
    async fn remap_scope_type(&mut self, new_id: u64, id_map: &HashMap<u64, u64>) -> Result<()> {
        let to_save = {
            let Some(sym) = self.symbols.get_mut(&new_id) else {
                return Ok(());
            };
            if sym.scope_id != 0 && let Some(&g) = id_map.get(&sym.scope_id) {
                sym.scope_id = g;
            }
            if sym.type_ref != 0 && let Some(&g) = id_map.get(&sym.type_ref) {
                sym.type_ref = g;
            }
            sym.clone()
        };
        self.storage
            .write()
            .await
            .save_symbol(&to_save)
            .await
            .map_err(serr)?;
        Ok(())
    }

    /// Thay placeholder `0` trong chain bằng id thật (resolve per-caller).
    fn resolve_calls(&mut self, calls: &[CallRecord]) {
        let mut caller_calls: HashMap<u64, Vec<&CallRecord>> = HashMap::new();
        for c in calls {
            if c.caller_id != 0 {
                caller_calls.entry(c.caller_id).or_default().push(c);
            }
        }
        for (caller_id, ccs) in caller_calls {
            // Collect dưới borrow immutable (resolve cần đọc `self`), rồi apply
            // qua get_mut — tránh E0502 (chain mutable + self immutable).
            let mut resolved: Vec<(usize, u64)> = Vec::new();
            {
                let Some(chain) = self.chains_map.get(&caller_id) else {
                    continue;
                };
                for c in ccs {
                    if c.position >= chain.len() || chain[c.position] != 0 {
                        continue;
                    }
                    if let Some(real) = self.resolve_call_placeholder(c, caller_id) {
                        resolved.push((c.position, real));
                    }
                }
            }
            if let Some(chain) = self.chains_map.get_mut(&caller_id) {
                for (pos, real) in resolved {
                    chain[pos] = real;
                }
            }
        }
    }

    /// Resolve một call record về real symbol id — trả `None` nếu không được.
    ///
    /// Ưu tiên: structural hint (TargetClass/TargetMethod — VD Java class
    /// literal) → exact name → short name (phần sau dấu chấm) → best-candidate
    /// (@Override +10 / has-chain +5 / same-file +3).
    fn resolve_call_placeholder(&self, call: &CallRecord, caller_id: u64) -> Option<u64> {
        if let (Some(tc), Some(tm)) = (&call.target_class, &call.target_method)
            && let Some(id) = self.lookup_method_of_class(tc, tm)
        {
            return Some(id);
        }

        let mut candidates: Vec<u64> = self
            .name_index
            .get(&call.call_name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if candidates.is_empty() {
            let short = call.call_name.rsplit('.').next().unwrap_or("").to_lowercase();
            if !short.is_empty() {
                candidates = self.name_index.get(&short).cloned().unwrap_or_default();
            }
        }
        if candidates.is_empty() {
            return None;
        }
        Some(self.pick_best_candidate(&candidates, caller_id))
    }

    /// Tìm method của class theo tên (scope_id == class id).
    fn lookup_method_of_class(&self, class_name: &str, method_name: &str) -> Option<u64> {
        let class_ids = self.name_index.get(&class_name.to_lowercase())?;
        let method_ids = self.name_index.get(&method_name.to_lowercase())?;
        for &cid in class_ids {
            let class_sym = self.symbols.get(&cid)?;
            if !matches!(class_sym.kind, SymbolKind::Class | SymbolKind::Interface) {
                continue;
            }
            for &mid in method_ids {
                let m = self.symbols.get(&mid)?;
                if matches!(m.kind, SymbolKind::Function | SymbolKind::Method) && m.scope_id == cid {
                    return Some(mid);
                }
            }
        }
        None
    }

    /// Chọn ứng viên tốt nhất trong danh sách trùng tên.
    fn pick_best_candidate(&self, candidates: &[u64], caller_id: u64) -> u64 {
        if candidates.len() == 1 {
            return candidates[0];
        }
        let caller_file = self.symbols.get(&caller_id).map(|s| s.file.clone());
        let mut best = candidates[0];
        let mut best_score = i32::MIN;
        for &id in candidates {
            let Some(sym) = self.symbols.get(&id) else {
                continue;
            };
            let mut score = 0;
            if sym.annotations.iter().any(|a| a.name == "Override") {
                score += 10;
            }
            if self.chains_map.contains_key(&id) {
                score += 5;
            }
            if let Some(f) = &caller_file && &sym.file == f {
                score += 3;
            }
            if score > best_score {
                best_score = score;
                best = id;
            }
        }
        best
    }

    /// Build edges từ chains (đã resolve) + call records; persist call records +
    /// call-name index (kèm alias type-qualified `svc.validate` → `type.validate`).
    ///
    /// Edge model: mọi symbol element trong chain là một callee (thống nhất với
    /// `rebuild_edges` khi reopen) — call record chỉ bổ sung metadata theo
    /// position. Chain dựng thẳng (không qua placeholder) vẫn sinh edge đủ.
    async fn build_edges_from_calls(&mut self, calls: &[CallRecord]) -> Result<()> {
        let mut recs_by_caller: HashMap<u64, Vec<CallRecord>> = HashMap::new();
        for c in calls {
            let caller = c.caller_id;
            recs_by_caller.entry(caller).or_default().push(c.clone());

            // Call-site index: key theo tên thô + alias type-qualified (nếu có).
            let site = CallSite {
                caller_id: caller,
                call_name: c.call_name.clone(),
                line: c.line,
                condition: c.condition.clone(),
                is_loop_body: c.is_loop_body,
                arg_exprs: c.arg_exprs.clone(),
            };
            let raw_key = c.call_name.to_lowercase();
            self.call_names
                .entry(raw_key.clone())
                .or_default()
                .push(site.clone());
            if let Some(alias) = self.alias_qualified_name(caller, &c.call_name)
                && alias != raw_key
            {
                self.call_names.entry(alias).or_default().push(site);
            }
        }

        // Edges từ mọi chain — rec lookup theo position cho metadata.
        for (&caller, chain) in &self.chains_map {
            let rec_by_pos: HashMap<usize, &CallRecord> = recs_by_caller
                .get(&caller)
                .map(|rs| rs.iter().map(|c| (c.position, c)).collect())
                .unwrap_or_default();
            for (i, &e) in chain.iter().enumerate() {
                // Vị trí 0 = chính func id (owner) — không phải call. Recursion
                // thật xuất hiện ở vị trí > 0 (vẫn giữ là edge is_recursive).
                if i == 0 || is_marker(e) || e == 0 {
                    continue;
                }
                let rec = rec_by_pos.get(&i);
                let arg_ids = rec
                    .map(|r| self.resolve_arg_ids(caller, &r.arg_exprs))
                    .unwrap_or_default();
                self.edges.insert(
                    (caller, e),
                    EdgeMeta {
                        caller_id: caller,
                        callee_id: e,
                        position: i,
                        condition: rec.and_then(|r| r.condition.clone()),
                        effect: rec.map(|r| r.effect).unwrap_or_default(),
                        effect_desc: rec.and_then(|r| r.effect_desc.clone()),
                        arg_ids,
                        is_loop_body: rec.map(|r| r.is_loop_body).unwrap_or(false),
                        is_recursive: e == caller,
                    },
                );
            }
        }

        // Persist call records (gom theo caller).
        for (caller, recs) in recs_by_caller {
            let bytes = serde_json::to_vec(&recs).map_err(|e| Error::Search(e.to_string()))?;
            self.storage
                .write()
                .await
                .set_call_records(caller, &bytes)
                .await
                .map_err(serr)?;
        }
        // Persist call-name index.
        for (name, sites) in &self.call_names {
            let bytes = serde_json::to_vec(sites).map_err(|e| Error::Search(e.to_string()))?;
            self.storage
                .write()
                .await
                .set_call_name_index(name, &bytes)
                .await
                .map_err(serr)?;
        }
        Ok(())
    }

    /// Alias type-qualified: `svc.validate` → `orderservice.validate` khi caller
    /// có var `svc` với `type_name = "orderservice.OrderService"` trong scope.
    fn alias_qualified_name(&self, caller_id: u64, call_name: &str) -> Option<String> {
        let dot = call_name.find('.')?;
        let var = &call_name[..dot];
        let mut scopes = vec![caller_id];
        if let Some(s) = self.symbols.get(&caller_id)
            && s.scope_id != 0
        {
            scopes.push(s.scope_id);
        }
        for sid in scopes {
            if let Some(ids) = self.scope_index.get(&sid) {
                for id in ids {
                    let sym = self.symbols.get(id)?;
                    if sym.name == var && let Some(tn) = &sym.type_name {
                        let rest = &call_name[dot + 1..];
                        return Some(format!("{}.{}", tn.to_lowercase(), rest.to_lowercase()));
                    }
                }
            }
        }
        None
    }

    /// Resolve arg expr (tên var/param) về symbol id trong scope của caller.
    fn resolve_arg_ids(&self, caller_id: u64, arg_exprs: &[String]) -> Vec<u64> {
        let mut scopes = vec![caller_id];
        if let Some(s) = self.symbols.get(&caller_id)
            && s.scope_id != 0
        {
            scopes.push(s.scope_id);
        }
        let mut out = Vec::with_capacity(arg_exprs.len());
        for expr in arg_exprs {
            let mut found = 0;
            'outer: for sid in &scopes {
                if let Some(ids) = self.scope_index.get(sid) {
                    for id in ids {
                        if self.symbols.get(id).is_some_and(|s| s.name == *expr) {
                            found = *id;
                            break 'outer;
                        }
                    }
                }
            }
            out.push(found);
        }
        out
    }

    // ── Queries ──

    /// Tìm symbol theo tên (substring, case-insensitive) qua name engine; lọc
    /// theo kind nếu `Some`. `limit = 0` = không giới hạn (vẫn chặn bởi engine).
    pub async fn search_symbol(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        limit: usize,
    ) -> Result<Vec<Symbol>> {
        let q = query.to_lowercase();
        let hits = match self.names.search(q.as_bytes(), None).await {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };
        let limit = if limit == 0 { usize::MAX } else { limit };
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (record, _) in hits {
            if record == 0 {
                continue;
            }
            let Some(name) = self.name_records.get(record - 1) else {
                continue;
            };
            let Some(ids) = self.name_index.get(name) else {
                continue;
            };
            for &id in ids {
                if !seen.insert(id) {
                    continue;
                }
                let Some(s) = self.symbols.get(&id) else {
                    continue;
                };
                if kind.is_some_and(|k| s.kind != k) {
                    continue;
                }
                out.push(s.clone());
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        Ok(out)
    }

    /// Symbol theo id.
    pub fn symbol_by_id(&self, id: u64) -> Option<Symbol> {
        self.symbols.get(&id).cloned()
    }

    /// Resolve theo id (kèm kiểm tra tên) hoặc theo tên chính xác (case-
    /// insensitive). Trùng tên → `ambiguous = true` + toàn bộ matches.
    pub fn resolve_by_name_or_id(&self, name: &str, symbol_id: u64) -> Result<ResolveResult> {
        if symbol_id != 0 {
            let s = self
                .symbols
                .get(&symbol_id)
                .cloned()
                .ok_or_else(|| Error::Invalid(format!("symbol id {symbol_id} not found")))?;
            if !name.is_empty() && s.name != name {
                return Err(Error::Invalid(format!(
                    "symbol id {symbol_id} has name {:?}, not {name:?}",
                    s.name
                )));
            }
            return Ok(ResolveResult {
                symbol: Some(s),
                matches: Vec::new(),
                ambiguous: false,
            });
        }

        let ids = self
            .name_index
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(Error::Invalid(format!("symbol {name:?} not found")));
        }
        let matches: Vec<Symbol> = ids
            .iter()
            .filter_map(|id| self.symbols.get(id).cloned())
            .collect();
        if matches.is_empty() {
            return Err(Error::Invalid(format!("symbol {name:?} not found")));
        }
        if matches.len() > 1 {
            return Ok(ResolveResult {
                symbol: None,
                matches,
                ambiguous: true,
            });
        }
        Ok(ResolveResult {
            symbol: Some(matches[0].clone()),
            matches: Vec::new(),
            ambiguous: false,
        })
    }

    /// Callers (transitive BFS) — `depth` = số hop tối đa (1 = direct).
    pub async fn callers(&self, id: u64, depth: usize) -> Result<Vec<Symbol>> {
        if !self.symbols.contains_key(&id) {
            return Err(Error::Invalid(format!("symbol id {id} not found")));
        }
        let mut visited = HashSet::new();
        visited.insert(id);
        let mut frontier = vec![id];
        let mut out_ids = Vec::new();
        for _ in 0..depth.max(1) {
            let mut next = Vec::new();
            for &cur in &frontier {
                for caller in self.direct_callers(cur).await? {
                    if visited.insert(caller) {
                        out_ids.push(caller);
                        next.push(caller);
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        Ok(out_ids
            .into_iter()
            .filter_map(|i| self.symbols.get(&i).cloned())
            .collect())
    }

    /// Callers trực tiếp của `id` — substring search `[id]` trên chain engine.
    ///
    /// Mọi chain chứa id ở vị trí callee (hoặc vị trí 0 — chính chain của id,
    /// bỏ qua khi `caller == id`).
    async fn direct_callers(&self, id: u64) -> Result<Vec<u64>> {
        let pattern = [id];
        let hits = match self.chains.search(&pattern, None).await {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for (record, _) in hits {
            let caller = record as u64;
            if caller != id && self.symbols.contains_key(&caller) {
                out.push(caller);
            }
        }
        Ok(out)
    }

    /// Callees trực tiếp — đọc chain, skip marker/0/self/seen. Không có chain
    /// (symbol không phải function / không có body) → rỗng, không lỗi.
    pub async fn callees(&self, id: u64) -> Result<Vec<Symbol>> {
        let Some(chain) = self.chains_map.get(&id).cloned() else {
            return Ok(Vec::new());
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in chain {
            if is_marker(e) || e == 0 || e == id || !seen.insert(e) {
                continue;
            }
            if let Some(s) = self.symbols.get(&e) {
                out.push(s.clone());
            }
        }
        Ok(out)
    }

    /// Flow của một hàm — chain render (marker name / symbol name / call thô
    /// cho unresolved) + call edges kèm line/condition/effect/args.
    pub async fn flow(&self, id: u64) -> Result<FlowResult> {
        let sym = self
            .symbols
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("symbol id {id} not found")))?;
        let chain = self
            .chains_map
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("chain for {:?} not found", sym.name)))?;

        // Call records (position → record) — hiện call không resolve được thành
        // symbol với tên thật thay vì "unknown(0)".
        let recs: Vec<CallRecord> = match self
            .storage
            .read()
            .await
            .get_call_records(id)
            .await
            .map_err(serr)?
        {
            Some(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            None => Vec::new(),
        };
        let rec_by_pos: HashMap<usize, &CallRecord> = recs.iter().map(|r| (r.position, r)).collect();

        let chain_desc = chain
            .iter()
            .enumerate()
            .map(|(i, &e)| {
                if is_marker(e) {
                    marker_name(e).unwrap_or("MARKER").to_string()
                } else if let Some(s) = self.symbols.get(&e) {
                    s.name.clone()
                } else if let Some(rec) = rec_by_pos.get(&i) {
                    if !rec.call_name.is_empty() {
                        rec.call_name.clone()
                    } else {
                        format!("unknown({e})")
                    }
                } else {
                    format!("unknown({e})")
                }
            })
            .collect();

        let mut calls = Vec::new();
        for (i, &e) in chain.iter().enumerate() {
            if is_marker(e) || e == id {
                continue;
            }
            if e == 0 {
                if let Some(rec) = rec_by_pos.get(&i) {
                    calls.push(FlowCall {
                        position: i,
                        to_name: rec.call_name.clone(),
                        to_id: None,
                        line: rec.line,
                        condition: rec.condition.clone(),
                        effect: rec.effect,
                        effect_desc: rec.effect_desc.clone(),
                        args: rec.arg_exprs.clone(),
                    });
                }
                continue;
            }
            let Some(callee) = self.symbols.get(&e) else {
                continue;
            };
            let meta = self.edges.get(&(id, e));
            let rec = rec_by_pos.get(&i);
            let mut cond = meta.and_then(|m| m.condition.clone());
            let mut effect = meta.map(|m| m.effect).unwrap_or_default();
            let mut effect_desc = meta.and_then(|m| m.effect_desc.clone());
            if let Some(r) = rec {
                if cond.is_none() {
                    cond = r.condition.clone();
                }
                if effect == EffectType::None {
                    effect = r.effect;
                }
                if effect_desc.is_none() {
                    effect_desc = r.effect_desc.clone();
                }
            }
            calls.push(FlowCall {
                position: i,
                to_name: callee.name.clone(),
                to_id: Some(e),
                line: rec.map(|r| r.line).unwrap_or(0),
                condition: cond,
                effect,
                effect_desc,
                args: rec.map(|r| r.arg_exprs.clone()).unwrap_or_default(),
            });
        }

        Ok(FlowResult {
            symbol: sym,
            chain,
            chain_desc,
            calls,
        })
    }

    /// Tìm hàm có chain chứa pattern (KMP substring qua chain engine).
    pub async fn search_flow(&self, pattern: &[u64]) -> Result<Vec<SearchFlowResult>> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        let hits = match self.chains.search(pattern, None).await {
            Ok(h) => h,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out = Vec::new();
        for (record, _) in hits {
            let func_id = record as u64;
            if let Some(sym) = self.symbols.get(&func_id) {
                let chain = self.chains_map.get(&func_id).cloned().unwrap_or_default();
                out.push(SearchFlowResult {
                    function_id: func_id,
                    function_name: sym.name.clone(),
                    chain,
                    match_count: 1,
                });
            }
        }
        Ok(out)
    }

    /// Tìm function gọi một library call có tên chứa `query` (case-insensitive
    /// substring trên call-name index, kể cả call unresolved). Gom theo caller,
    /// sort theo FuncName rồi FuncID.
    pub async fn callers_by_call_name(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<CallSiteResult>> {
        let q = query.to_lowercase();
        let mut matched: Vec<(&String, &Vec<CallSite>)> = self
            .call_names
            .iter()
            .filter(|(name, _)| name.contains(&q))
            .collect();
        matched.sort_by_key(|(name, _)| (*name).clone());

        let mut by_func: HashMap<u64, CallSiteResult> = HashMap::new();
        for (_, sites) in matched {
            for site in sites {
                let entry = by_func.entry(site.caller_id).or_insert_with(|| {
                    let sym = self.symbols.get(&site.caller_id);
                    CallSiteResult {
                        func_id: site.caller_id,
                        func_name: sym.map(|s| s.name.clone()).unwrap_or_default(),
                        file: sym.map(|s| s.file.clone()).unwrap_or_default(),
                        call_sites: Vec::new(),
                    }
                });
                entry.call_sites.push(site.clone());
            }
        }
        let mut out: Vec<CallSiteResult> = by_func.into_values().collect();
        out.sort_by(|a, b| a.func_name.cmp(&b.func_name).then(a.func_id.cmp(&b.func_id)));
        let limit = if limit == 0 { usize::MAX } else { limit };
        out.truncate(limit);
        Ok(out)
    }

    /// Files trong graph.
    pub fn files(&self) -> Vec<FileInfo> {
        self.files.clone()
    }

    // ── Class / scope queries (tương ứng semgraph_get_class* / function_scope) ──

    /// Toàn bộ symbol con của một scope id (methods/fields/nested của class,
    /// params/locals của function).
    pub fn members_of(&self, id: u64) -> Vec<Symbol> {
        self.scope_index
            .get(&id)
            .map(|ids| {
                ids.iter()
                    .filter_map(|i| self.symbols.get(i).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Methods của class (kind Function/Method), projection `MemberInfo` gọn.
    pub fn list_methods_of_class(&self, id: u64) -> Vec<MemberInfo> {
        let mut members: Vec<MemberInfo> = self
            .members_of(id)
            .into_iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
            .map(|s| MemberInfo::from_symbol(&s))
            .collect();
        members.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        members
    }

    /// Thông tin class: symbol + fields và methods tách riêng. `None` nếu symbol
    /// không phải class/interface/enum (function có scope params → không class).
    pub fn get_class_info(&self, id: u64) -> Option<ClassInfo> {
        let class = self.symbols.get(&id)?;
        if !matches!(
            class.kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        ) {
            return None;
        }
        let class = class.clone();
        let members = self.members_of(id);
        let fields: Vec<MemberInfo> = members
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Field | SymbolKind::Variable | SymbolKind::Constant))
            .map(|s| MemberInfo::from_symbol(s))
            .collect();
        let methods: Vec<MemberInfo> = members
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Function | SymbolKind::Method))
            .map(|s| MemberInfo::from_symbol(s))
            .collect();
        Some(ClassInfo {
            class,
            fields,
            methods,
        })
    }

    /// Scope của function: parameters + locals (kind Variable/Constant).
    pub fn function_scope(&self, id: u64) -> Option<FunctionScope> {
        let function = self.symbols.get(&id)?.clone();
        let members = self.members_of(id);
        let parameters = members
            .iter()
            .filter(|s| s.kind == SymbolKind::Parameter)
            .cloned()
            .collect();
        let locals = members
            .iter()
            .filter(|s| matches!(s.kind, SymbolKind::Variable | SymbolKind::Constant))
            .cloned()
            .collect();
        Some(FunctionScope {
            function,
            parameters,
            locals,
        })
    }

    /// Liệt kê symbol theo kind (class/interface/enum/...), phân trang —
    /// sort theo name rồi id để ổn định giữa các trang.
    pub fn list_symbols_by_kind(
        &self,
        kind: SymbolKind,
        limit: usize,
        offset: usize,
    ) -> (Vec<Symbol>, usize) {
        let mut all: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|s| s.kind == kind)
            .cloned()
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        (all.into_iter().skip(offset).take(limit).collect(), total)
    }

    /// Tìm symbol theo annotation (case-insensitive substring trên tên
    /// annotation). Trả về (page, total, truncated) — total là con số thật,
    /// truncated=true khi còn trang sau.
    pub fn search_by_annotation(
        &self,
        annotation: &str,
        kind: Option<SymbolKind>,
        offset: usize,
        limit: usize,
    ) -> (Vec<Symbol>, usize, bool) {
        let q = annotation.to_lowercase();
        let mut all: Vec<Symbol> = self
            .symbols
            .values()
            .filter(|s| {
                s.annotations
                    .iter()
                    .any(|a| a.name.to_lowercase().contains(&q))
            })
            .filter(|s| kind.is_none_or(|k| s.kind == k))
            .cloned()
            .collect();
        all.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        let page = all.into_iter().skip(offset).take(limit).collect::<Vec<_>>();
        let truncated = offset + page.len() < total;
        (page, total, truncated)
    }

    /// Ước lượng dependencies từ call names: tách module prefix (phần trước dấu
    /// chấm đầu tiên) — internal nếu có symbol trong repo mang chính tên đó,
    /// external còn lại. Sort theo số call sites giảm dần.
    pub fn dependencies_report(&self) -> DependenciesReport {
        let mut internal: BTreeMap<String, usize> = BTreeMap::new();
        let mut external: BTreeMap<String, usize> = BTreeMap::new();
        // Dedup theo (caller, line, raw call_name): alias type-qualified index
        // (`svc.validate` → `type.validate`) đẩy cùng site vào nhiều key — mỗi
        // call site chỉ tính một lần, dùng tên thô để rút module prefix.
        let mut seen: HashSet<(u64, u32, String)> = HashSet::new();
        for (_, sites) in &self.call_names {
            for site in sites {
                if !seen.insert((site.caller_id, site.line, site.call_name.clone())) {
                    continue;
                }
                let Some((mod_part, _)) = site.call_name.split_once('.') else {
                    continue;
                };
                let mod_part = mod_part.to_lowercase();
                let entry = if self.name_index.contains_key(&mod_part) {
                    &mut internal
                } else {
                    &mut external
                };
                *entry.entry(mod_part).or_default() += 1;
            }
        }
        let to_list = |m: BTreeMap<String, usize>| -> Vec<Dependency> {
            let mut v: Vec<Dependency> = m
                .into_iter()
                .map(|(name, count)| Dependency { name, count })
                .collect();
            v.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
            v
        };
        let internal = to_list(internal);
        let external = to_list(external);
        let total = internal.len() + external.len();
        DependenciesReport {
            internal,
            external,
            total,
        }
    }

    /// Search symbol nâng cao: lọc theo kind + match mode (contains/prefix/
    /// suffix/exact) + phân trang. Trả về (page, total) — total là số khớp
    /// trước phân trang, page sort theo (name, id) cho pagination ổn định.
    pub async fn search_symbol_paged(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        mode: SymbolMatch,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Symbol>, usize)> {
        let q = query.to_lowercase();
        let mut seen = HashSet::new();
        let mut ids: Vec<u64> = Vec::new();
        match mode {
            // Substring qua name engine (radix — nhanh hơn duyệt toàn bộ tên).
            SymbolMatch::Contains => {
                let hits = match self.names.search(q.as_bytes(), None).await {
                    Ok(h) => h,
                    Err(_) => return Ok((Vec::new(), 0)),
                };
                for (record, _) in hits {
                    if record == 0 {
                        continue;
                    }
                    let Some(name) = self.name_records.get(record - 1) else {
                        continue;
                    };
                    let Some(name_ids) = self.name_index.get(name) else {
                        continue;
                    };
                    for &id in name_ids {
                        if seen.insert(id) {
                            ids.push(id);
                        }
                    }
                }
            }
            // Prefix/suffix/exact duyệt name_index (bộ nhỏ hơn symbol registry).
            SymbolMatch::Prefix | SymbolMatch::Suffix | SymbolMatch::Exact => {
                for (name, name_ids) in &self.name_index {
                    let matched = match mode {
                        SymbolMatch::Prefix => name.starts_with(&q),
                        SymbolMatch::Suffix => name.ends_with(&q),
                        SymbolMatch::Exact => name == &q,
                        _ => false,
                    };
                    if !matched {
                        continue;
                    }
                    for &id in name_ids {
                        if seen.insert(id) {
                            ids.push(id);
                        }
                    }
                }
            }
        }
        let mut all: Vec<u64> = ids
            .into_iter()
            .filter(|&id| match kind {
                Some(k) => self.symbols.get(&id).is_some_and(|s| s.kind == k),
                None => true,
            })
            .collect();
        all.sort_by(|&a, &b| {
            let na = self.symbols.get(&a).map(|s| s.name.as_str()).unwrap_or("");
            let nb = self.symbols.get(&b).map(|s| s.name.as_str()).unwrap_or("");
            na.cmp(nb).then(a.cmp(&b))
        });
        let total = all.len();
        let limit = if limit == 0 { usize::MAX } else { limit };
        let page = all
            .into_iter()
            .skip(offset)
            .take(limit)
            .filter_map(|id| self.symbols.get(&id).cloned())
            .collect();
        Ok((page, total))
    }

    /// Số liệu tổng hợp.
    pub fn stats(&self) -> SemgraphStats {
        SemgraphStats {
            symbols: self.symbols.len() as u64,
            chains: self.chains_map.len() as u64,
            edges: self.edges.len() as u64,
            files: self.files.len() as u64,
            next_id: self.next_id,
        }
    }

    /// Version index hiện tại (bump mỗi lần ingest).
    pub fn version(&self) -> u64 {
        self.version
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{
        Annotation, ScopeLevel, MARKER_BRANCH_END, MARKER_IF_TRUE, MARKER_LOOP, MARKER_LOOP_BACK,
    };

    fn sym(file: &str, name: &str, id: u64) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: file.to_string(),
            line: 1,
            end_line: 1,
            signature: None,
            doc: None,
            annotations: Vec::new(),
            language: "test".to_string(),
        }
    }

    fn result(
        path: &str,
        symbols: Vec<Symbol>,
        chains: HashMap<u64, Vec<u64>>,
        calls: Vec<CallRecord>,
    ) -> ParseResult {
        ParseResult {
            path: path.to_string(),
            language: "test".to_string(),
            bytes: 0,
            lines: 0,
            symbols,
            chains,
            calls,
        }
    }

    #[tokio::test]
    async fn ingest_and_query_basic() {
        let mut idx = GraphIndex::in_memory();
        // a gọi b; b có if → gọi c (local ids trùng global khi ingest 1 file).
        let chains = HashMap::from([
            (SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1]),
            (
                SYMBOL_BASE + 1,
                vec![
                    SYMBOL_BASE + 1,
                    MARKER_IF_TRUE,
                    SYMBOL_BASE + 2,
                    MARKER_BRANCH_END,
                ],
            ),
        ]);
        let r = result(
            "a.ts",
            vec![
                sym("a.ts", "a", SYMBOL_BASE),
                sym("b.ts", "b", SYMBOL_BASE + 1),
                sym("c.ts", "c", SYMBOL_BASE + 2),
            ],
            chains,
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();

        // search_symbol (substring, case-insensitive).
        let hits = idx.search_symbol("b", None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "b");
        assert!(idx.search_symbol("zzz", None, 10).await.unwrap().is_empty());

        // callees của a = [b]; của b = [c].
        let cees = idx.callees(SYMBOL_BASE).await.unwrap();
        assert_eq!(cees.len(), 1);
        assert_eq!(cees[0].name, "b");
        assert_eq!(idx.callees(SYMBOL_BASE + 1).await.unwrap()[0].name, "c");
        assert!(idx.callees(SYMBOL_BASE + 2).await.unwrap().is_empty());

        // callers direct + BFS.
        let callers1 = idx.callers(SYMBOL_BASE + 2, 1).await.unwrap();
        assert_eq!(callers1.len(), 1);
        assert_eq!(callers1[0].name, "b");
        let callers2 = idx.callers(SYMBOL_BASE + 2, 2).await.unwrap();
        assert_eq!(callers2.len(), 2);
        assert!(idx.callers(SYMBOL_BASE + 2, 3).await.unwrap().len() <= 2);

        // flow render.
        let flow = idx.flow(SYMBOL_BASE + 1).await.unwrap();
        assert_eq!(flow.symbol.name, "b");
        assert_eq!(
            flow.chain,
            vec![
                SYMBOL_BASE + 1,
                MARKER_IF_TRUE,
                SYMBOL_BASE + 2,
                MARKER_BRANCH_END
            ]
        );
        assert_eq!(flow.chain_desc[0], "b");
        assert_eq!(flow.chain_desc[1], "IF_TRUE");
        assert_eq!(flow.chain_desc[2], "c");
        assert_eq!(flow.chain_desc[3], "BRANCH_END");
        assert_eq!(flow.calls.len(), 1);
        assert_eq!(flow.calls[0].to_name, "c");
        assert_eq!(flow.calls[0].to_id, Some(SYMBOL_BASE + 2));

        // search_flow pattern [IF_TRUE, c].
        let sf = idx
            .search_flow(&[MARKER_IF_TRUE, SYMBOL_BASE + 2])
            .await
            .unwrap();
        assert_eq!(sf.len(), 1);
        assert_eq!(sf[0].function_name, "b");
        assert!(idx.search_flow(&[MARKER_LOOP]).await.unwrap().is_empty());

        // stats.
        let st = idx.stats();
        assert_eq!(st.symbols, 3);
        assert_eq!(st.chains, 2);
        assert_eq!(st.edges, 2); // a→b, b→c
        assert_eq!(st.next_id, SYMBOL_BASE + 3);
        assert_eq!(idx.version(), 1);

        // symbol_by_id / resolve.
        assert_eq!(idx.symbol_by_id(SYMBOL_BASE).unwrap().name, "a");
        let res = idx.resolve_by_name_or_id("a", 0).unwrap();
        assert!(!res.ambiguous);
        assert_eq!(res.symbol.unwrap().id, SYMBOL_BASE);
        assert!(idx.resolve_by_name_or_id("nope", 0).is_err());
    }

    #[tokio::test]
    async fn resolve_placeholder_from_call_record() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "g".to_string(),
                position: 1,
                arg_exprs: vec![],
                line: 1,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            },
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "h".to_string(),
                position: 2,
                arg_exprs: vec![],
                line: 2,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            },
        ];
        let r = result(
            "f.ts",
            vec![
                sym("f.ts", "f", SYMBOL_BASE),
                sym("g.ts", "g", SYMBOL_BASE + 1),
                sym("h.ts", "h", SYMBOL_BASE + 2),
            ],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0, 0])]),
            calls,
        );
        idx.ingest(&[r]).await.unwrap();

        // Placeholder 0 đã được resolve về id thật (exact name match).
        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(flow.chain, vec![SYMBOL_BASE, SYMBOL_BASE + 1, SYMBOL_BASE + 2]);
        assert_eq!(flow.chain_desc, vec!["f", "g", "h"]);
        let cees = idx.callees(SYMBOL_BASE).await.unwrap();
        assert_eq!(cees.len(), 2);
    }

    #[tokio::test]
    async fn unresolved_call_keeps_raw_name_in_flow() {
        let mut idx = GraphIndex::in_memory();
        // "fmt.Println" không có symbol tương ứng → không resolve được.
        let calls = vec![CallRecord {
            caller_id: SYMBOL_BASE,
            call_name: "fmt.Println".to_string(),
            position: 1,
            arg_exprs: vec!["msg".to_string()],
            line: 7,
            condition: None,
            is_loop_body: false,
            effect: EffectType::Log,
            effect_desc: None,
            target_class: None,
            target_method: None,
        }];
        let r = result(
            "f.ts",
            vec![sym("f.ts", "f", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            calls,
        );
        idx.ingest(&[r]).await.unwrap();

        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(flow.chain, vec![SYMBOL_BASE, 0]);
        assert_eq!(flow.chain_desc[1], "fmt.Println");
        assert_eq!(flow.calls.len(), 1);
        assert_eq!(flow.calls[0].to_name, "fmt.Println");
        assert_eq!(flow.calls[0].to_id, None);
        assert_eq!(flow.calls[0].line, 7);
        assert_eq!(flow.calls[0].effect, EffectType::Log);

        // callees chỉ trả symbol đã resolve.
        assert!(idx.callees(SYMBOL_BASE).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn resolve_ambiguous_on_duplicate_name() {
        let mut idx = GraphIndex::in_memory();
        let r1 = result(
            "a.ts",
            vec![sym("a.ts", "process", SYMBOL_BASE)],
            HashMap::new(),
            vec![],
        );
        let r2 = result(
            "b.ts",
            vec![sym("b.ts", "process", SYMBOL_BASE)],
            HashMap::new(),
            vec![],
        );
        idx.ingest(&[r1, r2]).await.unwrap();

        // Cùng tên 2 file → ambiguous với đủ matches.
        let res = idx.resolve_by_name_or_id("process", 0).unwrap();
        assert!(res.ambiguous);
        assert_eq!(res.matches.len(), 2);
        assert!(res.symbol.is_none());

        // Resolve theo id → không ambiguous.
        let res2 = idx.resolve_by_name_or_id("process", SYMBOL_BASE).unwrap();
        assert!(!res2.ambiguous);
        assert_eq!(res2.symbol.unwrap().id, SYMBOL_BASE);

        // search_symbol mở rộng cả 2 symbol trùng tên.
        let hits = idx.search_symbol("process", None, 10).await.unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[tokio::test]
    async fn callers_by_call_name_finds_unresolved_calls() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![CallRecord {
            caller_id: SYMBOL_BASE,
            call_name: "fmt.Println".to_string(),
            position: 1,
            arg_exprs: vec![],
            line: 3,
            condition: None,
            is_loop_body: false,
            effect: EffectType::Log,
            effect_desc: None,
            target_class: None,
            target_method: None,
        }];
        let r = result(
            "f.ts",
            vec![sym("f.ts", "f", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0])]),
            calls,
        );
        idx.ingest(&[r]).await.unwrap();

        let hits = idx.callers_by_call_name("println", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].func_name, "f");
        assert_eq!(hits[0].call_sites.len(), 1);
        assert_eq!(hits[0].call_sites[0].call_name, "fmt.Println");
        assert_eq!(hits[0].call_sites[0].line, 3);
    }

    #[tokio::test]
    async fn loop_marker_golden_chain() {
        let mut idx = GraphIndex::in_memory();
        // Python-style: for item: validate(item); save(item)
        // → [F, LOOP, validate, save, LOOP_BACK]
        let chains = HashMap::from([(
            SYMBOL_BASE,
            vec![
                SYMBOL_BASE,
                MARKER_LOOP,
                SYMBOL_BASE + 1,
                SYMBOL_BASE + 2,
                MARKER_LOOP_BACK,
            ],
        )]);
        let r = result(
            "f.py",
            vec![
                sym("f.py", "f", SYMBOL_BASE),
                sym("f.py", "validate", SYMBOL_BASE + 1),
                sym("f.py", "save", SYMBOL_BASE + 2),
            ],
            chains,
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();

        let flow = idx.flow(SYMBOL_BASE).await.unwrap();
        assert_eq!(
            flow.chain,
            vec![
                SYMBOL_BASE,
                MARKER_LOOP,
                SYMBOL_BASE + 1,
                SYMBOL_BASE + 2,
                MARKER_LOOP_BACK
            ]
        );
        assert_eq!(
            flow.chain_desc,
            vec!["f", "LOOP", "validate", "save", "LOOP_BACK"]
        );

        // search_flow: pattern [LOOP, validate] tìm được f.
        let sf = idx
            .search_flow(&[MARKER_LOOP, SYMBOL_BASE + 1])
            .await
            .unwrap();
        assert_eq!(sf.len(), 1);
        assert_eq!(sf[0].function_name, "f");
    }

    /// Reopen file sqlite → rebuild index giữ nguyên dữ liệu + version.
    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn sqlite_persist_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.sqlite");
        let path = path.to_string_lossy().into_owned();
        let chains = HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]);
        let r = result(
            "a.ts",
            vec![
                sym("a.ts", "a", SYMBOL_BASE),
                sym("b.ts", "b", SYMBOL_BASE + 1),
            ],
            chains,
            vec![],
        );
        {
            let mut idx = GraphIndex::open(&path).await.unwrap();
            assert_eq!(idx.version(), 0);
            idx.ingest(&[r]).await.unwrap();
            assert_eq!(idx.version(), 1);
            assert_eq!(idx.callees(SYMBOL_BASE).await.unwrap().len(), 1);
        }
        // Reopen: version giữ nguyên, dữ liệu query lại được.
        let idx = GraphIndex::open(&path).await.unwrap();
        assert_eq!(idx.version(), 1);
        assert_eq!(idx.stats().symbols, 2);
        assert_eq!(idx.stats().chains, 1);
        assert_eq!(idx.stats().edges, 1);
        assert_eq!(idx.callees(SYMBOL_BASE).await.unwrap()[0].name, "b");
        assert_eq!(idx.symbol_by_id(SYMBOL_BASE + 1).unwrap().file, "b.ts");
    }

    /// Ingest 2 lần = full re-index — dữ liệu cũ biến mất, id gán lại từ đầu.
    #[tokio::test]
    async fn ingest_twice_is_full_reindex() {
        let mut idx = GraphIndex::in_memory();
        let r1 = result(
            "a.ts",
            vec![sym("a.ts", "a", SYMBOL_BASE)],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE])]),
            vec![],
        );
        idx.ingest(&[r1]).await.unwrap();
        assert_eq!(idx.stats().symbols, 1);

        let r2 = result(
            "b.ts",
            vec![
                sym("b.ts", "b", SYMBOL_BASE),
                sym("b.ts", "c", SYMBOL_BASE + 1),
            ],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, SYMBOL_BASE + 1])]),
            vec![],
        );
        idx.ingest(&[r2]).await.unwrap();
        assert_eq!(idx.stats().symbols, 2);
        assert!(idx.symbol_by_id(SYMBOL_BASE + 2).is_none());
        let res = idx.resolve_by_name_or_id("a", 0);
        assert!(res.is_err(), "symbol cũ phải biến mất sau full re-index");
        assert_eq!(idx.version(), 2);
    }

    /// Class/scope queries: methods, class info, function scope, list by kind,
    /// annotation search, paged symbol search.
    #[tokio::test]
    async fn class_and_scope_queries() {
        let mut idx = GraphIndex::in_memory();
        let mut cls = sym("svc.rs", "OrderService", SYMBOL_BASE);
        cls.kind = SymbolKind::Class;
        let mut method1 = sym("svc.rs", "getOrders", SYMBOL_BASE + 1);
        method1.kind = SymbolKind::Method;
        method1.scope_id = SYMBOL_BASE;
        method1.signature = Some("fn getOrders(userId: i32) -> Vec<Order>".to_string());
        let mut field = sym("svc.rs", "repo", SYMBOL_BASE + 2);
        field.kind = SymbolKind::Field;
        field.scope_id = SYMBOL_BASE;
        let mut func = sym("util.rs", "validate", SYMBOL_BASE + 3);
        func.kind = SymbolKind::Function;
        let mut param = sym("util.rs", "user", SYMBOL_BASE + 4);
        param.kind = SymbolKind::Parameter;
        param.scope_id = SYMBOL_BASE + 3;
        let mut local = sym("util.rs", "tmp", SYMBOL_BASE + 5);
        local.kind = SymbolKind::Variable;
        local.scope_id = SYMBOL_BASE + 3;
        let mut controller = sym("ctrl.rs", "OrderController", SYMBOL_BASE + 6);
        controller.kind = SymbolKind::Class;
        controller.annotations = vec![Annotation {
            name: "RestController".to_string(),
            args: HashMap::new(),
            line: 1,
        }];
        let mut iface = sym("repo.rs", "OrderRepository", SYMBOL_BASE + 7);
        iface.kind = SymbolKind::Interface;

        let r = result(
            "svc.rs",
            vec![
                cls, method1, field, func, param, local, controller, iface,
            ],
            HashMap::new(),
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();

        // list_methods_of_class — chỉ Function/Method, sort theo tên.
        let methods = idx.list_methods_of_class(SYMBOL_BASE);
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0].name, "getOrders");
        assert_eq!(
            methods[0].signature.as_deref(),
            Some("fn getOrders(userId: i32) -> Vec<Order>")
        );

        // get_class_info — fields và methods tách riêng.
        let info = idx.get_class_info(SYMBOL_BASE).unwrap();
        assert_eq!(info.class.name, "OrderService");
        assert_eq!(info.fields.len(), 1);
        assert_eq!(info.fields[0].name, "repo");
        assert_eq!(info.methods.len(), 1);
        assert!(idx.get_class_info(SYMBOL_BASE + 3).is_none(), "function không phải class");

        // function_scope — parameters + locals.
        let scope = idx.function_scope(SYMBOL_BASE + 3).unwrap();
        assert_eq!(scope.function.name, "validate");
        assert_eq!(scope.parameters.len(), 1);
        assert_eq!(scope.parameters[0].name, "user");
        assert_eq!(scope.locals.len(), 1);
        assert_eq!(scope.locals[0].name, "tmp");

        // list_symbols_by_kind — sort theo tên, phân trang.
        let (classes, total) = idx.list_symbols_by_kind(SymbolKind::Class, 10, 0);
        assert_eq!(total, 2);
        assert_eq!(classes[0].name, "OrderController");
        assert_eq!(classes[1].name, "OrderService");
        let (one, total) = idx.list_symbols_by_kind(SymbolKind::Class, 1, 0);
        assert_eq!(total, 2);
        assert_eq!(one.len(), 1);
        let (second, _) = idx.list_symbols_by_kind(SymbolKind::Class, 1, 1);
        assert_eq!(second[0].name, "OrderService");
        let (_interfaces, total) = idx.list_symbols_by_kind(SymbolKind::Interface, 10, 0);
        assert_eq!(total, 1);

        // search_by_annotation — case-insensitive substring.
        let (hits, total, truncated) = idx.search_by_annotation("restcontroller", None, 0, 10);
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "OrderController");
        assert!(!truncated);
        let (hits, total, truncated) = idx.search_by_annotation("GetMapping", None, 0, 10);
        assert_eq!(total, 0);
        assert!(hits.is_empty());
        assert!(!truncated);
        let (_, total, _) = idx.search_by_annotation("controller", Some(SymbolKind::Class), 0, 1);
        assert_eq!(total, 1, "kind filter loại bỏ match không đúng kind");

        // search_symbol_paged — prefix/suffix/exact + kind filter.
        let (hits, total) = idx
            .search_symbol_paged("order", Some(SymbolKind::Class), SymbolMatch::Prefix, 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 2, "OrderService + OrderController khớp prefix 'order' + kind class");
        assert_eq!(hits[0].name, "OrderController");
        assert_eq!(hits[1].name, "OrderService");
        let (hits, total) = idx
            .search_symbol_paged("service", Some(SymbolKind::Class), SymbolMatch::Suffix, 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "OrderService");
        let (hits, total) = idx
            .search_symbol_paged("validate", None, SymbolMatch::Exact, 10, 0)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(hits[0].name, "validate");
        // contains + pagination. Sort byte-wise (case-sensitive): uppercase
        // "Order*" đứng trước "getOrders".
        let (page0, total) = idx
            .search_symbol_paged("order", None, SymbolMatch::Contains, 2, 0)
            .await
            .unwrap();
        assert_eq!(total, 4, "OrderService, OrderController, OrderRepository + getOrders");
        assert_eq!(page0.len(), 2);
        assert_eq!(page0[0].name, "OrderController");
        assert_eq!(page0[1].name, "OrderRepository");
        let (page1, _) = idx
            .search_symbol_paged("order", None, SymbolMatch::Contains, 2, 2)
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].name, "OrderService");
        assert_eq!(page1[1].name, "getOrders");
    }

    /// dependencies_report — module prefix từ call names, internal vs external.
    #[tokio::test]
    async fn dependencies_report_splits_internal_external() {
        let mut idx = GraphIndex::in_memory();
        let calls = vec![
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "fmt.Println".to_string(),
                position: 1,
                arg_exprs: vec![],
                line: 1,
                condition: None,
                is_loop_body: false,
                effect: EffectType::Log,
                effect_desc: None,
                target_class: None,
                target_method: None,
            },
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "requests.get".to_string(),
                position: 2,
                arg_exprs: vec![],
                line: 2,
                condition: None,
                is_loop_body: false,
                effect: EffectType::HttpCall,
                effect_desc: None,
                target_class: None,
                target_method: None,
            },
            // Internal call: class "OrderService" trong repo, method getOrders.
            CallRecord {
                caller_id: SYMBOL_BASE,
                call_name: "OrderService.getOrders".to_string(),
                position: 3,
                arg_exprs: vec![],
                line: 3,
                condition: None,
                is_loop_body: false,
                effect: EffectType::None,
                effect_desc: None,
                target_class: None,
                target_method: None,
            },
        ];
        let mut cls = sym("svc.rs", "OrderService", SYMBOL_BASE + 1);
        cls.kind = SymbolKind::Class;
        let r = result(
            "f.rs",
            vec![sym("f.rs", "f", SYMBOL_BASE), cls],
            HashMap::from([(SYMBOL_BASE, vec![SYMBOL_BASE, 0, 0, 0])]),
            calls,
        );
        idx.ingest(&[r]).await.unwrap();

        let report = idx.dependencies_report();
        assert_eq!(report.total, 3);
        let internal_names: Vec<&str> = report.internal.iter().map(|d| d.name.as_str()).collect();
        assert!(internal_names.contains(&"orderservice"));
        let external_names: Vec<&str> = report.external.iter().map(|d| d.name.as_str()).collect();
        assert!(external_names.contains(&"fmt"));
        assert!(external_names.contains(&"requests"));
    }
}
