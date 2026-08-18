//! LMDB-backed radix / entity storage (`lmdb-rkv`).
//!
//! Ánh xạ toàn bộ schema của sqlite (`rt_*` / `sg_*`) thành các named-database
//! trong một LMDB environment: mỗi bảng = một DBI, key/value pack LE 8-byte
//! giống sqlite (id/record/shard = `u64` LE).
//!
//! CHÚ Ý — mô hình concurrency:
//! - LMDB là sync/memory-mapped; các thao tác hoàn thành trong µs và KHÔNG
//!   chờ `.await` giữa begin/commit, nên blocking executor không đáng kể so với
//!   sqlx pool.
//! - `LmdbStorage` được `GraphIndex` bọc trong `Arc<RwLock<dyn Storage>>` → mọi
//!   mutation đã tuần tự hoá nên `read-modify-write` của children/shortcuts/
//!   counter không bao giờ va chạm giữa 2 writer.
//! - `tx.commit()` áp dụng buffer trong MỘT `RwTransaction` (atomic).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codegraph_core::{FileInfo, Symbol};
#[cfg(feature = "lmdb")]
use lmdb::EnvironmentFlags;
use lmdb::{Cursor, Database, DatabaseFlags, Environment, Transaction, WriteFlags};

use super::{
    EMPTY, IndexCounts, Result, Storage, StorageError, Tx, TxOp, decode_chain, decode_vector,
    encode_chain, encode_vector,
};

/// Map lỗi LMDB → `StorageError`.
fn e(err: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(err.to_string())
}

// ── key/value packing (LE) ──

#[inline]
fn k8(v: usize) -> [u8; 8] {
    (v as u64).to_le_bytes()
}

#[inline]
fn ku64(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

#[inline]
fn de_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b.try_into().expect("8-byte value"))
}

/// Pack `IndexCounts` (5 × u64 LE) thành 40-byte value — lưu gọn trong 1 key.
fn pack_counts(c: &IndexCounts) -> [u8; 40] {
    let mut b = [0u8; 40];
    b[0..8].copy_from_slice(&c.symbols.to_le_bytes());
    b[8..16].copy_from_slice(&c.chains.to_le_bytes());
    b[16..24].copy_from_slice(&c.edges.to_le_bytes());
    b[24..32].copy_from_slice(&c.files.to_le_bytes());
    b[32..40].copy_from_slice(&c.next_id.to_le_bytes());
    b
}

fn unpack_counts(b: &[u8]) -> IndexCounts {
    let at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8-byte value"));
    IndexCounts {
        symbols: at(0),
        chains: at(8),
        edges: at(16),
        files: at(24),
        next_id: at(32),
    }
}

// ── key chuỗi dài ──
//
// LMDB giới hạn key ≈ 511 byte (MDB_BAD_VALSIZE nếu vượt). Hai DBI dùng key là
// chuỗi dài (call_names, files) gặp tên/path > giới hạn. Khi đó ta ánh xạ chuỗi
// về key có độ dài cố định (marker 8B + FNV-1a 128-bit × 2 salt ~ổn định, va
// chạm ~2^-128) và lưu chuỗi gốc trong value để phục hồi lại đúng khi scan.

const MAX_STR_KEY: usize = 440;

fn fnv1a(h: u64, s: &str) -> u64 {
    let mut h = h;
    for &b in s.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Key ổn định cho chuỗi: chuỗi ngắn dùng nguyên byte; dài → marker + hash.
fn str_key(s: &str) -> Vec<u8> {
    if s.len() <= MAX_STR_KEY {
        return s.as_bytes().to_vec();
    }
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(&u64::MAX.to_le_bytes());
    v.extend_from_slice(&fnv1a(0xcbf29ce484222325, s).to_le_bytes());
    v.extend_from_slice(&fnv1a(0x84222325cbf29ce4, s).to_le_bytes());
    v
}

/// Value = `[u32 name_len] ++ name ++ payload` — name giữ nguyên phần key bị hash.
fn call_payload(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + name.len() + payload.len());
    v.extend_from_slice(&(name.len() as u32).to_le_bytes());
    v.extend_from_slice(name.as_bytes());
    v.extend_from_slice(payload);
    v
}

/// Tách value `call_payload` → `(name, payload)`.
fn de_call_payload(v: &[u8]) -> (String, &[u8]) {
    let n = u32::from_le_bytes(v[..4].try_into().expect("call payload len")) as usize;
    let name = String::from_utf8_lossy(&v[4..4 + n]).into_owned();
    (name, &v[4 + n..])
}

/// Giá trị node = `prefix ++ record(8 LE)`.
fn node_val(prefix: &[u8], record: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(prefix.len() + 8);
    v.extend_from_slice(prefix);
    v.extend_from_slice(&(record as u64).to_le_bytes());
    v
}

fn de_node_val(v: &[u8]) -> (Vec<u8>, usize) {
    let (p, r) = v.split_at(v.len() - 8);
    (p.to_vec(), de_u64(r) as usize)
}

