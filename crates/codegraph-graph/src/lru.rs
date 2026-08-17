use dashmap::DashMap;
use parking_lot::Mutex;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const NULL: usize = usize::MAX;

// --- CẤU TRÚC DỮ LIỆU ---

struct Node<K, V> {
    key: Option<K>,
    value: Option<V>,
    next: AtomicUsize,
    prev: AtomicUsize,
}

struct HeadTail {
    first: usize,
    last: usize,
}

/// AlignedShard giúp mỗi Mutex nằm riêng trên một Cache Line (64 bytes).
/// Điều này loại bỏ hiện tượng False Sharing, giúp tăng tốc ghi đa luồng.
#[repr(align(64))]
struct AlignedShard {
    mutex: Mutex<HeadTail>,
}

pub struct LruCache<K, V, const S: usize> {
    mapping: DashMap<K, usize>,
    caching: Box<[Node<K, V>]>,
    shards: [AlignedShard; S],
    shard_mask: usize,
    pub on_removing: Option<Arc<dyn Fn(K, V) + Send + Sync>>,
    pub on_updating: Option<Arc<dyn Fn(K, V) + Send + Sync>>,
}

impl<K, V, const S: usize> fmt::Debug for LruCache<K, V, S>
where
    K: fmt::Debug + std::hash::Hash + Eq,
    V: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LruCache")
            .field("mapping", &self.mapping)
            .field("caching_len", &self.caching.len())
            .field("shard_mask", &self.shard_mask)
            .field("on_removing", &self.on_removing.as_ref().map(|_| "Closure"))
            .field("on_updating", &self.on_updating.as_ref().map(|_| "Closure"))
            .finish()
    }
}
// --- IMPLEMENTATION ---

