//! Embedding backend cho semantic search (KNN / k-means).
//!
//! Thiết kế **pluggable**: `GraphIndex` chỉ biết [`EmbeddingBackend`] (trait) —
//! backend cụ thể sinh vector từ text. Có hai backend:
//!
//! - [`FastEmbedBackend`] (feature `fastembed`): dùng crate `fastembed` chạy
//!   model ONNX **BAAI/bge-small-en-v1.5** (384-dim, đa ngôn ngữ) — sinh vector
//!   **semantic thật** (sentence-transformer). Đây là backend duy nhất sinh
//!   vector dùng được; **phải được bật tường minh** qua `[embedding].backend`.
//! - [`HashingEmbeddings`]: dependency-free, thuần Rust — baseline lexical khi
//!   không bật fastembed. **KHÔNG** được dùng làm fallback silent: nếu
//!   `[embedding].backend = "fastembed"` mà model tải thất bại (thiếu mạng,
//!   thiếu ONNX runtime...), init index sẽ **báo lỗi** chứ không lặng lẽ chuyển
//!   sang hashing.
//!
//! ## Opt-in (mặc định TẮT)
//!
//! Embedding **không bật mặc định**. Chỉ khi `[embedding].backend = "fastembed"`
//! (trong `.codegraph/config.toml`) được set vào lúc `init`/`open` thì vector
//! index mới được xây + persist. Nếu không set (hoặc set `"hashing"`), semantic
//! search không khả dụng và `GraphIndex` không chạy embedding gì cả.
//!
//! ```toml
//! [embedding]
//! backend = "fastembed"          # "fastembed" (bật) | "hashing"/unset (tắt)
//! model = "bge-small-en-v1.5"    # alias thân thiện hoặc variant name
//! cache_dir = "~/.cache/codegraph/embeddings"  # thư mục global chứa model
//! ```
//!
//! `cache_dir` là **thư mục global** — model được tải/đệm vào đây một lần, chia
//! sẻ cho mọi project. Dùng `codegraph embed --model <x>` để pre-download trước.
//! Xem [`EmbeddingConfig`] / [`set_embedding_config`] / [`warm_model_cache`].
//!
//! Các vector được **persist vào storage** (qua `Storage::save_embedding`) để
//! KNN/k-means tái dùng qua các lần restart mà không phải re-embed.
//!
//! Cả hai backend đều trả vector đã **L2-normalize** (cosine similarity = dot
//! product) để `VectorIndex` hoạt động nhất quán.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Số chiều mặc định của vector embedding.
///
/// Bằng đúng dim của model fastembed mặc định (BGE-small-en-v1.5 = 384) để
/// `VectorIndex` khởi tạo cùng chiều với backend mặc định; `rebuild_vector_index`
/// vẫn lấy `dim()` từ backend thực tế nên khác biệt nhẹ không gây lỗi.
pub const VECTOR_DIM: usize = 384;

/// Backend sinh embedding: ánh xạ một đoạn text → vector f32 (đã L2-normalize
/// để cosine similarity = dot product).
pub trait EmbeddingBackend: Send + Sync {
    /// Số chiều vector.
    fn dim(&self) -> usize;
    /// Embed `text` → vector f32 đã chuẩn hóa (norm = 1).
    fn embed(&self, text: &str) -> Vec<f32>;
    /// Embed một batch text → vector. Mặc định lặp `embed` từng phần tử (chậm
    /// với model ONNX). `FastEmbedBackend` override để batch (tận dụng tính toán
    /// vector hoá hàng loạt — nhanh gấp bội so với gọi `embed` tuần tự, nhất là
    /// khi index hàng chục ngàn symbol).
    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// L2-normalize một vector (norm = 1). Trả nguyên `v` nếu là zero-vector.
/// Sau khi normalize, cosine similarity = dot product.
fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// Loại embedding backend (từ `[embedding].backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmbeddingBackendKind {
    /// fastembed (ONNX, semantic thật). Phải bật tường minh qua config.
    Fastembed,
    /// HashingEmbeddings (dependency-free, lexical overlap). Không sinh vector
    /// semantic; `[embedding]` unset hoặc `backend = "hashing"` → embedding TẮT.
    #[default]
    Hashing,
}

