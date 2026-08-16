//! Vector index cho semantic search: lưu embedding mỗi symbol (id → vector f32
//! đã normalize) và hỗ trợ KNN (cosine) + k-means clustering.
//!
//! MVP: **brute-force** — tính cosine với mọi vector (O(n)). Đủ cho index tới
//! vài chục ngàn symbol (latency < vài ms trên CPU). Khi cần scale → thay
//! bằng HNSW/IVF sau (cùng interface `knn`). Vector index là **derived state**
//! (hàm pure của symbols) nên rebuild từ entity store mỗi lần `ingest`/`open`,
//! không persist riêng.

use std::collections::HashMap;

/// Kết quả KNN: (symbol id, cosine similarity ∈ [-1, 1]).
pub type KnnHit = (u64, f32);

/// Kết quả k-means: centroids + assignment (symbol id → cluster index).
#[derive(Debug, Clone)]
pub struct KMeansResult {
    pub centroids: Vec<Vec<f32>>,
    pub assignments: HashMap<u64, usize>,
}

/// Index vector in-memory.
pub struct VectorIndex {
    dim: usize,
    vectors: HashMap<u64, Vec<f32>>,
}

impl VectorIndex {
    pub fn new(dim: usize) -> Self {
        Self {
            dim: dim.max(1),
            vectors: HashMap::new(),
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.vectors.len()
    }

    /// Thay thế toàn bộ index.
    pub fn set_all(&mut self, vectors: HashMap<u64, Vec<f32>>) {
        self.vectors = vectors;
    }

    /// Thêm / cập nhật embedding của một symbol.
    pub fn insert(&mut self, id: u64, vec: Vec<f32>) {
        self.vectors.insert(id, vec);
    }

    /// Xoá embedding của một symbol.
    pub fn delete(&mut self, id: u64) {
        self.vectors.remove(&id);
    }

    /// Xoá toàn bộ.
    pub fn clear(&mut self) {
        self.vectors.clear();
    }

    /// Lấy vector của một symbol (nếu có).
    pub fn get(&self, id: u64) -> Option<&Vec<f32>> {
        self.vectors.get(&id)
    }

    /// Cosine similarity của hai vector (giả định đã normalize → = dot).
    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        debug_assert_eq!(a.len(), b.len());
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    /// KNN: top-`k` symbol gần nhất với `query_vec` (cosine giảm dần).
    /// `k = 0` → trả toàn bộ (sort theo similarity). Rỗng nếu index trống.
    pub fn knn(&self, query_vec: &[f32], k: usize) -> Vec<KnnHit> {
        if self.vectors.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<KnnHit> = self
            .vectors
            .iter()
            .map(|(&id, v)| (id, Self::cosine(query_vec, v)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if k > 0 && scored.len() > k {
            scored.truncate(k);
        }
        scored
    }

    /// K-means (Lloyd) clustering trên các vector đã lưu. Khởi tạo bằng
    /// k-means++ (deterministic: seed cố định) để ổn định. Trả về centroids +
    /// assignment. `k` clamp về `[1, n]`; `max_iters` giới hạn vòng lặp.
    ///
    /// Dùng cho việc gom nhóm symbol liên quan (VD "tất cả hàm xử lý auth").
    pub fn kmeans(&self, k: usize, max_iters: usize) -> KMeansResult {
        let points: Vec<(u64, Vec<f32>)> = self
            .vectors
            .iter()
            .map(|(&id, v)| (id, v.clone()))
            .collect();
        let n = points.len();
        if n == 0 {
            return KMeansResult {
                centroids: Vec::new(),
                assignments: HashMap::new(),
            };
        }
        let k = k.clamp(1, n);

        // ── k-means++ init (deterministic xorshift seed) ──
        let mut rng = XorShift::new(0x9E37_79B9_7F4A_7C15 ^ (n as u64));
        let mut centroids: Vec<Vec<f32>> = Vec::with_capacity(k);
        centroids.push(points[rng.next() as usize % n].1.clone());
        while centroids.len() < k {
            // Chọn điểm có D² (khoảng cách tới centroid gần nhất) lớn nhất,
            // dùng rng để bốc (xấp xỉ k-means++ mà không sort toàn bộ mỗi bước).
            let mut best = 0usize;
            let mut best_d = -1.0f32;
            for (i, (_, v)) in points.iter().enumerate() {
                let d = centroids
                    .iter()
                    .map(|c| Self::cosine(c, v))
                    .fold(f32::MAX, |acc, s| acc.min(1.0 - s));
                if d > best_d {
                    best_d = d;
                    best = i;
                }
            }
            centroids.push(points[best].1.clone());
        }

        // ── Lloyd iterations ──
        let mut assignments: HashMap<u64, usize> = HashMap::with_capacity(n);
        for _ in 0..max_iters.max(1) {
            let mut changed = false;
            // Assign.
            for (id, v) in &points {
                let mut best = 0usize;
                let mut best_s = f32::NEG_INFINITY;
                for (ci, c) in centroids.iter().enumerate() {
                    let s = Self::cosine(c, v);
                    if s > best_s {
                        best_s = s;
                        best = ci;
                    }
                }
                if assignments.get(id) != Some(&best) {
                    assignments.insert(*id, best);
                    changed = true;
                }
            }
            // Update centroids (mean rồi normalize).
            let mut sums: Vec<Vec<f32>> = vec![vec![0.0f32; self.dim]; k];
            let mut counts = vec![0usize; k];
            for (id, v) in &points {
                let c = assignments[id];
                counts[c] += 1;
                for (j, x) in v.iter().enumerate() {
                    sums[c][j] += x;
                }
            }
            for (ci, sum) in sums.iter_mut().enumerate() {
                if counts[ci] > 0 {
                    for x in sum.iter_mut() {
                        *x /= counts[ci] as f32;
                    }
                }
                normalize(sum);
                centroids[ci] = std::mem::take(sum);
            }
            if !changed {
                break;
            }
        }

        KMeansResult {
            centroids,
            assignments,
        }
    }
}

/// L2-normalize một vector tại chỗ.
fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// PRNG nhỏ, deterministic (không phụ thuộc `rand`).
struct XorShift {
    state: u64,
}

impl XorShift {
    fn new(seed: u64) -> Self {
        Self { state: seed | 1 }
    }
    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(xs: &[f32]) -> Vec<f32> {
        xs.to_vec()
    }

    #[test]
    fn knn_basic() {
        let mut idx = VectorIndex::new(3);
        // Vector đã L2-normalized (đúng contract của cosine = dot product).
        idx.insert(1, v(&[1.0, 0.0, 0.0]));
        idx.insert(2, v(&[0.0, 1.0, 0.0]));
        idx.insert(3, v(&[0.995, 0.0995, 0.0]));
        let hits = idx.knn(&[1.0, 0.0, 0.0], 2);
        assert_eq!(hits.len(), 2);
        // id 1 (chính nó) và id 3 (gần nhất) đứng đầu.
        assert_eq!(hits[0].0, 1);
        assert_eq!(hits[1].0, 3);
        assert!(hits[0].1 > hits[1].1);
    }

    #[test]
    fn knn_empty() {
        let idx = VectorIndex::new(4);
        assert!(idx.knn(&[0.0; 4], 5).is_empty());
    }

    #[test]
    fn kmeans_groups_similar() {
        let mut idx = VectorIndex::new(2);
        // Cluster A: quanh (1,0).
        idx.insert(1, v(&[1.0, 0.0]));
        idx.insert(2, v(&[0.9, 0.1]));
        // Cluster B: quanh (0,1).
        idx.insert(3, v(&[0.0, 1.0]));
        idx.insert(4, v(&[0.1, 0.9]));
        let res = idx.kmeans(2, 20);
        assert_eq!(res.centroids.len(), 2);
        // Hai symbol trong cluster A phải cùng nhãn.
        assert_eq!(res.assignments[&1], res.assignments[&2]);
        assert_eq!(res.assignments[&3], res.assignments[&4]);
        // Hai cluster khác nhãn.
        assert_ne!(res.assignments[&1], res.assignments[&3]);
    }
}