/// Danh sách node id → bytes (mỗi phần tử `u64` LE).
fn list_val(list: &[usize]) -> Vec<u8> {
    let mut v = Vec::with_capacity(list.len() * 8);
    for &x in list {
        v.extend_from_slice(&(x as u64).to_le_bytes());
    }
    v
}

fn de_list(v: &[u8]) -> Vec<usize> {
    v.chunks_exact(8)
        .map(|c| u64::from_le_bytes(c.try_into().unwrap()) as usize)
        .collect()
}

/// Thêm `x` vào danh sách (bỏ `EMPTY`, dedup, giữ sort) — mirror sqlite
/// `ORDER BY` + `ON CONFLICT DO NOTHING`.
fn push_unique(list: &mut Vec<usize>, x: usize) {
    if x != EMPTY && !list.contains(&x) {
        list.push(x);
        list.sort_unstable();
    }
}

// ── Tên DBI (schema — khớp bảng sqlite) ──

const D_NODES: &str = "rt_nodes";
const D_CHILDREN: &str = "rt_children";
const D_ROOTS: &str = "rt_roots";
const D_META: &str = "rt_meta";
const D_KEYLEN: &str = "rt_keylen";
const D_SHORTCUTS: &str = "rt_shortcuts";
const D_CHAINS: &str = "rt_chains";
const D_EDGES: &str = "rt_edge";
const D_NODE_META: &str = "rt_node_meta";
#[cfg(feature = "bloom-search")]
const D_BLOOMS: &str = "rt_node_blooms";
const D_COUNTER: &str = "rt_counter";
const D_SYMBOLS: &str = "sg_symbols";
const D_NEXT_ID: &str = "sg_next_id";
const D_CALL_RECORDS: &str = "sg_call_records";
const D_CALL_NAMES: &str = "sg_call_names";
const D_FILES: &str = "sg_files";
const D_VERSION: &str = "sg_meta";
const D_EMBEDDINGS: &str = "sg_embeddings";
const D_STATS: &str = "sg_stats";

/// Key duy nhất cho các "row đơn" (counter / next_id / version) — mỗi DBI chỉ có 1 row.
const KEY_ONE: [u8; 8] = [0u8; 8];

// ── Env ──

fn open_env(path: &str) -> Result<Arc<Environment>> {
    let p = Path::new(path);
    std::fs::create_dir_all(p).map_err(e)?;
    let mut b = Environment::new();
    b.set_max_dbs(32); // schema dùng ~17 named-db
    b.set_max_readers(512); // locktable đủ chỗ cho runtime/mcp probe + request song song
    b.set_map_size(1 << 30); // 1 GiB address space (LMDB chỉ commit trang thực đụng)
    let env = b.open(p).map_err(e)?;
    Ok(Arc::new(env))
}

#[cfg(feature = "lmdb")]
#[cfg_attr(feature = "sqlite", allow(dead_code))] // probe chỉ dùng khi lmdb là backend file
fn open_env_read_only(path: &str) -> lmdb::Result<Environment> {
    let mut b = Environment::new();
    b.set_flags(EnvironmentFlags::READ_ONLY);
    b.set_max_dbs(32);
    // Phải set map_size khớp với env read-write (1 GiB) — mở read-only không set
    // map_size có thể trả EACCES/PERMISSION_DENIED trên một số platform.
    b.set_map_size(1 << 30);
    b.open(Path::new(path))
}

/// Cache read-only `Environment` theo path — 1 env dùng chung cho mọi `probe_version`.
///
/// `Environment` là `Send + Sync` nên an toàn để dùng chung; env sống trọn
/// process (không drop) để locktable không bị mở/đóng lặp.
#[cfg(feature = "lmdb")]
#[cfg_attr(feature = "sqlite", allow(dead_code))] // probe chỉ dùng khi lmdb là backend file
fn probe_env(path: &str) -> lmdb::Result<Arc<Environment>> {
    let data = Path::new(path).join("data.mdb");
    if !data.is_file() {
        return Err(lmdb::Error::NotFound);
    }
    static CACHE: std::sync::LazyLock<Mutex<HashMap<String, Arc<Environment>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    let mut cache = CACHE.lock().expect("probe env cache lock");
    if let Some(env) = cache.get(path) {
        return Ok(env.clone());
    }
    let env = Arc::new(open_env_read_only(path)?);
    cache.insert(path.to_string(), env.clone());
    Ok(env)
}

/// Đọc `version` từ file mà KHÔNG tạo file (nếu chưa có) — dùng bởi
/// `SharedGraphIndex::ensure_fresh` để dò stale. Mirror `SqliteStorage::probe_version`.
///
/// Reuse env cache (`probe_env`) để không mở/đóng `Environment` mỗi lần gọi —
/// `MDB_BAD_RSLOT` xảy ra khi nhiều `Environment` cùng mở/đóng trên một locktable
/// (lock.mdb) khi nhiều request probe song song (runtime/mcp: mỗi request gọi qua
/// `ensure_fresh` → `current_version`). Cache theo path giữ 1 env read-only dùng
/// chung (sống trọn process) nên không còn tranh chấp slot reader.
#[cfg(feature = "lmdb")]
#[cfg_attr(feature = "sqlite", allow(dead_code))] // probe chỉ dùng khi lmdb là backend file
pub async fn probe_version(path: &str) -> Result<u64> {
    let env = probe_env(path)
        .map_err(|err| StorageError::Internal(format!("lmdb file not found: {path} ({err})")))?;
    let db = env.open_db(Some(D_VERSION)).map_err(e)?;
    let tx = env.begin_ro_txn().map_err(e)?;
    match tx.get(db, &KEY_ONE).map(de_u64) {
        Ok(v) => Ok(v),
        Err(lmdb::Error::NotFound) => {
            Err(StorageError::Internal("lmdb version row missing".into()))
        }
        Err(err) => Err(StorageError::Internal(err.to_string())),
    }
}