impl std::str::FromStr for EmbeddingBackendKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fastembed" | "fast" | "onnx" | "embedding" => Ok(Self::Fastembed),
            "hashing" | "hash" | "lexical" => Ok(Self::Hashing),
            other => Err(format!("unknown embedding backend: {other}")),
        }
    }
}

/// Cấu hình embedding backend — đọc từ `[embedding]` trong `.codegraph/config.toml`.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    /// Có bật embedding (vector index) không. **OPT-IN**: chỉ `true` khi
    /// `[embedding]` được khai báo tường minh trong config (thậm chí chỉ cần
    /// có key `backend`). Mặc định `false` → không chạy embedding, không xây
    /// vector index, semantic search báo lỗi rõ ràng.
    pub enabled: bool,
    /// Loại backend (`fastembed` | `hashing`) — chỉ có nghĩa khi `enabled`.
    /// - `hashing`: dependency-free, lexical (không tải model).
    /// - `fastembed`: ONNX sentence-transformer; **lỗi init nếu tải model thất
    ///   bại** (KHÔNG fallback silent sang hashing).
    pub backend: EmbeddingBackendKind,
    /// Tên model fastembed (alias thân thiện hoặc variant name, VD
    /// `"bge-small-en-v1.5"` / `"BGESmallENV15"`). Mặc định BGE-small-en-v1.5.
    pub model: String,
    /// Thư mục cache model (global). `None` → `~/.cache/codegraph/embeddings`.
    pub cache_dir: Option<PathBuf>,
    /// Thư mục chứa extension sqlite-vss (`vector0`/`vss0`). **Chỉ cho backend
    /// SQLite**: khi được set (và file tồn tại), KNN semantic chạy qua `vss0`
    /// (HNSW ANN trong chính SQLite) thay vì brute-force in-memory. Thiếu file →
    /// fallback brute-force (KHÔNG lỗi). `None` → tự dò `<cache_dir>/vss`.
    pub vss_extension: Option<PathBuf>,
    /// Execution provider cho ONNX Runtime (fastembed) — chỉ có nghĩa khi backend
    /// = `fastembed` VÀ crate compile với feature `apple-accel` (macOS). Giá trị:
    /// `None`/"cpu" (mặc định) → ONNX Runtime chạy trên CPU (Accelerate/vecLib
    /// SIMD + mọi core); `"coreml"` → Core ML EP (ANE/GPU, Apple Silicon);
    /// `"metal"` → Metal EP (GPU). Yêu cầu build `--features fastembed,apple-accel`
    /// trên macOS; nếu set `"coreml"`/`"metal"` mà build thiếu `apple-accel` →
    /// bỏ qua (chạy CPU), KHÔNG lỗi. Platform khác macOS → luôn CPU.
    pub execution_provider: Option<String>,
}

impl Default for EmbeddingConfig {
    /// Mặc định: embedding **TẮT** (`enabled = false`). Phải khai báo `[embedding]`
    /// trong config mới kích hoạt. `model`/`cache_dir` vẫn giữ sẵn để khi bật
    /// lên không phải set thêm.
    fn default() -> Self {
        Self {
            enabled: false,
            backend: EmbeddingBackendKind::Hashing,
            model: "bge-small-en-v1.5".to_string(),
            cache_dir: default_cache_dir(),
            vss_extension: None,
            execution_provider: None,
        }
    }
}

