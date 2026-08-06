//! Diff → graph impact ("draft" analysis).
//!
//! Nhận một **unified diff** (từ MR / patch file / `git diff`), map các dòng đã
//! sửa — phía *new* của hunk, vì index phản ánh working tree = trạng thái "sau
//! khi MR áp dụng" — lên các symbol trong index, rồi tìm call-site nào trong
//! flow nào nằm trong vùng bị sửa, kèm marker context. Hoàn toàn **read-only**:
//! kết quả là một bản draft về tác động lên graph trước khi thay đổi thực sự
//! được index lại.

use crate::GraphIndex;
use codegraph_core::{Error, Result, Symbol, SymbolId, SymbolKind, is_marker, marker_name};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

// ==================== Unified diff parser ====================

/// Một hunk trong diff.
#[derive(Debug, Clone, Serialize)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    /// Số dòng phía new (context + added) — dòng hiện có sau khi MR áp dụng.
    pub new_lines: Vec<u32>,
    /// Số dòng added (`+`) trong hunk.
    pub added: usize,
    /// Số dòng removed (`-`) trong hunk.
    pub removed: usize,
}

/// Một file xuất hiện trong diff.
#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    /// Đường dẫn git-relative (có thể còn prefix `a/`/`b/`).
    pub path: String,
    /// `true` nếu file bị xoá hoàn toàn (phía new rỗng).
    pub deleted: bool,
    pub hunks: Vec<Hunk>,
}

/// Kết quả parse toàn bộ diff.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ParsedDiff {
    pub files: Vec<FileDiff>,
}

/// Parse một unified diff. Chỉ lưu *số dòng* phía new của từng hunk — không cần
/// nội dung. Các header không liên quan (`index …`, mode lines, `Binary files
/// differ`, `rename …`) được bỏ qua.
pub fn parse_unified_diff(input: &str) -> Result<ParsedDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur_path: Option<String> = None;
    let mut cur_deleted = false;
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut hunk: Option<Hunk> = None;
    let mut new_n: u32 = 0;

    // Đóng hunk đang mở vào `hunks`.
    macro_rules! end_hunk {
        () => {
            if let Some(h) = hunk.take() {
                hunks.push(h);
            }
        };
    }

    // Đẩy file hiện tại vào output (đảo hunk + suy deleted từ `+0,0`).
    fn push_file(
        files: &mut Vec<FileDiff>,
        path: String,
        mut deleted: bool,
        hunks: &mut Vec<Hunk>,
    ) {
        if !hunks.is_empty() && hunks.iter().all(|h| h.new_len == 0) {
            deleted = true;
        }
        files.push(FileDiff {
            path,
            deleted,
            hunks: std::mem::take(hunks),
        });
    }

    for raw in input.lines() {
        if let Some(p) = raw.strip_prefix("diff --git ") {
            if let Some(path) = cur_path.take() {
                end_hunk!();
                push_file(&mut files, path, cur_deleted, &mut hunks);
            }
            cur_deleted = false;
            // `diff --git a/x b/y` — lấy phía b/ (đổi tên file cũng rơi vào đây).
            cur_path = p.split_once(" b/").map(|(_, b)| format!("b/{b}"));
        } else if let Some(p) = raw.strip_prefix("+++ ") {
            end_hunk!();
            let p = p.trim();
            if p == "/dev/null" {
                // File bị xoá: giữ path cũ, đánh dấu deleted.
                cur_deleted = true;
            } else {
                cur_path = Some(p.to_string());
                cur_deleted = false;
            }
        } else if let Some(rest) = raw.strip_prefix("--- ") {
            // Path xác nhận phía cũ; path "new" lấy từ `+++` (hoặc `diff --git`).
            end_hunk!();
            let p = rest.trim();
            if cur_path.is_none() && p != "/dev/null" {
                cur_path = Some(p.to_string());
            }
        } else if raw.starts_with("@@ ") {
            end_hunk!();
            let parsed = parse_hunk_header(raw)?;
            new_n = parsed.new_start;
            hunk = Some(parsed);
        } else if raw.starts_with('\\') {
            // `\ No newline at end of file` — không phải dòng nội dung.
        } else if let Some(h) = hunk.as_mut() {
            match raw.as_bytes().first().copied() {
                Some(b' ') => {
                    h.new_lines.push(new_n);
                    new_n += 1;
                }
                Some(b'+') => {
                    h.new_lines.push(new_n);
                    new_n += 1;
                    h.added += 1;
                }
                Some(b'-') => {
                    h.removed += 1;
                }
                // Dòng lạ trong lúc đang mở hunk — coi như hunk kết thúc.
                _ => end_hunk!(),
            }
        }
    }
    if let Some(path) = cur_path {
        end_hunk!();
        push_file(&mut files, path, cur_deleted, &mut hunks);
    }
    Ok(ParsedDiff { files })
}