impl<K, V, const S: usize> LruCache<K, V, S>
where
    K: Clone + Hash + Eq + Send + Sync,
    V: Clone + Send + Sync,
{
    pub fn new(total_capacity: usize) -> Self {
        // S phải là lũy thừa của 2 để dùng bitwise AND thay cho phép chia lấy dư (%)
        assert!(
            S > 0 && S.is_power_of_two(),
            "SHARD_COUNT (S) phải là lũy thừa của 2 (ví dụ: 8, 16, 32)"
        );

        let capacity_per_shard = total_capacity.div_ceil(S);
        let actual_total = capacity_per_shard * S;

        // 1. Khởi tạo Arena bộ nhớ phẳng
        let mut caching_vec = Vec::with_capacity(actual_total);
        for shard_idx in 0..S {
            let offset = shard_idx * capacity_per_shard;
            for i in 0..capacity_per_shard {
                let current = offset + i;
                caching_vec.push(Node {
                    key: None,
                    value: None,
                    next: AtomicUsize::new(if i + 1 < capacity_per_shard {
                        current + 1
                    } else {
                        NULL
                    }),
                    prev: AtomicUsize::new(if i > 0 { current - 1 } else { NULL }),
                });
            }
        }

        // 2. Khởi tạo mảng các Shard Mutex (đã được aligned)
        let shards = std::array::from_fn(|i| {
            let offset = i * capacity_per_shard;
            AlignedShard {
                mutex: Mutex::new(HeadTail {
                    first: if capacity_per_shard > 0 { offset } else { NULL },
                    last: if capacity_per_shard > 0 {
                        offset + capacity_per_shard - 1
                    } else {
                        NULL
                    },
                }),
            }
        });

        Self {
            mapping: DashMap::with_capacity(actual_total),
            caching: caching_vec.into_boxed_slice(),
            shards,
            shard_mask: S - 1,
            on_removing: None,
            on_updating: None,
        }
    }

    #[inline]
    pub fn get_shard_idx(&self, key: &K) -> usize {
        let mut s = DefaultHasher::new();
        key.hash(&mut s);
        (s.finish() as usize) & self.shard_mask
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let index = *self.mapping.get(key)?;

        // Đọc giá trị an toàn (Node này chắc chắn tồn tại vì mapping đang giữ nó)
        let val = self.caching[index].value.as_ref()?.clone();

        // Optimistic LRU Update: Dùng try_lock để không làm chậm luồng Read
        let shard_idx = self.get_shard_idx(key);
        if let Some(mut ht) = self.shards[shard_idx].mutex.try_lock() {
            self.move_to_front_inside_lock(&mut ht, index);
        }

        Some(val)
    }

    pub fn put(&self, key: K, value: V) {
        let shard_idx = self.get_shard_idx(&key);

        // Case 1: Key đã tồn tại (Update)
        if let Some(entry) = self.mapping.get_mut(&key) {
            let index = *entry.value();
            if let Some(cb) = &self.on_updating {
                cb(key.clone(), value.clone());
            }

            unsafe {
                let node_ptr = &self.caching[index] as *const Node<K, V> as *mut Node<K, V>;
                (*node_ptr).value = Some(value);
            }
            drop(entry);

            // Cập nhật thứ tự (Có thể dùng try_lock hoặc lock tùy độ ưu tiên)
            if let Some(mut ht) = self.shards[shard_idx].mutex.try_lock() {
                self.move_to_front_inside_lock(&mut ht, index);
            }
            return;
        }

        // Case 2: Ghi mới (Bắt buộc dùng lock cứng để bảo vệ tính nhất quán)
        let mut ht = self.shards[shard_idx].mutex.lock();
        let last_idx = ht.last;
        if last_idx == NULL {
            return;
        }

        let node = &self.caching[last_idx];

        // Đuổi dữ liệu cũ nếu có
        if let Some(ref old_key) = node.key {
            self.mapping.remove(old_key);
            if let Some(cb) = &self.on_removing {
                cb(old_key.clone(), node.value.as_ref().unwrap().clone());
            }
        }

        // Ghi dữ liệu mới vào Node cuối của Shard
        unsafe {
            let node_ptr = node as *const Node<K, V> as *mut Node<K, V>;
            (*node_ptr).key = Some(key.clone());
            (*node_ptr).value = Some(value);
        }

        self.mapping.insert(key, last_idx);
        self.move_to_front_inside_lock(&mut ht, last_idx);
    }

    /// Xoá entry khỏi cache theo key
    /// Chỉ remove khỏi DashMap, slot trong arena được tái sử dụng khi `put` overwrite.
    pub fn remove(&self, key: &K) -> Option<V> {
        let (_, index) = self.mapping.remove(key)?;
        self.caching[index].value.clone()
    }

    /// Xoá toàn bộ entry (dùng khi invalidate hàng loạt, VD sau transaction
    /// commit hoặc `clear_*` của storage). Reset cả arena lẫn linked-list.
    pub fn clear(&self) {
        self.mapping.clear();
        let cap_per_shard = self.caching.len() / S;
        for shard_idx in 0..S {
            let offset = shard_idx * cap_per_shard;
            for i in 0..cap_per_shard {
                let cur = offset + i;
                unsafe {
                    let p = &self.caching[cur] as *const Node<K, V> as *mut Node<K, V>;
                    (*p).key = None;
                    (*p).value = None;
                    (*p).next.store(
                        if i + 1 < cap_per_shard { cur + 1 } else { NULL },
                        Ordering::Release,
                    );
                    (*p).prev
                        .store(if i > 0 { cur - 1 } else { NULL }, Ordering::Release);
                }
            }
            let mut ht = self.shards[shard_idx].mutex.lock();
            ht.first = if cap_per_shard > 0 { offset } else { NULL };
            ht.last = if cap_per_shard > 0 {
                offset + cap_per_shard - 1
            } else {
                NULL
            };
        }
    }

    fn move_to_front_inside_lock(&self, ht: &mut HeadTail, index: usize) {
        if ht.first == index || ht.first == NULL {
            return;
        }

        let node = &self.caching[index];
        let p = node.prev.load(Ordering::Acquire);
        let n = node.next.load(Ordering::Acquire);

        // Cắt node ra khỏi vị trí hiện tại
        if p != NULL {
            self.caching[p].next.store(n, Ordering::Release);
        }
        if n != NULL {
            self.caching[n].prev.store(p, Ordering::Release);
        }

        if index == ht.last {
            ht.last = p;
        }

        // Đưa lên đầu danh sách của Shard
        let old_first = ht.first;
        node.next.store(old_first, Ordering::Release);
        node.prev.store(NULL, Ordering::Release);

        if old_first != NULL {
            self.caching[old_first].prev.store(index, Ordering::Release);
        }

        ht.first = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    const SHARD_COUNT: usize = 32;

    #[test]
    fn test_lru_cache_sharded_logic() {
        let capacity_per_shard = 2;
        let cache = LruCache::<usize, usize, 32>::new(capacity_per_shard * SHARD_COUNT);

        // Tìm 3 key rơi vào cùng 1 shard để test logic eviction
        let mut keys = Vec::new();
        for i in 0..1000 {
            if cache.get_shard_idx(&i) == 0 {
                keys.push(i);
                if keys.len() == 3 {
                    break;
                }
            }
        }
        let (k1, k2, k3) = (keys[0], keys[1], keys[2]);

        cache.put(k1, 10);
        cache.put(k2, 20);

        assert_eq!(cache.get(&k1), Some(10)); // k1 lên head của shard
        cache.put(k3, 30); // shard full (2 slot), evict k2 (vì k1 vừa được access)

        assert_eq!(cache.get(&k2), None); // k2 bị đuổi
        assert_eq!(cache.get(&k1), Some(10));
        assert_eq!(cache.get(&k3), Some(30));
    }

    #[test]
    fn test_update_existing_key() {
        let cache = LruCache::<usize, usize, 32>::new(16 * 2); // 2 slot mỗi shard
        cache.put(1, 10);
        cache.put(1, 20);

        assert_eq!(cache.get(&1), Some(20));
        assert_eq!(cache.mapping.len(), 1);

        let index = *cache.mapping.get(&1).unwrap();
        cache.put(1, 30);
        assert_eq!(index, *cache.mapping.get(&1).unwrap(), "Index không đổi");
    }

    #[test]
    fn test_empty_cache() {
        let cache = LruCache::<usize, usize, 32>::new(0);
        cache.put(1, 10);
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn test_extreme_data_integrity() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let capacity_per_shard = 50;
        let total_capacity = capacity_per_shard * SHARD_COUNT;
        let cache = LruCache::<usize, usize, 32>::new(total_capacity);

        // Hàm tạo giá trị "chuẩn" theo Key để kiểm tra integrity
        let gen_value = |k: usize| -> usize {
            let mut s = DefaultHasher::new();
            k.hash(&mut s);
            s.finish() as usize
        };

        let num_threads = 12;
        let ops_per_thread = 2000;

        // --- PHASE 1: STRESS WRITE ---
        thread::scope(|s| {
            for t in 0..num_threads {
                let cache_ref = &cache;
                s.spawn(move || {
                    for i in 0..ops_per_thread {
                        let key = t * ops_per_thread + i;
                        let val = gen_value(key);
                        cache_ref.put(key, val);
                    }
                });
            }
        });

        // --- PHASE 2: INTEGRITY VALIDATION ---

        // 1. Kiểm tra từng cặp Key-Value trong Mapping
        for entry in cache.mapping.iter() {
            let key = *entry.key();
            let index = *entry.value();

            let node = &cache.caching[index];
            let stored_key = node.key.expect("Node trong mapping phải có key");
            let stored_val = node.value.expect("Node trong mapping phải có value");

            assert_eq!(
                key, stored_key,
                "Data Corruption: Key trong mapping ({}) khác Key trong Node ({})",
                key, stored_key
            );
            assert_eq!(
                stored_val,
                gen_value(key),
                "Data Corruption: Value của key {} bị sai lệch!",
                key
            );

            // 2. Kiểm tra Shard Consistency: Key phải nằm đúng Shard của nó
            let expected_shard = cache.get_shard_idx(&key);
            // Kiểm tra xem index này có nằm trong dải bộ nhớ của Shard đó không
            let actual_shard = index / capacity_per_shard;
            assert_eq!(
                expected_shard, actual_shard,
                "Key {} nằm sai phân vùng Shard!",
                key
            );
        }

        // 3. Kiểm tra tính toàn vẹn của cấu trúc Danh sách liên kết (Double-ended check)
        for s_idx in 0..SHARD_COUNT {
            let ht = cache.shards[s_idx].mutex.lock();
            let mut forward_count = 0;
            let mut backward_count = 0;

            // Duyệt xuôi: Head -> Tail
            let mut curr = ht.first;
            let mut last_seen = NULL;
            while curr != NULL {
                forward_count += 1;
                last_seen = curr;
                curr = cache.caching[curr].next.load(Ordering::Acquire);
            }
            assert_eq!(
                last_seen, ht.last,
                "Tail của Shard {} không khớp khi duyệt xuôi",
                s_idx
            );

            // Duyệt ngược: Tail -> Head
            let mut curr = ht.last;
            let mut first_seen = NULL;
            while curr != NULL {
                backward_count += 1;
                first_seen = curr;
                curr = cache.caching[curr].prev.load(Ordering::Acquire);
            }
            assert_eq!(
                first_seen, ht.first,
                "Head của Shard {} không khớp khi duyệt ngược",
                s_idx
            );
            assert_eq!(
                forward_count, backward_count,
                "Số lượng node duyệt xuôi và ngược không bằng nhau ở Shard {}",
                s_idx
            );
            assert_eq!(
                forward_count, capacity_per_shard,
                "Shard {} không đủ số lượng node",
                s_idx
            );
        }

        println!("🚀 [PASSED] Dữ liệu chuẩn 100%, không phát hiện Race Condition trên Node!");
    }

    #[test]
    fn test_internal_state_after_eviction_sharded() {
        // Để dễ test eviction, ta chọn capacity sao cho mỗi shard có đúng 2 slot
        let capacity_per_shard = 2;
        let total_capacity = capacity_per_shard * SHARD_COUNT;
        let cache = LruCache::<usize, usize, 32>::new(total_capacity);

        // 1. Tìm 3 key sao cho chúng rơi vào CÙNG MỘT SHARD
        // Điều này quan trọng vì mỗi shard tự quản lý việc đuổi (eviction) riêng
        let mut keys = Vec::new();

        for i in 0..1000 {
            if cache.get_shard_idx(&i) == 0 {
                keys.push(i);
                if keys.len() == 3 {
                    break;
                }
            }
        }

        let k1 = keys[0];
        let k2 = keys[1];
        let k3 = keys[2];

        // Giai đoạn lấp đầy 2 slot của Shard 0
        cache.put(k1, 10);
        cache.put(k2, 20);

        // Lấy index của k1 trước khi nó bị đuổi
        let index_of_k1 = *cache.mapping.get(&k1).expect("Key 1 phải tồn tại").value();

        // 2. Evict k1 bằng cách chèn k3 (vào cùng shard 0)
        cache.put(k3, 30);

        // Kiểm tra mapping
        assert_eq!(
            cache.mapping.get(&k3).map(|e| *e.value()),
            Some(index_of_k1),
            "Key 3 phải chiếm slot của Key 1"
        );
        assert!(cache.mapping.get(&k1).is_none(), "Key 1 phải bị đuổi");

        // 3. Lock đúng Shard 0 để kiểm tra Head/Tail
        let shard_idx = cache.get_shard_idx(&k3);
        let ht = cache.shards[shard_idx].mutex.lock();

        let mru_index = *cache.mapping.get(&k3).unwrap().value();
        let lru_index = *cache.mapping.get(&k2).unwrap().value();

        assert_eq!(ht.first, mru_index, "Key 3 phải là đầu danh sách của shard");
        assert_eq!(ht.last, lru_index, "Key 2 phải là cuối danh sách của shard");

        // 4. Kiểm tra liên kết giữa các node trong Arena
        let mru_node = &cache.caching[mru_index];
        let lru_node = &cache.caching[lru_index];

        assert_eq!(mru_node.key, Some(k3));
        assert_eq!(mru_node.next.load(Ordering::Relaxed), lru_index);
        assert_eq!(mru_node.prev.load(Ordering::Relaxed), NULL);

        assert_eq!(lru_node.key, Some(k2));
        assert_eq!(lru_node.next.load(Ordering::Relaxed), NULL);
        assert_eq!(lru_node.prev.load(Ordering::Relaxed), mru_index);
    }

    #[test]
    fn test_lru_deadlock() {
        // Khởi tạo cache với capacity 10
        let cache = Arc::new(LruCache::<usize, String, 32>::new(16));

        // Giả lập dữ liệu ban đầu
        cache.put(1, "A".to_string());
        cache.put(2, "B".to_string());

        let cache_clone1 = Arc::clone(&cache);
        let t1 = thread::spawn(move || {
            for _ in 0..1000 {
                // Thread 1: Liên tục gọi put (chiếm nhiều lock bên trong)
                cache_clone1.put(1, "A_updated".to_string());
            }
        });

        let cache_clone2 = Arc::clone(&cache);
        let t2 = thread::spawn(move || {
            for _ in 0..1000 {
                // Thread 2: Liên tục gọi get (cũng gây move_to_front và chiếm lock)
                cache_clone2.get(&2);
            }
        });

        // Đợi 5 giây. Nếu code đúng O(1) thì 2000 thao tác này phải xong trong < 1s.
        // Nếu sau 5s không xong nghĩa là đã Deadlock.
        let result = thread::spawn(move || {
            t1.join().unwrap();
            t2.join().unwrap();
        });

        // Cơ chế check timeout cho test
        if wait_timeout(result, Duration::from_secs(5)).is_err() {
            panic!(
                "TEST FAILED: Deadlock detected! Cấu trúc nhiều RwLock lồng nhau đã làm treo thread."
            );
        }
    }

    fn wait_timeout<T: 'static>(
        handle: thread::JoinHandle<T>,
        timeout: Duration,
    ) -> Result<(), ()> {
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        // Đợi kết quả từ thread trong khoảng timeout
        rx.recv_timeout(timeout).map_err(|_| ())
    }

    #[test]
    fn prove_deadlock_extremes() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let cache = Arc::new(LruCache::<usize, usize, 32>::new(100));

        // Nạp sẵn dữ liệu để thread 2 luôn rơi vào nhánh move_to_front
        for i in 0..100 {
            cache.put(i, i);
        }

        let cache_clone = cache.clone();
        let t1 = thread::spawn(move || {
            for i in 100..10000 {
                // Thread 1: Liên tục PUT key mới (gây áp lực lên chèn node và cập nhật first/last)
                cache_clone.put(i, i);
            }
        });

        let cache_clone2 = cache.clone();
        let t2 = thread::spawn(move || {
            for _ in 0..10000 {
                // Thread 2: Liên tục GET key cũ (gây áp lực lên move_to_front)
                // move_to_front sẽ chiếm caching.write rồi lại đòi first.write/read
                cache_clone2.get(&50);
            }
        });

        // Nếu không treo, 20.000 ops này phải xong trong < 1 giây
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            t1.join().unwrap();
            t2.join().unwrap();
            let _ = tx.send(());
        });

        if rx.recv_timeout(Duration::from_secs(10)).is_err() {
            panic!("DEADLOCK CONFIRMED: Hệ thống đã treo hoàn toàn sau 10 giây!");
        }
    }

    #[test]
    fn test_no_data_loss_and_leak() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let capacity_per_shard = 100;
        let total_capacity = capacity_per_shard * SHARD_COUNT;
        let evicted_count = Arc::new(AtomicUsize::new(0));

        // Setup cache với callback đếm số lần bị đuổi
        let evicted_clone = Arc::clone(&evicted_count);
        let mut cache = LruCache::<usize, usize, 32>::new(total_capacity);
        cache.on_removing = Some(Arc::new(move |_, _| {
            evicted_clone.fetch_add(1, Ordering::SeqCst);
        }));

        let num_threads = 8;
        let ops_per_thread = 5000;
        let total_ops = num_threads * ops_per_thread;

        thread::scope(|s| {
            for t in 0..num_threads {
                let cache_ref = &cache;
                s.spawn(move || {
                    for i in 0..ops_per_thread {
                        let key = t * ops_per_thread + i;
                        cache_ref.put(key, i);
                    }
                });
            }
        });

        // --- BẮT ĐẦU VALIDATION ---

        // 1. Kiểm tra Mapping size
        // Số lượng phần tử hiện tại phải bằng total_capacity vì chúng ta chèn vượt ngưỡng rất nhiều
        assert_eq!(
            cache.mapping.len(),
            total_capacity,
            "Mapping phải đầy khít capacity"
        );

        // 2. Kiểm tra tính nhất quán của Linked List (Duyệt từng Shard)
        let mut total_nodes_in_lists = 0;
        for i in 0..SHARD_COUNT {
            let ht = cache.shards[i].mutex.lock();
            let mut count = 0;
            let mut curr = ht.first;
            let mut visited = std::collections::HashSet::new();

            while curr != NULL {
                assert!(
                    visited.insert(curr),
                    "Phát hiện chu trình (vòng lặp vô tận) trong Shard {}",
                    i
                );
                count += 1;
                curr = cache.caching[curr].next.load(Ordering::Acquire);
            }
            assert_eq!(
                count, capacity_per_shard,
                "Shard {} bị thiếu node trong danh sách liên kết",
                i
            );
            total_nodes_in_lists += count;
        }
        assert_eq!(total_nodes_in_lists, total_capacity);

        // 3. Kiểm tra số lượng đã bị đuổi (Eviction Balance)
        // Công thức: Tổng Put - Capacity = Số lần phải Evict
        let actual_evicted = evicted_count.load(Ordering::SeqCst);
        let expected_evicted = total_ops - total_capacity;
        assert_eq!(
            actual_evicted, expected_evicted,
            "Số lượng callback xóa không khớp với logic eviction"
        );

        println!("✅ Test passed: Không có dữ liệu bị 'lạc trôi', Linked List hoàn hảo!");
    }
}