impl EmbeddingConfig {
    /// Parse từ raw config strings (từ `[embedding]` trong `config.toml`).
    ///
    /// `enabled = true` **chỉ khi** `backend` được khai báo tường minh (dù là
    /// `"hashing"` hay `"fastembed"`) — đảm bảo opt-in: bỏ qua `[embedding]`
    /// hoàn toàn = tắt. `backend` parse sai → coi như `"hashing"` (vẫn enabled,
    /// nhưng dùng backend rẻ, không tải model).
    pub fn from_raw(
        backend: Option<&str>,
        model: Option<&str>,
        cache_dir: Option<&str>,
        vss_extension: Option<&str>,
        execution_provider: Option<&str>,
    ) -> Self {
        let enabled = backend.is_some();
        let backend = backend
            .and_then(|s| s.parse::<EmbeddingBackendKind>().ok())
            .unwrap_or_default();
        let model = model.unwrap_or("bge-small-en-v1.5").to_string();
        let cache_dir = cache_dir.and_then(expand_tilde);
        let vss_extension = vss_extension.and_then(expand_tilde);
        let execution_provider = execution_provider.map(|s| s.trim().to_ascii_lowercase());
        Self {
            enabled,
            backend,
            model,
            cache_dir,
            vss_extension,
            execution_provider,
        }
    }
}

/// Cache dir mặc định: `~/.cache/codegraph/embeddings` (global, cross-project).
fn default_cache_dir() -> Option<PathBuf> {
    expand_tilde("~/.cache/codegraph/embeddings")
}

/// Expand `~` thành home dir (best-effort, cross-platform). Trả `Some` nếu
/// không bắt đầu bằng `~`. Dùng `dirs::home_dir()` để lấy home đúng trên mọi OS
/// (Windows: `USERPROFILE`/`HOMEDRIVE`, macOS/Linux: `$HOME`).
fn expand_tilde(path: &str) -> Option<PathBuf> {
    if !path.starts_with('~') {
        return Some(PathBuf::from(path));
    }
    let home = dirs::home_dir()?;
    let rest = path.strip_prefix('~').unwrap_or("");
    Some(home.join(rest.trim_start_matches('/')))
}

/// Suffix file extension của sqlite-vss theo OS (`.dylib` / `.so` / `.dll`).
fn vss_lib_suffix() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    }
}

/// Giải đường dẫn tới 2 extension sqlite-vss (`vector0`, `vss0`).
///
/// Trả `Some((vector0, vss0))` nếu cả hai file tồn tại, `None` nếu chưa cấu
/// hình hoặc thiếu file. Ưu tiên `vss_extension` trong config; nếu `None` → tự
/// dò `<cache_dir>/vss`. Chỉ trả `Some` khi file thực sự tồn tại → caller có
/// thể yên tâm thêm extension vào kết nối SQLite mà không làm hỏng `open`
/// khi thiếu binary.
pub fn resolve_vss_extensions() -> Option<(PathBuf, PathBuf)> {
    let cfg = embedding_config();
    let dir = cfg
        .vss_extension
        .clone()
        .or_else(|| cfg.cache_dir.as_ref().map(|c| c.join("vss")))?;
    let ext = vss_lib_suffix();
    let v0 = dir.join(format!("vector0.{ext}"));
    let vss = dir.join(format!("vss0.{ext}"));
    (v0.exists() && vss.exists()).then_some((v0, vss))
}

/// Global embedding config — set 1 lần lúc startup (từ project config) qua
/// [`set_embedding_config`]; các `GraphIndex` đọc qua [`embedding_config`].
///
/// Quan trọng: [`embedding_config`] KHÔNG tự khởi tạo OnceLock này (chỉ đọc,
/// fallback về [`DEFAULT_EMBEDDING_CONFIG`]) — để tránh race trong test: nếu
/// `embedding_config()` tự `get_or_init(default)` thì config mặc định (tắt) sẽ
/// bị "khoá" trước khi test gọi `set_embedding_config`, làm opt-in bị ignore.
static EMBEDDING_CONFIG: OnceLock<EmbeddingConfig> = OnceLock::new();

/// Config mặc định (embedding TẮT) — lazily init, KHÔNG ảnh hưởng `EMBEDDING_CONFIG`.
static DEFAULT_EMBEDDING_CONFIG: OnceLock<EmbeddingConfig> = OnceLock::new();

/// Áp dụng config embedding (chỉ có tác dụng lần đầu; các lần sau bị bỏ qua).
/// Gọi ở nơi mở index (VD `ExtractConfig::storage_route`).
pub fn set_embedding_config(cfg: EmbeddingConfig) {
    EMBEDDING_CONFIG.get_or_init(|| cfg);
}