/// Parse header hunk `@@ -old,count +new,count @@ …`. Count mặc định 1 khi thiếu.
fn parse_hunk_header(line: &str) -> Result<Hunk> {
    let body = line
        .strip_prefix("@@")
        .ok_or_else(|| Error::Invalid(format!("bad hunk header: {line}")))?
        .trim_start()
        .split_once(" @@")
        .map(|(h, _)| h)
        .unwrap_or(line.trim_start_matches("@@").trim_start());
    let (old, new) = body
        .split_once(' ')
        .ok_or_else(|| Error::Invalid(format!("bad hunk header: {line}")))?;
    let (old_start, old_len) = parse_range(old)?;
    let (new_start, new_len) = parse_range(new)?;
    Ok(Hunk {
        old_start,
        old_len,
        new_start,
        new_len,
        new_lines: Vec::new(),
        added: 0,
        removed: 0,
    })
}

/// Parse `-start,count` hoặc `+start,count` (count mặc định 1).
fn parse_range(s: &str) -> Result<(u32, u32)> {
    let s = s
        .strip_prefix('-')
        .or_else(|| s.strip_prefix('+'))
        .ok_or_else(|| Error::Invalid(format!("bad range: {s}")))?;
    match s.split_once(',') {
        Some((a, b)) => Ok((
            a.parse::<u32>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
            b.parse::<u32>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
        )),
        None => Ok((
            s.parse::<u32>()
                .map_err(|e| Error::Invalid(e.to_string()))?,
            1,
        )),
    }
}

// ==================== Diff → graph impact ====================

/// Tóm tắt tổng thể của bản draft.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DiffSummary {
    pub files_in_diff: usize,
    pub files_matched: usize,
    pub symbols_affected: usize,
    pub flows_affected: usize,
    /// File trong diff chưa từng được index (mới thêm / không phải code).
    pub new_files: Vec<String>,
    /// File trong diff không khớp được file nào trong index (vd rename / non-code).
    pub unmatched_files: Vec<String>,
}

/// Một call-site nằm trong vùng dòng bị sửa của flow.
#[derive(Debug, Clone, Serialize)]
pub struct DiffAffectedCall {
    pub position: usize,
    pub callee: String,
    pub to_id: Option<SymbolId>,
    pub line: u32,
    /// Marker guard đứng ngay trước call-site trong chain (ngoài → trong).
    pub markers: Vec<String>,
}

/// Một flow bị ảnh hưởng (hàm có body chứa dòng đã sửa).
#[derive(Debug, Clone, Serialize)]
pub struct DiffFlow {
    pub id: SymbolId,
    pub name: String,
    pub file: String,
    pub line: u32,
    /// Call-site nằm trong vùng dòng bị sửa.
    pub affected_calls: Vec<DiffAffectedCall>,
    /// Các marker xuất hiện trong khoảng chain giữa call-site đầu/cuối bị ảnh hưởng.
    pub marker_window: Vec<String>,
    /// Caller trực tiếp (flow phụ thuộc gián tiếp — hàm bị sửa được ai gọi).
    pub called_by: Vec<Symbol>,
}