// ==================== LmdbStorage ====================

/// LMDB backend: `Arc<Environment>` + handle (Copy) của từng DBI.
pub struct LmdbStorage {
    env: Arc<Environment>,
    nodes: Database,
    children: Database,
    roots: Database,
    meta: Database,
    keylen: Database,
    shortcuts: Database,
    chains: Database,
    edges: Database,
    node_meta: Database,
    #[cfg(feature = "bloom-search")]
    blooms: Database,
    counter: Database,
    symbols: Database,
    next_id: Database,
    call_records: Database,
    call_names: Database,
    files: Database,
    version: Database,
    embeddings: Database,
    stats: Database,
}

impl LmdbStorage {
    /// Mở (hoặc tạo mới nếu chưa có) LMDB tại thư mục `path`. Idempotent —
    /// sentinel/counter chỉ seed nếu chưa có nên reopen giữ nguyên dữ liệu.
    pub async fn open(path: &str) -> Result<Self> {
        let env = open_env(path)?;
        let s = Self::from_env(env)?;
        s.init().await?;
        Ok(s)
    }

    fn from_env(env: Arc<Environment>) -> Result<Self> {
        let nodes = env
            .create_db(Some(D_NODES), DatabaseFlags::empty())
            .map_err(e)?;
        let children = env
            .create_db(Some(D_CHILDREN), DatabaseFlags::empty())
            .map_err(e)?;
        let roots = env
            .create_db(Some(D_ROOTS), DatabaseFlags::empty())
            .map_err(e)?;
        let meta = env
            .create_db(Some(D_META), DatabaseFlags::empty())
            .map_err(e)?;
        let keylen = env
            .create_db(Some(D_KEYLEN), DatabaseFlags::empty())
            .map_err(e)?;
        let shortcuts = env
            .create_db(Some(D_SHORTCUTS), DatabaseFlags::empty())
            .map_err(e)?;
        let chains = env
            .create_db(Some(D_CHAINS), DatabaseFlags::empty())
            .map_err(e)?;
        let edges = env
            .create_db(Some(D_EDGES), DatabaseFlags::empty())
            .map_err(e)?;
        let node_meta = env
            .create_db(Some(D_NODE_META), DatabaseFlags::empty())
            .map_err(e)?;
        #[cfg(feature = "bloom-search")]
        let blooms = env
            .create_db(Some(D_BLOOMS), DatabaseFlags::empty())
            .map_err(e)?;
        let counter = env
            .create_db(Some(D_COUNTER), DatabaseFlags::empty())
            .map_err(e)?;
        let symbols = env
            .create_db(Some(D_SYMBOLS), DatabaseFlags::empty())
            .map_err(e)?;
        let next_id = env
            .create_db(Some(D_NEXT_ID), DatabaseFlags::empty())
            .map_err(e)?;
        let call_records = env
            .create_db(Some(D_CALL_RECORDS), DatabaseFlags::empty())
            .map_err(e)?;
        let call_names = env
            .create_db(Some(D_CALL_NAMES), DatabaseFlags::empty())
            .map_err(e)?;
        let files = env
            .create_db(Some(D_FILES), DatabaseFlags::empty())
            .map_err(e)?;
        let version = env
            .create_db(Some(D_VERSION), DatabaseFlags::empty())
            .map_err(e)?;
        let embeddings = env
            .create_db(Some(D_EMBEDDINGS), DatabaseFlags::empty())
            .map_err(e)?;
        let stats = env
            .create_db(Some(D_STATS), DatabaseFlags::empty())
            .map_err(e)?;
        Ok(Self {
            env,
            nodes,
            children,
            roots,
            meta,
            keylen,
            shortcuts,
            chains,
            edges,
            node_meta,
            #[cfg(feature = "bloom-search")]
            blooms,
            counter,
            symbols,
            next_id,
            call_records,
            call_names,
            files,
            version,
            embeddings,
            stats,
        })
    }