/// Đọc config embedding hiện tại (mặc định TẮT nếu chưa set bởi [`set_embedding_config`]).
pub fn embedding_config() -> &'static EmbeddingConfig {
    match EMBEDDING_CONFIG.get() {
        Some(c) => c,
        None => DEFAULT_EMBEDDING_CONFIG.get_or_init(EmbeddingConfig::default),
    }
}

/// Embedding có được kích hoạt không.
///
/// Chỉ `true` khi `[embedding]` được khai báo tường minh trong config (tức
/// `EmbeddingConfig.enabled`). Khi `false`: `GraphIndex` không chạy embedding,
/// không xây vector index, và semantic search báo lỗi rõ thay vì fallback silent.
pub fn embedding_enabled() -> bool {
    embedding_config().enabled
}

/// Backend dependency-free (fallback): feature-hashing bag-of-words + character
/// n-gram vào vector chiều `dim`, rồi L2-normalize. Deterministic, rất nhanh.
///
/// Hai symbol chia sẻ nhiều token/substring → vector gần nhau (cosine cao) →
/// KNN trả về gần nhau. "Lexical similarity" chứ không phải semantic sâu, nhưng
/// phục vụ tốt việc "search tên tương tự" và không cần tải model.
pub struct HashingEmbeddings {
    dim: usize,
}

impl HashingEmbeddings {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(1) }
    }

    /// Hash một token → bin [0, dim).
    fn bin(&self, token: &str) -> usize {
        let mut h = DefaultHasher::new();
        token.hash(&mut h);
        (h.finish() as usize) % self.dim
    }
}

impl EmbeddingBackend for HashingEmbeddings {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dim];
        // Token (alphanumeric) + character trigram → capture cả word và
        // substring overlap.
        for raw in text.split(|c: char| !c.is_alphanumeric()) {
            if raw.is_empty() {
                continue;
            }
            let tok = raw.to_lowercase();
            v[self.bin(&tok)] += 1.0;
            let chars: Vec<char> = tok.chars().collect();
            for w in chars.windows(3) {
                let trigram: String = w.iter().collect();
                v[self.bin(&trigram)] += 0.5;
            }
        }
        normalize(v)
    }
}

