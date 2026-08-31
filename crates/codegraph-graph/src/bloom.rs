//! Bloom filter — cho phép kiểm tra "phần tử có tồn tại trong tập hợp không?"
//!
//! - **0 false negative**: nếu `contains` trả về `false` → chắc chắn không tồn tại
//! - **False positive**: có thể nói "có" khi thực tế không — tunable qua `m` và `k`

// ==================== BloomFilter ====================

/// Bloom filter với `m` bits, `k` hash functions (Kirsch-Mitzenmacker optimization).
///
/// ## Parameters
///
/// | `m` (bits) | `k` (hashes) | Target items | False positive |
/// |---|---|---|---|
/// | 1024 | 7 | ~50 | ~1% |
/// | 2048 | 7 | ~100 | ~1% |
/// | 4096 | 10 | ~300 | ~0.1% |
/// | 8192 | 14 | ~800 | ~0.01% |
#[derive(Clone)]
pub struct BloomFilter {
    /// Bit array (m bits).
    bits: Vec<u64>,
    /// Number of hash functions.
    k: u64,
    /// Total bits (m = bits.len() * 64).
    #[allow(dead_code)]
    m: u64,
    /// Mask for fast modulo (m must be power of 2).
    m_mask: u64,
}

impl BloomFilter {
    /// Tạo bloom filter với `m` bits, `k` hash functions.
    ///
    /// `m` được làm tròn lên thành power of 2 (để modulo nhanh).
    pub fn new(m: usize, k: usize) -> Self {
        let m = m.next_power_of_two().max(64); // tối thiểu 64 bits
        let m_u64 = m / 64;
        Self {
            bits: vec![0u64; m_u64],
            k: k as u64,
            m: m as u64,
            m_mask: (m - 1) as u64,
        }
    }

    /// Insert `data` vào bloom filter (set k bits tương ứng).
    pub fn insert(&mut self, data: &[u8]) {
        let (h1, h2) = Self::hash128(data);
        let m_mask = self.m_mask;

        for i in 0..self.k {
            let bit_pos = (h1.wrapping_add(i.wrapping_mul(h2))) & m_mask;
            self.set_bit(bit_pos as usize);
        }
    }

    /// Kiểm tra `data` có khả năng tồn tại?
    ///
    /// - `true` → **có thể** tồn tại (hoặc false positive)
    /// - `false` → **chắc chắn** không tồn tại
    pub fn contains(&self, data: &[u8]) -> bool {
        let (h1, h2) = Self::hash128(data);
        let m_mask = self.m_mask;

        for i in 0..self.k {
            let bit_pos = (h1.wrapping_add(i.wrapping_mul(h2))) & m_mask;
            if !self.get_bit(bit_pos as usize) {
                return false;
            }
        }

        true
    }

    /// Merge bloom filter khác vào (bitwise OR).
    /// Dùng khi split node để kết hợp bloom của node cha + leg.
    #[allow(dead_code)] // API giữ nguyên — dùng khi kết hợp bloom của các node khi rebuild.
    pub fn union(&mut self, other: &BloomFilter) {
        assert_eq!(self.bits.len(), other.bits.len(), "bloom size mismatch");
        for (a, b) in self.bits.iter_mut().zip(other.bits.iter()) {
            *a |= *b;
        }
    }