/// Một symbol bị ảnh hưởng.
#[derive(Debug, Clone, Serialize)]
pub struct DiffSymbol {
    pub symbol: Symbol,
    /// `"modified"` hoặc `"removed"` (file bị xoá).
    pub impact: String,
}

/// Chi tiết per-file trong draft.
#[derive(Debug, Clone, Serialize)]
pub struct DiffFile {
    /// Đường dẫn trong diff (giữ nguyên, không prefix).
    pub path: String,
    pub matched: bool,
    /// Đường dẫn trong index khớp được (`None` nếu chưa được index).
    pub matched_path: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
    pub deleted: bool,
    pub symbols: Vec<DiffSymbol>,
    pub flows: Vec<DiffFlow>,
}

/// Bản draft — kết quả phân tích tác động của diff lên graph hiện tại.
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    /// Đánh dấu đây là bản draft read-only — chưa được áp vào graph/index.
    pub draft: bool,
    pub summary: DiffSummary,
    pub files: Vec<DiffFile>,
}

impl GraphIndex {
    /// Phân tích tác động của một diff lên index hiện tại (draft).
    ///
    /// `root` là đường dẫn gốc workspace — dùng để nối khi path trong diff là
    /// git-relative mà `Symbol.file` trong index là absolute. Không đọc/sửa file
    /// nào, không mutate index.
    pub async fn diff_assess(
        &self,
        parsed: &ParsedDiff,
        root: Option<&std::path::Path>,
    ) -> DiffReport {
        // Group symbol theo file (key = `Symbol.file` trong index).
        let mut by_file: HashMap<&str, Vec<&Symbol>> = HashMap::new();
        for s in self.symbols.values() {
            by_file.entry(s.file.as_str()).or_default().push(s);
        }

        let mut report_files = Vec::new();
        let mut summary = DiffSummary {
            files_in_diff: parsed.files.len(),
            ..Default::default()
        };

        for fd in &parsed.files {
            let rel = strip_git_prefix(&fd.path);
            let matched_key = find_matching_file(&by_file, rel, root);

            let mut file_out = DiffFile {
                path: rel.to_string(),
                matched: matched_key.is_some(),
                matched_path: matched_key.map(|k| k.to_string()),
                added_lines: fd.hunks.iter().map(|h| h.added).sum(),
                removed_lines: fd.hunks.iter().map(|h| h.removed).sum(),
                deleted: fd.deleted,
                symbols: Vec::new(),
                flows: Vec::new(),
            };

            let new_lines: HashSet<u32> = fd
                .hunks
                .iter()
                .flat_map(|h| h.new_lines.iter().copied())
                .collect();

            match matched_key {
                None => {
                    // Chưa từng được index: file mới (old_len 0) hay chưa match.
                    let is_new = fd.hunks.iter().all(|h| h.old_len == 0);
                    if fd.deleted {
                        // File xoá nhưng không có trong index — không có gì để báo.
                    } else if is_new && !fd.hunks.is_empty() {
                        summary.new_files.push(rel.to_string());
                    } else {
                        summary.unmatched_files.push(rel.to_string());
                    }
                }
                Some(key) => {
                    summary.files_matched += 1;
                    let symbols = by_file.get(key).cloned().unwrap_or_default();
                    for s in symbols {
                        if fd.deleted || new_lines.is_empty() {
                            // File bị xoá hoàn toàn: mọi symbol của file bị xoá.
                            if fd.deleted {
                                summary.symbols_affected += 1;
                                file_out.symbols.push(DiffSymbol {
                                    symbol: (*s).clone(),
                                    impact: "removed".into(),
                                });
                            }
                            continue;
                        }
                        if !symbol_overlaps(s, &new_lines) {
                            continue;
                        }
                        summary.symbols_affected += 1;
                        file_out.symbols.push(DiffSymbol {
                            symbol: (*s).clone(),
                            impact: "modified".into(),
                        });
                        if !matches!(s.kind, SymbolKind::Function | SymbolKind::Method) {
                            continue;
                        }
                        // Flow impact cho hàm/method bị chạm.
                        if let Some(flow) = self.diff_flow(s, &new_lines).await {
                            summary.flows_affected += 1;
                            file_out.flows.push(flow);
                        }
                    }
                }
            }
            report_files.push(file_out);
        }

        DiffReport {
            draft: true,
            summary,
            files: report_files,
        }
    }