    /// Seed sentinel node 0 + counter/next_id/version nếu chưa tồn tại.
    async fn init(&self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        let db = self.nodes;
        if matches!(tx.get(db, &k8(EMPTY)), Err(lmdb::Error::NotFound)) {
            tx.put(db, &k8(EMPTY), &node_val(b"", 0), WriteFlags::empty())
                .map_err(e)?;
        }
        if matches!(tx.get(self.counter, &KEY_ONE), Err(lmdb::Error::NotFound)) {
            tx.put(self.counter, &KEY_ONE, &ku64(1), WriteFlags::empty())
                .map_err(e)?;
        }
        if matches!(tx.get(self.next_id, &KEY_ONE), Err(lmdb::Error::NotFound)) {
            // next_id bắt đầu từ SYMBOL_BASE (marker reserved 1..=99) — mirror sqlite.
            tx.put(self.next_id, &KEY_ONE, &ku64(100), WriteFlags::empty())
                .map_err(e)?;
        }
        if matches!(tx.get(self.version, &KEY_ONE), Err(lmdb::Error::NotFound)) {
            tx.put(self.version, &KEY_ONE, &ku64(0), WriteFlags::empty())
                .map_err(e)?;
        }
        if matches!(tx.get(self.stats, &KEY_ONE), Err(lmdb::Error::NotFound)) {
            tx.put(self.stats, &KEY_ONE, &[0u8; 40], WriteFlags::empty())
                .map_err(e)?;
        }
        tx.commit().map_err(e)?;
        Ok(())
    }

    fn get_opt<'txn, K: AsRef<[u8]>>(
        &self,
        tx: &'txn impl Transaction,
        db: Database,
        key: &K,
    ) -> Result<Option<&'txn [u8]>> {
        match tx.get(db, key) {
            Ok(v) => Ok(Some(v)),
            Err(lmdb::Error::NotFound) => Ok(None),
            Err(err) => Err(StorageError::Internal(err.to_string())),
        }
    }
}

// ==================== Storage impl ====================

#[async_trait]
impl Storage for LmdbStorage {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        // Không có RETURNING — đọc-rồi-ghi counter trong cùng write tx; an toàn
        // vì GraphIndex tuần tự hoá mọi writer qua RwLock.
        let next = match self.get_opt(&tx, self.counter, &KEY_ONE)? {
            Some(v) => de_u64(v),
            None => 1,
        };
        let id = next as usize;
        tx.put(self.counter, &KEY_ONE, &ku64(next + 1), WriteFlags::empty())
            .map_err(e)?;
        tx.put(
            self.nodes,
            &k8(id),
            &node_val(&prefix, record),
            WriteFlags::empty(),
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        let key = k8(id);
        let Some(cur) = self.get_opt(&tx, self.nodes, &key)?.map(de_node_val) else {
            return Err(StorageError::BranchOutOfRange(id));
        };
        let (mut p, mut r) = cur;
        if let Some(np) = prefix {
            p = np;
        }
        if let Some(nr) = record {
            r = nr;
        }
        tx.put(self.nodes, &key, &node_val(&p, r), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_node(&self, id: usize) -> Result<(Vec<u8>, usize)> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let Some(v) = self.get_opt(&tx, self.nodes, &k8(id))? else {
            return Err(StorageError::BranchOutOfRange(id));
        };
        Ok(de_node_val(v))
    }

    async fn get_children(&self, id: usize) -> Result<Vec<usize>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut out = match self.get_opt(&tx, self.children, &k8(id))? {
            Some(v) => de_list(v),
            None => Vec::new(),
        };
        out.sort_unstable();
        Ok(out)
    }