    /// Reset toàn bộ bits về 0.
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.bits.fill(0);
    }

    // ── Public / crate-visible helpers ──

    /// Hash `data` thành 2 u64 độc lập (sip hash với seed 0 và 1).
    #[inline]
    pub(crate) fn hash128(data: &[u8]) -> (u64, u64) {
        // Hằng số nhân của FxHash (64-bit)
        const FX_PRIME: u64 = 0x517cc1b727220a95;

        // --- Tính Hash thứ nhất (h1) với Seed mặc định ---
        let mut h1 = 0;
        for &byte in data {
            h1 = (h1 ^ byte as u64).wrapping_mul(FX_PRIME);
        }

        // --- Tính Hash thứ hai (h2) với Seed khác biệt để đảm bảo độc lập ---
        // Khởi tạo bằng một hằng số ngẫu nhiên lớn (Kẻ phá vỡ tính đối xứng)
        let mut h2 = 0xa5a5a5a5a5a5a5a5;
        for &byte in data {
            h2 = (h2 ^ byte as u64).wrapping_mul(FX_PRIME);
        }

        // Thực hiện thêm một bước xáo trộn bit cuối để triệt tiêu tương quan tuyến tính
        let h1_final = h1 ^ (h1 >> 32);
        let h2_final = h2 ^ (h2 >> 32);

        (h1_final, h2_final)
    }

    /// Kiểm tra `data` có khả năng tồn tại? (dùng hash đã tính sẵn)
    ///
    /// - `true` → **có thể** tồn tại (hoặc false positive)
    /// - `false` → **chắc chắn** không tồn tại
    ///
    /// ## Khi nào dùng
    ///
    /// Khi cần check cùng 1 data trên nhiều bloom filters (vd: search_like).
    /// Hash chỉ tính 1 lần, dùng `contains_raw` cho mỗi bloom filter.
    #[allow(dead_code)] // API giữ nguyên — dùng cho search_like batch.
    #[inline]
    pub fn contains_raw(&self, h1: u64, h2: u64) -> bool {
        let m_mask = self.m_mask;
        for i in 0..self.k {
            let bit_pos = (h1.wrapping_add(i.wrapping_mul(h2))) & m_mask;
            if !self.get_bit(bit_pos as usize) {
                return false;
            }
        }
        true
    }

    /// Serialize bloom filter thành Vec<u8> để lưu xuống storage.
    ///
    /// Format:
    /// - 8 bytes: bits.len() (u64 LE)
    /// - 8 bytes: k (u64 LE)
    /// - 8 bytes: m (u64 LE)
    /// - 8 bytes: m_mask (u64 LE)
    /// - bits.len() * 8 bytes: raw bits array
    #[inline]
    pub fn serialize(&self) -> Vec<u8> {
        let len = self.bits.len();
        let mut buf = Vec::with_capacity(32 + len * 8);
        buf.extend_from_slice(&(len as u64).to_le_bytes());
        buf.extend_from_slice(&self.k.to_le_bytes());
        buf.extend_from_slice(&self.m.to_le_bytes());
        buf.extend_from_slice(&self.m_mask.to_le_bytes());
        for &w in &self.bits {
            buf.extend_from_slice(&w.to_le_bytes());
        }
        buf
    }

    /// Deserialize bloom filter từ bytes (format tương ứng serialize).
    #[inline]
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        if data.len() < 32 {
            return None;
        }
        let (header, rest) = data.split_at(32);
        let bits_len = u64::from_le_bytes(header[0..8].try_into().ok()?) as usize;
        let k = u64::from_le_bytes(header[8..16].try_into().ok()?);
        let m = u64::from_le_bytes(header[16..24].try_into().ok()?);
        let m_mask = u64::from_le_bytes(header[24..32].try_into().ok()?);

        if rest.len() < bits_len * 8 {
            return None;
        }
        let mut bits = vec![0u64; bits_len];
        for (i, w) in bits.iter_mut().enumerate() {
            let start = i * 8;
            *w = u64::from_le_bytes(rest[start..start + 8].try_into().ok()?);
        }

        Some(Self { bits, k, m, m_mask })
    }

    /// Set bit tại `pos` (0-indexed).
    #[inline]
    fn set_bit(&mut self, pos: usize) {
        let idx = pos / 64;
        let bit = pos % 64;
        self.bits[idx] |= 1u64 << bit;
    }

    /// Get bit tại `pos` (0-indexed).
    #[inline]
    fn get_bit(&self, pos: usize) -> bool {
        let idx = pos / 64;
        let bit = pos % 64;
        (self.bits[idx] >> bit) & 1 == 1
    }

    /// Số bits đang được set (population count).
    #[allow(dead_code)] // API giữ nguyên — đo mật độ bloom.
    #[inline]
    pub fn popcount(&self) -> u64 {
        let (chunks, remainder) = self.bits.as_chunks::<4>();

        let mut total = 0u64;
        for chunk in chunks {
            total += (chunk[0].count_ones()
                + chunk[1].count_ones()
                + chunk[2].count_ones()
                + chunk[3].count_ones()) as u64;
        }

        for &word in remainder {
            total += word.count_ones() as u64;
        }

        total
    }

    /// False positive rate ước lượng (dựa trên số bits đã set).
    #[allow(dead_code)] // API giữ nguyên — đo chất lượng bloom.
    #[inline]
    pub fn estimated_fpr(&self) -> f64 {
        let ones = self.popcount();
        let total = self.m;
        let p = ones as f64 / total as f64;
        p.powf(self.k as f64)
    }
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_basic() {
        let mut bf = BloomFilter::new(1024, 7);
        assert!(!bf.contains(b"hello"));
        bf.insert(b"hello");
        assert!(bf.contains(b"hello"));
    }

    #[test]
    fn test_bloom_no_false_negative() {
        let mut bf = BloomFilter::new(4096, 10);
        let items: Vec<&[u8]> = vec![
            "Vàng".as_bytes(),
            "Tiệm".as_bytes(),
            b"PNJ",
            b"SJC",
            "Bảo Tín".as_bytes(),
            b"hello",
            b"world",
            b"rust",
            b"bloom",
            b"filter",
            b"algorithm",
            b"radix",
            b"tree",
            b"search",
            b"index",
        ];
        for item in &items {
            bf.insert(item);
        }
        // Mọi item đã insert phải contains == true
        for item in &items {
            assert!(
                bf.contains(item),
                "false negative: {:?}",
                std::str::from_utf8(item)
            );
        }
    }

    #[test]
    fn test_bloom_union() {
        let mut bf1 = BloomFilter::new(1024, 7);
        let mut bf2 = BloomFilter::new(1024, 7);
        bf1.insert(b"hello");
        bf2.insert(b"world");
        bf1.union(&bf2);
        assert!(bf1.contains(b"hello"));
        assert!(bf1.contains(b"world"));
    }

    #[test]
    fn test_bloom_clear() {
        let mut bf = BloomFilter::new(1024, 7);
        bf.insert(b"hello");
        assert!(bf.contains(b"hello"));
        bf.clear();
        assert!(!bf.contains(b"hello"));
    }

    #[test]
    fn test_bloom_popcount() {
        let mut bf = BloomFilter::new(2048, 7);
        assert_eq!(bf.popcount(), 0);
        bf.insert(b"hello");
        assert_eq!(bf.popcount(), 7); // k = 7 bits set
    }

    #[test]
    fn test_bloom_m_power_of_two() {
        // m = 1000 → next power of two = 1024
        let bf = BloomFilter::new(1000, 7);
        assert_eq!(bf.m, 1024);
        assert_eq!(bf.bits.len(), 1024 / 64);
    }

    #[test]
    fn test_bloom_min_m() {
        let bf = BloomFilter::new(1, 1);
        assert_eq!(bf.m, 64); // tối thiểu 64 bits
    }
}