/// Backend fastembed (model ONNX) — chỉ compile khi bật feature `fastembed`.
///
/// `fastembed::TextEmbedding::embed` yêu cầu `&mut self`, nên giữ model trong
/// `Mutex` (interior mutability) và share một instance process-wide qua
/// `OnceLock` để không tải model (~130MB, cache `cache_dir`) nhiều lần.
#[cfg(feature = "fastembed")]
mod fastembed_backend {
    use super::*;
    use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// Process-wide ONNX model, share bởi mọi `GraphIndex`. Lần đầu gọi sẽ tải
    /// và cache model; các lần sau reuse instance đã tải. Nếu init lỗi (thiếu
    /// mạng / ONNX runtime), lỗi được cache trong `OnceLock` và mọi lần gọi sau
    /// đều trả lại lỗi đó (KHÔNG fallback silent sang [`HashingEmbeddings`]).
    pub(crate) fn global_model(
        model: EmbeddingModel,
        cache_dir: Option<PathBuf>,
    ) -> Result<&'static Arc<Mutex<TextEmbedding>>, String> {
        static MODEL_CELL: OnceLock<Result<Arc<Mutex<TextEmbedding>>, String>> = OnceLock::new();
        MODEL_CELL
            .get_or_init(|| {
                // Dùng mọi core vật lý — ONNX Runtime sẽ chạy inference SIMD
                // (trên macOS là Accelerate/vecLib) đa luồng. Đặt tường minh để
                // không bị default lệch trên máy ít core ảo.
                let intra = std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4);
                let mut opt = TextInitOptions::new(model)
                    .with_show_download_progress(true)
                    .with_intra_threads(intra);
                if let Some(dir) = cache_dir {
                    opt = opt.with_cache_dir(dir);
                }
                // Execution provider (CoreML/Metal) — CHỈ compile khi feature
                // `apple-accel` bật (macOS, fastembed). Các type CoreML/Metal EP
                // chỉ tồn tại khi `ort` compile với feature tương ứng. Nếu config
                // yêu cầu "coreml"/"metal" mà build thiếu `apple-accel` → block này
                // bị loại bỏ → chạy CPU (KHÔNG lỗi silent, chỉ không dùng GPU).
                #[cfg(all(feature = "fastembed", feature = "apple-accel"))]
                {
                    use ort::execution_providers::CoreML;
                    // `ort` 2.0.0-rc.13 chỉ expose CoreML EP (Metal EP chưa có
                    // feature). CoreML trên Apple Silicon đã chạy trên GPU/ANE,
                    // nên cả "coreml" và "metal" đều dùng CoreML EP. Nếu config
                    // ghi "metal" → in note rõ thay vì lặng lẹ.
                    let ep = match embedding_config().execution_provider.as_deref() {
                        Some("coreml") => Some(CoreML::default().build()),
                        Some("metal") => {
                            eprintln!(
                                "[codegraph-graph] Metal EP is not exposed by ONNX Runtime 2.0.0-rc.13; using CoreML (Apple GPU/ANE) instead"
                            );
                            Some(CoreML::default().build())
                        }
                        _ => None,
                    };
                    if let Some(ep) = ep {
                        opt = opt.with_execution_providers(vec![ep]);
                    }
                }
                TextEmbedding::try_new(opt)
                    .map(|m| Arc::new(Mutex::new(m)))
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    /// Map tên model thân thiện (hoặc variant name) → `EmbeddingModel`.
    pub(crate) fn resolve_model(s: &str) -> EmbeddingModel {
        let t = s.trim();
        let by_alias = match t.to_ascii_lowercase().as_str() {
            "bge-small-en-v1.5" | "bge-small" => Some(EmbeddingModel::BGESmallENV15),
            "bge-base-en-v1.5" | "bge-base" => Some(EmbeddingModel::BGEBaseENV15),
            "bge-large-en-v1.5" | "bge-large" => Some(EmbeddingModel::BGELargeENV15),
            "all-minilm-l6-v2" | "all-minilm" => Some(EmbeddingModel::AllMiniLML6V2),
            "all-mpnet-base-v2" => Some(EmbeddingModel::AllMpnetBaseV2),
            "nomic-embed-text-v1.5" | "nomic-embed-text" => Some(EmbeddingModel::NomicEmbedTextV15),
            "multilingual-e5-small" => Some(EmbeddingModel::MultilingualE5Small),
            _ => None,
        };
        if let Some(m) = by_alias {
            return m;
        }
        // Thử variant name thô (VD "BGESmallENV15").
        if let Ok(m) = t.parse::<EmbeddingModel>() {
            return m;
        }
        eprintln!(
            "[codegraph-graph] unknown embedding model '{s}', falling back to BGE-small-en-v1.5"
        );
        EmbeddingModel::BGESmallENV15
    }

    pub struct FastEmbedBackend {
        model: Arc<Mutex<TextEmbedding>>,
        dim: usize,
    }

    impl FastEmbedBackend {
        /// Khởi tạo backend từ [`EmbeddingConfig`], tái sử dụng model đã tải
        /// (nếu có). Trả `Err` nếu fastembed không init được (thiếu mạng tải
        /// model, thiếu ONNX runtime…).
        pub fn try_new(cfg: &EmbeddingConfig) -> Result<Self, String> {
            let model = resolve_model(&cfg.model);
            let cache = cfg.cache_dir.clone().or_else(default_cache_dir);
            let model = global_model(model, cache)?;
            // Xác định dim thực tế bằng một embedding probe (robust với mọi model).
            let dim = {
                let mut g = model.lock();
                g.embed(vec!["__probe__"], None)
                    .map(|v| v[0].len())
                    .map_err(|e| e.to_string())?
            };
            Ok(Self {
                model: model.clone(),
                dim,
            })
        }
    }

    impl EmbeddingBackend for FastEmbedBackend {
        fn dim(&self) -> usize {
            self.dim
        }

        fn embed(&self, text: &str) -> Vec<f32> {
            let mut g = self.model.lock();
            let v = g
                .embed(vec![text], None)
                .expect("fastembed embed failed (model unloaded?)");
            normalize(v.into_iter().next().unwrap())
        }

        /// Batch embedding — fastembed tính toán vector hoá hàng loạt (SIMD/đa
        /// luồng qua ONNX Runtime), nhanh hơn rất nhiều so với gọi `embed` tuần
        /// tự từng symbol. Truyền toàn bộ chunk làm 1 batch ONNX (`batch_size =
        /// texts.len()`) để tận dụng tối đa throughput.
        fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
            let mut g = self.model.lock();
            let v = g
                .embed(texts, Some(texts.len().max(1)))
                .expect("fastembed embed_batch failed (model unloaded?)");
            v.into_iter().map(normalize).collect()
        }
    }
}