    /// Flow impact của một hàm bị chạm: call-site nằm trong vùng sửa + marker
    /// context + caller trực tiếp.
    async fn diff_flow(&self, sym: &Symbol, new_lines: &HashSet<u32>) -> Option<DiffFlow> {
        let flow = self.flow(sym.id).await.ok()?;
        let mut affected_calls = Vec::new();
        let mut first = usize::MAX;
        let mut last = 0;
        for call in &flow.calls {
            if new_lines.contains(&call.line) {
                affected_calls.push(DiffAffectedCall {
                    position: call.position,
                    callee: call.to_name.clone(),
                    to_id: call.to_id,
                    line: call.line,
                    markers: guard_markers(&flow.chain, call.position),
                });
                first = first.min(call.position);
                last = last.max(call.position);
            }
        }
        if affected_calls.is_empty() {
            return None;
        }
        let called_by = self.callers(sym.id, 1).await.unwrap_or_default();
        Some(DiffFlow {
            id: sym.id,
            name: sym.name.clone(),
            file: sym.file.clone(),
            line: sym.line,
            marker_window: marker_window(&flow.chain, first, last),
            affected_calls,
            called_by,
        })
    }
}

/// Marker guard trực tiếp của một call-site: walk ngược từ `position-1` trong
/// chain, gom các marker liên tiếp (dừng khi gặp phần tử không phải marker).
fn guard_markers(chain: &[u64], position: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = position;
    while i > 0 {
        i -= 1;
        let e = chain[i];
        if !is_marker(e) {
            break;
        }
        if let Some(n) = marker_name(e) {
            out.push(n.to_string());
        }
    }
    out.reverse(); // ngoài → trong
    out
}

/// Các marker xuất hiện trong khoảng từ marker guard của call-site đầu tiên đến
/// call-site cuối cùng bị ảnh hưởng (dedupe, giữ thứ tự).
fn marker_window(chain: &[u64], first: usize, last: usize) -> Vec<String> {
    // Điểm bắt đầu: lùi về marker trực tiếp trước call-site đầu tiên.
    let mut start = first;
    while start > 0 && is_marker(chain[start - 1]) {
        start -= 1;
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for &e in &chain[start..=last.min(chain.len().saturating_sub(1))] {
        if let Some(n) = marker_name(e)
            && seen.insert(n)
        {
            out.push(n.to_string());
        }
    }
    out
}

/// Bỏ tiền tố git `a/`/`b/` (tối đa một lần).
fn strip_git_prefix(path: &str) -> &str {
    if let Some(rest) = path.strip_prefix("a/") {
        rest
    } else if let Some(rest) = path.strip_prefix("b/") {
        rest
    } else {
        path
    }
}

/// Tìm file trong index khớp với path của diff: exact (hoặc root.join) trước,
/// rồi suffix-match (`/rel`).
fn find_matching_file<'a>(
    by_file: &'a HashMap<&'a str, Vec<&'a Symbol>>,
    rel: &str,
    root: Option<&std::path::Path>,
) -> Option<&'a str> {
    let mut candidates = Vec::new();
    candidates.push(rel.to_string());
    if let Some(r) = root {
        candidates.push(r.join(rel).to_string_lossy().into_owned());
    }
    for c in &candidates {
        if let Some(k) = by_file.get_key_value(c.as_str()) {
            return Some(k.0);
        }
    }
    let suffix = format!("/{rel}");
    by_file.keys().find(|k| k.ends_with(&suffix)).copied()
}