    #[cfg(feature = "bloom-search")]
    async fn set_node_bloom(&mut self, id: usize, bloom: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.blooms, &k8(id), &bloom, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    #[cfg(feature = "bloom-search")]
    async fn get_node_bloom(&self, id: usize) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self.get_opt(&tx, self.blooms, &k8(id))?.map(|b| b.to_vec()))
    }

    async fn set_edge_data(&mut self, edge: usize, data: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.edges, &k8(edge), &data, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_edge_data(&self, edge: usize) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.edges, &k8(edge))?
            .map(|v| v.to_vec()))
    }

    async fn clear_edges(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.clear_db(self.edges).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn for_each_edge_data(
        &self,
        f: &mut (dyn for<'a> FnMut(usize, &'a [u8]) -> Result<()> + Send),
    ) -> Result<()> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.edges).map_err(e)?;
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for item in cur.iter() {
            let (k, v) = item.map_err(e)?;
            rows.push((k.to_vec(), v.to_vec()));
        }
        drop(cur);
        drop(tx);
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in rows {
            f(de_u64(&k) as usize, &v)?;
        }
        Ok(())
    }

    async fn set_node_meta(&mut self, elem: usize, meta: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.node_meta, &k8(elem), &meta, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_node_meta(&self, elem: usize) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.node_meta, &k8(elem))?
            .map(|v| v.to_vec()))
    }

    async fn clear_node_meta(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.clear_db(self.node_meta).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn set_chain(&mut self, record: usize, chain: &[u64]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(
            self.chains,
            &k8(record),
            &encode_chain(chain),
            WriteFlags::empty(),
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_chain(&self, record: usize) -> Result<Option<Vec<u64>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.chains, &k8(record))?
            .map(decode_chain))
    }

    async fn clear_chains(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.clear_db(self.chains).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn save_symbol(&mut self, sym: &Symbol) -> Result<()> {
        let data =
            serde_json::to_vec(sym).map_err(|err| StorageError::Internal(err.to_string()))?;
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.symbols, &ku64(sym.id), &data, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn load_symbol(&self, id: u64) -> Result<Option<Symbol>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let Some(data) = self.get_opt(&tx, self.symbols, &ku64(id))? else {
            return Ok(None);
        };
        serde_json::from_slice(data)
            .map(Some)
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    async fn load_all_symbols(&self) -> Result<Vec<Symbol>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.symbols).map_err(e)?;
        let mut out = Vec::new();
        for item in cur.iter() {
            let (_k, v) = item.map_err(e)?;
            out.push(
                serde_json::from_slice(v).map_err(|err| StorageError::Internal(err.to_string()))?,
            );
        }
        Ok(out)
    }

    async fn save_embedding(&mut self, symbol_id: u64, vector: &[f32]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(
            self.embeddings,
            &ku64(symbol_id),
            &encode_vector(vector),
            WriteFlags::empty(),
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn load_embedding(&self, symbol_id: u64) -> Result<Option<Vec<f32>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.embeddings, &ku64(symbol_id))?
            .and_then(decode_vector))
    }

    async fn load_all_embeddings(&self) -> Result<HashMap<u64, Vec<f32>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.embeddings).map_err(e)?;
        let mut out = HashMap::new();
        for item in cur.iter() {
            let (k, v) = item.map_err(e)?;
            if k.len() == 8 {
                let id = de_u64(k);
                if let Some(vec) = decode_vector(v) {
                    out.insert(id, vec);
                }
            }
        }
        Ok(out)
    }

    async fn clear_embeddings(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.clear_db(self.embeddings).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn save_next_id(&mut self, next: u64) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.next_id, &KEY_ONE, &ku64(next), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn load_next_id(&self) -> Result<u64> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.next_id, &KEY_ONE)?
            .map(de_u64)
            .unwrap_or(100))
    }

    async fn all_chains(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.chains).map_err(e)?;
        let mut out = Vec::new();
        for item in cur.iter() {
            let (k, v) = item.map_err(e)?;
            out.push((de_u64(k), v.to_vec()));
        }
        Ok(out)
    }

    async fn set_call_records(&mut self, func: u64, records: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(
            self.call_records,
            &ku64(func),
            &records,
            WriteFlags::empty(),
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_call_records(&self, func: u64) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.call_records, &ku64(func))?
            .map(|v| v.to_vec()))
    }

    async fn all_call_records(&self) -> Result<Vec<(u64, Vec<u8>)>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.call_records).map_err(e)?;
        let mut out = Vec::new();
        for item in cur.iter() {
            let (k, v) = item.map_err(e)?;
            out.push((de_u64(k), v.to_vec()));
        }
        Ok(out)
    }

    async fn set_call_name_index(&mut self, name: &str, sites: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(
            self.call_names,
            &str_key(name),
            &call_payload(name, sites),
            WriteFlags::empty(),
        )
        .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn load_call_name_index(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.call_names, &str_key(name))?
            .map(|v| de_call_payload(v).1.to_vec()))
    }

    async fn all_call_name_indexes(&self) -> Result<Vec<(String, Vec<u8>)>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.call_names).map_err(e)?;
        let mut out = Vec::new();
        for item in cur.iter() {
            let (_k, v) = item.map_err(e)?;
            let (name, sites) = de_call_payload(v);
            out.push((name, sites.to_vec()));
        }
        Ok(out)
    }

    async fn upsert_file(&mut self, f: &FileInfo) -> Result<()> {
        let data = serde_json::to_vec(f).map_err(|err| StorageError::Internal(err.to_string()))?;
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.files, &str_key(&f.path), &data, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn load_all_files(&self) -> Result<Vec<FileInfo>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut cur = tx.open_ro_cursor(self.files).map_err(e)?;
        let mut out = Vec::new();
        for item in cur.iter() {
            let (_k, v) = item.map_err(e)?;
            out.push(
                serde_json::from_slice(v).map_err(|err| StorageError::Internal(err.to_string()))?,
            );
        }
        Ok(out)
    }

    async fn version(&self) -> Result<u64> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.version, &KEY_ONE)?
            .map(de_u64)
            .unwrap_or(0))
    }

    async fn set_version(&mut self, v: u64) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.version, &KEY_ONE, &ku64(v), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn set_stats(&mut self, s: IndexCounts) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.stats, &KEY_ONE, &pack_counts(&s), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn stats(&self) -> Result<IndexCounts> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        match tx.get(self.stats, &KEY_ONE) {
            Ok(b) => Ok(unpack_counts(b)),
            Err(lmdb::Error::NotFound) => Ok(IndexCounts::default()),
            Err(err) => Err(StorageError::Internal(err.to_string())),
        }
    }

    async fn clear_entities(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        for db in [
            self.symbols,
            self.call_records,
            self.call_names,
            self.files,
            self.embeddings,
        ] {
            tx.clear_db(db).map_err(e)?;
        }
        tx.put(self.next_id, &KEY_ONE, &ku64(100), WriteFlags::empty())
            .map_err(e)?;
        tx.put(self.version, &KEY_ONE, &ku64(0), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn set_root(&mut self, shard: usize, root: usize) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.roots, &k8(shard), &k8(root), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_root(&self, shard: usize) -> Result<usize> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.roots, &k8(shard))?
            .map(de_u64)
            .unwrap_or(EMPTY as u64) as usize)
    }

    async fn set_meta(&mut self, record: usize, meta: &[u8]) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.meta, &k8(record), &meta, WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_meta(&self, record: usize) -> Result<Option<Vec<u8>>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.meta, &k8(record))?
            .map(|v| v.to_vec()))
    }

    async fn set_key_len(&mut self, record: usize, len: usize) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.put(self.keylen, &k8(record), &k8(len), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_key_len(&self, record: usize) -> Result<Option<usize>> {
        let tx = self.env.begin_ro_txn().map_err(e)?;
        Ok(self
            .get_opt(&tx, self.keylen, &k8(record))?
            .map(de_u64)
            .map(|v| v as usize))
    }

    async fn add_shortcut_node(&mut self, shard: usize, elem: &[u8], node_id: usize) -> Result<()> {
        let mut key = k8(shard).to_vec();
        key.extend_from_slice(elem);
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        let mut list = match self.get_opt(&tx, self.shortcuts, &key)? {
            Some(v) => de_list(v),
            None => Vec::new(),
        };
        push_unique(&mut list, node_id);
        tx.put(self.shortcuts, &key, &list_val(&list), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    async fn get_shortcut_nodes(&self, shard: usize, elem: &[u8]) -> Result<Vec<usize>> {
        let mut key = k8(shard).to_vec();
        key.extend_from_slice(elem);
        let tx = self.env.begin_ro_txn().map_err(e)?;
        let mut out = match self.get_opt(&tx, self.shortcuts, &key)? {
            Some(v) => de_list(v),
            None => Vec::new(),
        };
        out.sort_unstable();
        Ok(out)
    }

    async fn clear_shortcuts(&mut self) -> Result<()> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        tx.clear_db(self.shortcuts).map_err(e)?;
        tx.commit().map_err(e)?;
        Ok(())
    }

    fn new_tx(&self) -> Box<dyn Tx> {
        Box::new(LmdbTx {
            env: self.env.clone(),
            nodes: self.nodes,
            children: self.children,
            counter: self.counter,
            nodes_pending: Vec::new(),
            ops: Vec::new(),
        })
    }
}

// ==================== LmdbTx ====================

/// Transaction cho `LmdbStorage`: buffer mutation, áp dụng atomic trong một
/// `RwTransaction` tại `commit`. `new_node` cấp id ngay (bump counter như
/// sqlite `RETURNING`) nhưng row chỉ lộ khi commit.
pub struct LmdbTx {
    env: Arc<Environment>,
    nodes: Database,
    children: Database,
    counter: Database,
    nodes_pending: Vec<(usize, Vec<u8>, usize)>,
    ops: Vec<TxOp>,
}

#[async_trait]
impl Tx for LmdbTx {
    async fn new_node(&mut self, prefix: Vec<u8>, record: usize) -> Result<usize> {
        let mut tx = self.env.begin_rw_txn().map_err(e)?;
        let next = match tx.get(self.counter, &KEY_ONE).map(de_u64) {
            Ok(v) => v,
            Err(lmdb::Error::NotFound) => 1,
            Err(err) => return Err(StorageError::Internal(err.to_string())),
        };
        tx.put(self.counter, &KEY_ONE, &ku64(next + 1), WriteFlags::empty())
            .map_err(e)?;
        tx.commit().map_err(e)?;
        let id = next as usize;
        self.nodes_pending.push((id, prefix, record));
        Ok(id)
    }

    async fn update_node(
        &mut self,
        id: usize,
        prefix: Option<Vec<u8>>,
        record: Option<usize>,
    ) -> Result<()> {
        self.ops.push(TxOp::UpdateNode { id, prefix, record });
        Ok(())
    }

    async fn add_child(&mut self, parent: usize, child: usize) -> Result<()> {
        self.ops.push(TxOp::AddChild { parent, child });
        Ok(())
    }

    async fn move_child(&mut self, from: usize, to: usize, child: usize) -> Result<()> {
        self.ops.push(TxOp::MoveChild { from, to, child });
        Ok(())
    }

    async fn commit(self: Box<Self>) -> Result<()> {
        let LmdbTx {
            env,
            nodes,
            children,
            counter,
            nodes_pending,
            ops,
        } = *self;
        let mut tx = env.begin_rw_txn().map_err(e)?;

        // 1. Materialize node mới — để ops add/move trỏ tới hợp lệ.
        for (id, prefix, record) in &nodes_pending {
            tx.put(
                nodes,
                &k8(*id),
                &node_val(prefix, *record),
                WriteFlags::empty(),
            )
            .map_err(e)?;
        }

        // 2. Counter đã được bump ở new_node; giữ MAX như sqlite phòng writer khác.
        if let Some(max_id) = nodes_pending.iter().map(|(id, _, _)| *id).max() {
            let cur = match tx.get(counter, &KEY_ONE).map(de_u64) {
                Ok(v) => v,
                Err(lmdb::Error::NotFound) => 1,
                Err(err) => return Err(StorageError::Internal(err.to_string())),
            };
            let nxt = cur.max(max_id as u64 + 1);
            tx.put(counter, &KEY_ONE, &ku64(nxt), WriteFlags::empty())
                .map_err(e)?;
        }

        // 3. Áp dụng ops — children là read-modify-write trên KV; gộp theo parent
        //    để tránh đọc/ghi lặp nhiều lần cho cùng một node.
        let mut child_map: HashMap<usize, Vec<usize>> = HashMap::new();
        for op in &ops {
            match op {
                TxOp::AddChild { parent, child } => {
                    let list = child_map.entry(*parent).or_insert_with(|| {
                        tx.get(children, &k8(*parent))
                            .map(de_list)
                            .unwrap_or_default()
                    });
                    push_unique(list, *child);
                }
                TxOp::MoveChild { from, to, child } => {
                    if from != to {
                        if let Some(list) = child_map.get_mut(from) {
                            list.retain(|x| x != child);
                        } else {
                            let list = tx
                                .get(children, &k8(*from))
                                .map(de_list)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|x| x != child)
                                .collect::<Vec<_>>();
                            child_map.insert(*from, list);
                        }
                        let list = child_map.entry(*to).or_insert_with(|| {
                            tx.get(children, &k8(*to)).map(de_list).unwrap_or_default()
                        });
                        push_unique(list, *child);
                    }
                }
                TxOp::UpdateNode { id, prefix, record } => {
                    let key = k8(*id);
                    let Some((mut p, mut r)) = tx.get(nodes, &key).map(de_node_val).ok() else {
                        continue;
                    };
                    if let Some(np) = prefix {
                        p = np.clone();
                    }
                    if let Some(nr) = record {
                        r = *nr;
                    }
                    tx.put(nodes, &key, &node_val(&p, r), WriteFlags::empty())
                        .map_err(e)?;
                }
            }
        }
        for (parent, list) in &child_map {
            tx.put(children, &k8(*parent), &list_val(list), WriteFlags::empty())
                .map_err(e)?;
        }

        tx.commit().map_err(e)?;
        Ok(())
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lmdb");
        let path = path.to_string_lossy().into_owned();
        (dir, path)
    }

    #[tokio::test]
    async fn test_new_node_and_get_node() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();
        let id = s.new_node(b"hello".to_vec(), 42).await.unwrap();
        assert_ne!(id, EMPTY);
        let (prefix, record) = s.get_node(id).await.unwrap();
        assert_eq!(prefix, b"hello");
        assert_eq!(record, 42);
    }

    #[tokio::test]
    async fn test_embeddings_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();
        let v1 = vec![0.1f32, 0.2, 0.3, -0.4];
        let v2 = vec![1.0f32, -1.0, 0.0, 0.5];
        s.save_embedding(100, &v1).await.unwrap();
        s.save_embedding(101, &v2).await.unwrap();
        // upsert overwrite cho 100.
        s.save_embedding(100, &v2).await.unwrap();

        let all = s.load_all_embeddings().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get(&100).unwrap(), &v2);
        assert_eq!(all.get(&101).unwrap(), &v2);
        assert_eq!(s.load_embedding(100).await.unwrap().unwrap(), v2);
        assert_eq!(s.load_embedding(101).await.unwrap().unwrap(), v2);

        s.clear_embeddings().await.unwrap();
        assert!(s.load_all_embeddings().await.unwrap().is_empty());
        assert_eq!(s.load_embedding(101).await.unwrap(), None);
    }

    /// Node trong tx chưa lộ ra reader cho tới `commit`.
    #[tokio::test]
    async fn test_tx_atomic() {
        let (_d, path) = tmp_path();
        let s = LmdbStorage::open(&path).await.unwrap();
        let mut tx = s.new_tx();
        let id = tx.new_node(b"x".to_vec(), 7).await.unwrap();
        // Chưa commit → đọc qua storage thấy "chưa có".
        assert!(matches!(
            s.get_node(id).await,
            Err(StorageError::BranchOutOfRange(_))
        ));

        tx.add_child(EMPTY, id).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(s.get_node(id).await.unwrap(), (b"x".to_vec(), 7));
        assert_eq!(s.get_children(EMPTY).await.unwrap(), vec![id]);
    }

    /// Move child từ parent này sang parent khác.
    #[tokio::test]
    async fn test_move_child() {
        let (_d, path) = tmp_path();
        let s = LmdbStorage::open(&path).await.unwrap();
        let mut sa = s.new_tx();
        let a = sa.new_node(b"a".to_vec(), 1).await.unwrap();
        let b = sa.new_node(b"b".to_vec(), 2).await.unwrap();
        sa.add_child(EMPTY, a).await.unwrap();
        sa.add_child(EMPTY, b).await.unwrap();
        sa.commit().await.unwrap();

        let mut tx = s.new_tx();
        tx.move_child(EMPTY, a, b).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(s.get_children(EMPTY).await.unwrap(), vec![a]);
        assert_eq!(s.get_children(a).await.unwrap(), vec![b]);
    }

    /// Shortcut set đọc/ghi đúng, node id unique sort.
    #[tokio::test]
    async fn test_shortcut() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();
        let elem = b"ab".to_vec();
        s.add_shortcut_node(0, &elem, 5).await.unwrap();
        s.add_shortcut_node(0, &elem, 3).await.unwrap();
        s.add_shortcut_node(0, &elem, 5).await.unwrap(); // dup — bị loại
        assert_eq!(s.get_shortcut_nodes(0, &elem).await.unwrap(), vec![3, 5]);
    }

    /// Meta/keylen ghi đọc như sqlite.
    #[tokio::test]
    async fn test_meta_and_keylen() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();
        s.set_meta(1, b"m").await.unwrap();
        assert_eq!(s.get_meta(1).await.unwrap(), Some(b"m".to_vec()));
        assert_eq!(s.get_meta(2).await.unwrap(), None);
        s.set_key_len(1, 9).await.unwrap();
        assert_eq!(s.get_key_len(1).await.unwrap(), Some(9));
    }

    /// Dữ liệu tồn tại sau reopen + probe_version đọc đúng, không tạo file mới.
    #[tokio::test]
    async fn test_reopen_persists_and_probe() {
        let (_d, path) = tmp_path();
        {
            let mut s = LmdbStorage::open(&path).await.unwrap();
            s.new_node(b"hi".to_vec(), 1).await.unwrap();
            s.set_version(7).await.unwrap();
        }
        let s = LmdbStorage::open(&path).await.unwrap();
        assert_eq!(s.version().await.unwrap(), 7);

        let (prefix, record) = s.get_node(1).await.unwrap();
        assert_eq!(prefix, b"hi");
        assert_eq!(record, 1);

        // probe_version đọc từ file hiện có (không tạo file mới).
        assert_eq!(probe_version(&path).await.unwrap(), 7);
        assert!(probe_version("definitely/missing.lmdb").await.is_err());
    }

    /// Regression MDB_BAD_RSLOT: nhiều reader probe song song trên cùng path
    /// phải dùng chung env cache — không mở/đóng Environment mỗi lần gọi.
    #[tokio::test]
    async fn test_concurrent_probe_reuses_env() {
        let (_d, path) = tmp_path();
        let (_d2, path2) = tmp_path();
        {
            let mut s = LmdbStorage::open(&path).await.unwrap();
            s.set_version(5).await.unwrap();
        }
        {
            let mut s = LmdbStorage::open(&path2).await.unwrap();
            s.set_version(9).await.unwrap();
        }

        // Nhiều task probe song song trên 2 path khác nhau — mỗi path trả đúng
        // version, và KHÔNG mở env mới mỗi lần (cache dùng chung → không BAD_RSLOT).
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let p1 = path.clone();
            let p2 = path2.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..20 {
                    let v1 = probe_version(&p1).await.unwrap();
                    let v2 = probe_version(&p2).await.unwrap();
                    assert_eq!(v1, 5);
                    assert_eq!(v2, 9);
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }

    /// Regression MDB_BAD_VALSIZE: key chuỗi > 511 byte bị LMDB từ chối; `str_key`
    /// phải hash về key cố định 24B và giữ chuỗi gốc trong value để đọc lại đúng.
    #[tokio::test]
    async fn test_long_call_name_and_path_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();

        // call-name > 511 byte.
        let long_call = format!("very{}long{}mangled", "t".repeat(300), "q".repeat(300));
        assert!(long_call.len() > 511);
        s.set_call_name_index(&long_call, b"sites").await.unwrap();
        // Đọc lại nguyên payload dù key đã bị hash.
        assert_eq!(
            s.load_call_name_index(&long_call).await.unwrap().as_deref(),
            Some(b"sites".as_slice())
        );
        // Scan trả đúng tên gốc.
        let all = s.all_call_name_indexes().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, long_call);
        assert_eq!(all[0].1, b"sites");

        // path file > 511 byte.
        let seg = "d".repeat(300);
        let long_path = format!("src/{seg}/{seg}/mod.ts");
        assert!(long_path.len() > 511);
        let f = FileInfo {
            path: long_path.clone(),
            language: "ts".into(),
            bytes: 10,
            lines: 1,
        };
        s.upsert_file(&f).await.unwrap();
        let files = s.load_all_files().await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, long_path);
    }

    #[tokio::test]
    async fn test_stats_roundtrip() {
        let (_d, path) = tmp_path();
        let mut s = LmdbStorage::open(&path).await.unwrap();
        // Chưa ghi → trả zeros (caller fallback rebuild).
        assert_eq!(s.stats().await.unwrap(), IndexCounts::default());
        let counts = IndexCounts {
            symbols: 12,
            chains: 3,
            edges: 5,
            files: 2,
            next_id: 100,
        };
        s.set_stats(counts).await.unwrap();
        assert_eq!(s.stats().await.unwrap(), counts);
        drop(s);
        // Mở lại (stats_cached mở storage từ route — LMDB cho phép nhiều RW handle)
        // vẫn đọc được counts đã persist.
        let ro = LmdbStorage::open(&path).await.unwrap();
        assert_eq!(ro.stats().await.unwrap(), counts);
    }
}