/// Pre-download (warm) một model fastembed vào `cache_dir` (global) — để semantic
/// search chạy offline sau này. Dùng bởi CLI `codegraph embed --model <x>`.
#[cfg(feature = "fastembed")]
pub fn warm_model_cache(model: &str, cache_dir: Option<&std::path::Path>) -> Result<(), String> {
    let m = fastembed_backend::resolve_model(model);
    let cache = cache_dir.map(PathBuf::from).or_else(default_cache_dir);
    fastembed_backend::global_model(m, cache)?;
    eprintln!("[codegraph-graph] embedding model '{model}' cached");
    Ok(())
}

/// Tạo backend từ config. Trả về `Err` (không fallback silent) khi:
///
/// - `backend = "fastembed"` mà feature `fastembed` chưa bật compile-time, hoặc
/// - `backend = "fastembed"` mà model tải thất bại (thiếu mạng / ONNX runtime).
///
/// Caller phải handle error (mở index sẽ báo lỗi rõ ràng nếu model không tải được).
pub fn make_backend() -> Result<Box<dyn EmbeddingBackend>, String> {
    let cfg = embedding_config();
    match cfg.backend {
        EmbeddingBackendKind::Hashing => Ok(Box::new(HashingEmbeddings::new(VECTOR_DIM))),
        EmbeddingBackendKind::Fastembed => {
            #[cfg(feature = "fastembed")]
            {
                fastembed_backend::FastEmbedBackend::try_new(cfg)
                    .map(|b| Box::new(b) as Box<dyn EmbeddingBackend>)
            }
            #[cfg(not(feature = "fastembed"))]
            {
                Err(
                    "embedding backend 'fastembed' requested but crate not compiled with 'fastembed' feature".to_string()
                )
            }
        }
    }
}

/// Backend mặc định cho `GraphIndex` — CHỈ dùng khi config tắt (`backend = "hashing"`
/// hoặc không set `[embedding]`). Nếu `[embedding].backend = "fastembed"` thì
/// caller PHẢI dùng `make_backend()` để handle error explicit.
pub fn default_backend() -> Box<dyn EmbeddingBackend> {
    Box::new(HashingEmbeddings::new(VECTOR_DIM))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dim_and_normalized() {
        let b = HashingEmbeddings::new(64);
        let v = b.embed("authenticateUser");
        assert_eq!(v.len(), 64);
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "vector phải L2-normalize");
    }

    #[test]
    fn similar_text_higher_cosine() {
        let b = HashingEmbeddings::new(256);
        let a = b.embed("user authentication service");
        let b2 = b.embed("authentication User service");
        let c = b.embed("render frame buffer");
        let dot = |x: &[f32], y: &[f32]| x.iter().zip(y).map(|(p, q)| p * q).sum::<f32>();
        let sim_ab = dot(&a, &b2);
        let sim_ac = dot(&a, &c);
        assert!(
            sim_ab > sim_ac,
            "hai text gần nhau phải có cosine > text khác biệt ({sim_ab} vs {sim_ac})"
        );
    }
}