/// Symbol có vùng `line..=end_line` chạm một trong các dòng đã sửa (phía new).
fn symbol_overlaps(s: &Symbol, new_lines: &HashSet<u32>) -> bool {
    new_lines.iter().any(|&l| s.line <= l && l <= s.end_line)
}

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{
        CallRecord, EffectType, MARKER_BRANCH_END, MARKER_IF_TRUE, SYMBOL_BASE, ScopeLevel,
    };

    fn sym(file: &str, name: &str, id: u64, line: u32, end_line: u32) -> Symbol {
        Symbol {
            id,
            name: name.to_string(),
            kind: SymbolKind::Function,
            scope: ScopeLevel::Global,
            scope_id: 0,
            type_ref: 0,
            type_name: None,
            file: file.to_string(),
            line,
            end_line,
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
    ) -> crate::ParseResult {
        crate::ParseResult {
            path: path.to_string(),
            language: "test".to_string(),
            bytes: 0,
            lines: 0,
            symbols,
            chains,
            calls,
        }
    }

    // ── parser ──

    #[test]
    fn parse_single_file_hunks() {
        let diff = "\
diff --git a/src/a.ts b/src/a.ts
index 1111111..2222222 100644
--- a/src/a.ts
+++ b/src/a.ts
@@ -10,4 +10,5 @@ fn main() {
     let x = 1;
     let y = 2;
+    let z = 3;
     foo();
 }
";
        let p = parse_unified_diff(diff).unwrap();
        assert_eq!(p.files.len(), 1);
        let f = &p.files[0];
        assert_eq!(f.path, "b/src/a.ts");
        assert!(!f.deleted);
        assert_eq!(f.hunks.len(), 1);
        let h = &f.hunks[0];
        assert_eq!(
            (h.old_start, h.old_len, h.new_start, h.new_len),
            (10, 4, 10, 5)
        );
        assert_eq!(h.added, 1);
        assert_eq!(h.removed, 0);
        // context (10,11,13,14) + added (12) → dòng new {10,11,12,13,14}.
        assert_eq!(h.new_lines, vec![10, 11, 12, 13, 14]);
    }

    #[test]
    fn parse_deleted_and_new_files() {
        let diff = "\
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
index 1111111..0000000
--- a/gone.rs
+++ /dev/null
@@ -1,3 +0,0 @@
-fn old() {}
-fn old2() {}
diff --git a/fresh.rs b/fresh.rs
new file mode 100644
index 0000000..2222222
--- /dev/null
+++ b/fresh.rs
@@ -0,0 +1,2 @@
+fn new_fn() {}
+fn new_fn2() {}
";
        let p = parse_unified_diff(diff).unwrap();
        assert_eq!(p.files.len(), 2);
        assert!(p.files[0].deleted);
        assert_eq!(p.files[0].hunks[0].new_len, 0);
        assert!(!p.files[1].deleted);
        assert_eq!(p.files[1].hunks[0].old_len, 0);
        assert_eq!(p.files[1].hunks[0].new_lines, vec![1, 2]);
    }

    #[test]
    fn parse_crlf_and_no_newline() {
        let diff = concat!(
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -1,2 +1,3 @@\n",
            " a\r\n",
            "+b\r\n",
            "\\ No newline at end of file\n",
        );
        let p = parse_unified_diff(diff).unwrap();
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].hunks[0].new_lines, vec![1, 2]);
        assert_eq!(p.files[0].hunks[0].added, 1);
    }

    #[test]
    fn parse_bad_header_errors() {
        assert!(parse_unified_diff("@@ nope @@").is_err());
    }

    // ── assess ──

    #[tokio::test]
    async fn assess_marks_flow_and_call_sites() {
        let mut idx = GraphIndex::in_memory();
        let process = SYMBOL_BASE;
        let fetch = SYMBOL_BASE + 1;
        let main = SYMBOL_BASE + 2;
        let chains = HashMap::from([
            // process: IF_TRUE → fetch
            (
                process,
                vec![process, MARKER_IF_TRUE, fetch, MARKER_BRANCH_END],
            ),
            (main, vec![main, process]),
        ]);
        let calls = vec![CallRecord {
            caller_id: process,
            call_name: "fetch".into(),
            position: 2,
            arg_exprs: Vec::new(),
            line: 12,
            condition: None,
            is_loop_body: false,
            effect: EffectType::None,
            effect_desc: None,
            target_class: None,
            target_method: None,
        }];
        let r = result(
            "a.ts",
            vec![
                sym("a.ts", "process", process, 1, 30),
                sym("b.ts", "fetch", fetch, 1, 10),
                sym("a.ts", "main", main, 40, 60),
            ],
            chains,
            calls,
        );
        idx.ingest(&[r]).await.unwrap();

        let diff = "\
--- a/a.ts
+++ b/a.ts
@@ -9,4 +9,4 @@
     let y = 2;
     foo();
+    bar();
 }
";
        let parsed = parse_unified_diff(diff).unwrap();
        let report = idx.diff_assess(&parsed, None).await;

        assert!(report.draft);
        assert_eq!(report.summary.files_matched, 1);
        assert_eq!(report.summary.symbols_affected, 1);
        assert_eq!(report.summary.flows_affected, 1);

        let f = &report.files[0];
        assert!(f.matched);
        assert_eq!(f.symbols.len(), 1);
        assert_eq!(f.symbols[0].symbol.name, "process");
        assert_eq!(f.symbols[0].impact, "modified");

        let fl = &f.flows[0];
        assert_eq!(fl.name, "process");
        assert_eq!(fl.affected_calls.len(), 1);
        let call = &fl.affected_calls[0];
        assert_eq!(call.callee, "fetch");
        assert_eq!(call.line, 12);
        assert_eq!(call.markers, vec!["IF_TRUE"]);
        assert!(fl.marker_window.contains(&"IF_TRUE".to_string()));
        // main gọi process → dependent flow.
        assert_eq!(fl.called_by.len(), 1);
        assert_eq!(fl.called_by[0].name, "main");
    }

    #[tokio::test]
    async fn assess_removed_file() {
        let mut idx = GraphIndex::in_memory();
        let f = SYMBOL_BASE;
        let chains = HashMap::from([(f, vec![f])]);
        let r = result(
            "old.rs",
            vec![sym("old.rs", "old_fn", f, 1, 5)],
            chains,
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();

        let diff = "\
--- a/old.rs
+++ /dev/null
@@ -1,5 +0,0 @@
-fn old_fn() {}
";
        let parsed = parse_unified_diff(diff).unwrap();
        let report = idx.diff_assess(&parsed, None).await;
        let f = &report.files[0];
        assert!(f.matched);
        assert!(f.deleted);
        assert_eq!(f.symbols.len(), 1);
        assert_eq!(f.symbols[0].impact, "removed");
    }

    #[tokio::test]
    async fn assess_path_matching_with_root() {
        let mut idx = GraphIndex::in_memory();
        let f = SYMBOL_BASE;
        let chains = HashMap::from([(f, vec![f])]);
        // Index lưu path absolute.
        let r = result(
            "/work/repo/src/a.rs",
            vec![sym("/work/repo/src/a.rs", "a_fn", f, 1, 5)],
            chains,
            vec![],
        );
        idx.ingest(&[r]).await.unwrap();

        // Diff git-relative, root = /work/repo → khớp.
        let diff = "\
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,3 @@
 fn a_fn() {
-    x();
+    y();
 }
";
        let parsed = parse_unified_diff(diff).unwrap();
        let report = idx
            .diff_assess(&parsed, Some(std::path::Path::new("/work/repo")))
            .await;
        assert!(report.files[0].matched);
        assert_eq!(
            report.files[0].matched_path.as_deref(),
            Some("/work/repo/src/a.rs")
        );

        // Không có root → suffix match vẫn ăn.
        let report2 = idx.diff_assess(&parsed, None).await;
        assert!(report2.files[0].matched);
    }
}
