//! Rerank candidate symbol bằng Jev (TypeSafe SystemOne).
//!
//! Jev không phải chat model: nó nhận `state` + `questions` trả về
//! `probabilities`. Ở đây một request gồm:
//!
//! - `best_symbol` (Choice): mỗi candidate là một option, cộng option
//!   [`codegraph_core::NONE_OPTION`] để model có thể nói "không có cái nào".
//! - `any_fits` (Noul): 0–1, **chỉ để báo cáo/chẩn đoán** — không dùng làm
//!   cổng. Jev trả noul thấp cả khi đã chọn đúng (đo được: top 0.75, noul 0.33),
//!   nên lấy noul làm ngưỡng sẽ chặn oan kết quả tốt.
//!
//! Tín hiệu "không khớp" là option `none_of_the_above` **thắng** (hoặc hoà)
//! xác suất của candidate đứng đầu — không phải `confidence`, vì Choice một
//! mình luôn tự tin kể cả với danh sách vô nghĩa (đo được: 8 candidate rác →
//! `confidence = 1.0`).
//!
//! Mọi lỗi (timeout, 4xx, JSON lạ, thiếu token) đều trả `Err` để caller
//! **fail-open**: giữ nguyên hành vi cũ, không làm hỏng `codegraph_context`.

use crate::rel_path;
use async_trait::async_trait;
use codegraph_core::{Error, JevConfig, Result, Symbol, SymbolKind, NONE_OPTION};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Một ứng viên gửi cho Jev — chỉ giữ những gì model cần để phân biệt.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: u64,
    pub name: String,
    pub kind: SymbolKind,
    /// Đường dẫn tương đối (đã strip workspace root) — tiết kiệm token.
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
}

impl Candidate {
    /// Dựng từ symbol, strip `strip` khỏi `file` như markdown hiển thị.
    pub fn from_symbol(s: &Symbol, strip: Option<&str>) -> Self {
        Self {
            id: s.id,
            name: s.name.clone(),
            kind: s.kind,
            file: rel_path(&s.file, strip).to_string(),
            line: s.line,
            signature: s.signature.clone(),
        }
    }

    /// Dòng mô tả đưa vào `criteria` của Choice.
    fn describe(&self) -> String {
        let sig = self
            .signature
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!(" — {s}"))
            .unwrap_or_default();
        format!(
            "{} `{}` in `{}:{}`{sig}",
            self.kind.as_str(),
            self.name,
            self.file,
            self.line
        )
    }

    /// Key option trong request/response — `s<id>` để không phụ thuộc tên.
    fn key(&self) -> String {
        format!("s{}", self.id)
    }
}

/// Kết quả rerank.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// `(symbol_id, probability)` đã sort giảm dần và **đã lọc** dưới ngưỡng.
    pub ranking: Vec<(u64, f32)>,
    /// Không có symbol nào đủ liên quan — caller trả `hits: []`.
    pub no_match: bool,
    /// `confidence` của Choice (0–1) — chỉ để log, không dùng để quyết định.
    pub confidence: Option<f32>,
    /// Xác suất của option "không có cái nào".
    pub none_probability: f32,
    /// Điểm của câu Noul "có candidate nào khớp?".
    pub fits: f32,
}

impl Verdict {
    /// Verdict "không khớp" — dùng khi không gọi được API hoặc pool quá nhỏ.
    pub fn no_match(reason: f32) -> Self {
        Self {
            ranking: Vec::new(),
            no_match: true,
            confidence: None,
            none_probability: 1.0,
            fits: reason,
        }
    }
}

/// Bộ chọn symbol — tách trait để test không cần mạng.
// `async_trait` sinh `#[must_use]` trên boxed future, trùng với `#[must_use]`
// của `Result` → clippy `double_must_use`. Không phải vấn đề thật.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Judge: Send + Sync {
    /// Trả ranking cho `candidates`; `Err` = caller fail-open.
    async fn rank(
        &self,
        query: &str,
        repo: Option<&str>,
        candidates: &[Candidate],
    ) -> Result<Verdict>;
}

/// `Judge` gọi Jev qua HTTP.
pub struct JevJudge {
    cfg: JevConfig,
}

impl JevJudge {
    pub fn new(cfg: JevConfig) -> Self {
        Self { cfg }
    }
}

#[derive(Serialize)]
struct Ask {
    model: String,
    state: String,
    questions: Questions,
}

#[derive(Serialize)]
struct Questions {
    best_symbol: ChoiceQuestion,
    any_fits: NoulQuestion,
}

#[derive(Serialize)]
struct ChoiceQuestion {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: String,
    criteria: HashMap<String, String>,
}

#[derive(Serialize)]
struct NoulQuestion {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: String,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    choice: Option<String>,
    #[serde(default)]
    probabilities: HashMap<String, f32>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    noul: Option<f32>,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    answers: HashMap<String, Answer>,
}

fn build_state(query: &str, repo: Option<&str>) -> String {
    match repo.filter(|r| !r.trim().is_empty()) {
        Some(repo) => format!("Repo: {repo}. Request: {query}"),
        None => format!("Request: {query}"),
    }
}

/// Map response → Verdict. Tách khỏi HTTP để test được bằng JSON tĩnh.
fn verdict_from_reply(raw: &str, candidates: &[Candidate], cfg: &JevConfig) -> Result<Verdict> {
    let reply: Reply = serde_json::from_str(raw)
        .map_err(|e| Error::Other(format!("Jev trả JSON không hợp lệ: {e}")))?;
    let best = reply
        .answers
        .get("best_symbol")
        .ok_or_else(|| Error::Other("Jev thiếu answer `best_symbol`".into()))?;
    let fits = reply
        .answers
        .get("any_fits")
        .and_then(|a| a.noul)
        .unwrap_or(0.0);

    let none_probability = best.probabilities.get(NONE_OPTION).copied().unwrap_or(0.0);
    let mut ranking: Vec<(u64, f32)> = candidates
        .iter()
        .filter_map(|c| {
            let p = best.probabilities.get(&c.key()).copied().unwrap_or(0.0);
            (p > 0.0).then_some((c.id, p))
        })
        .collect();
    ranking.sort_by(|a, b| b.1.total_cmp(&a.1));

    // `none` thắng hoặc hoà = model thấy "không có cái nào" đáng tin hơn mọi
    // candidate. Noul CHỈ để báo cáo: Jev trả noul thấp cả khi đã chọn đúng
    // (đo được: top 0.75 / noul 0.33), nên dùng nó làm cổng sẽ chặn oan.
    let top = ranking.first().map(|(_, p)| *p).unwrap_or(0.0);
    let no_match = best.choice.as_deref() == Some(NONE_OPTION)
        || ranking.is_empty()
        || none_probability >= top
        || top < cfg.min_probability;

    Ok(Verdict {
        ranking: if no_match { Vec::new() } else { ranking },
        no_match,
        confidence: best.confidence,
        none_probability,
        fits,
    })
}

#[async_trait]
impl Judge for JevJudge {
    async fn rank(
        &self,
        query: &str,
        repo: Option<&str>,
        candidates: &[Candidate],
    ) -> Result<Verdict> {
        if candidates.is_empty() {
            return Ok(Verdict::no_match(0.0));
        }
        let key = self
            .cfg
            .api_key()
            .ok_or_else(|| Error::Other(format!("thiếu env `{}` cho Jev", self.cfg.api_key_env)))?;

        let mut criteria = HashMap::with_capacity(candidates.len() + 1);
        for c in candidates {
            criteria.insert(c.key(), c.describe());
        }
        criteria.insert(
            NONE_OPTION.to_string(),
            "No candidate implements the requested behaviour".into(),
        );

        let ask = Ask {
            model: self.cfg.model.clone(),
            state: build_state(query, repo),
            questions: Questions {
                best_symbol: ChoiceQuestion {
                    kind: "choice",
                    instructions: "Which candidate is the code the request is asking about?".into(),
                    criteria,
                },
                any_fits: NoulQuestion {
                    kind: "noul",
                    instructions:
                        "Does any candidate implement the behaviour described in the request?"
                            .into(),
                },
            },
        };
        let body = serde_json::to_vec(&ask)
            .map_err(|e| Error::Other(format!("serialize request Jev: {e}")))?;

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(self.cfg.timeout_ms))
            .build()
            .map_err(|e| Error::Other(format!("tạo HTTP client cho Jev: {e}")))?;
        let resp = client
            .post(&self.cfg.endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {key}"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("gọi Jev thất bại: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Other(format!("đọc response Jev: {e}")))?;
        if !status.is_success() {
            let snippet: String = text.chars().take(200).collect();
            return Err(Error::Other(format!("Jev trả {status}: {snippet}")));
        }
        verdict_from_reply(&text, candidates, &self.cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, name: &str) -> Candidate {
        Candidate {
            id,
            name: name.into(),
            kind: SymbolKind::Function,
            file: "src/lib.rs".into(),
            line: 10,
            signature: Some(format!("fn {name}()")),
        }
    }

    fn cfg() -> JevConfig {
        JevConfig {
            enabled: true,
            min_probability: 0.25,
            ..JevConfig::default()
        }
    }

    #[test]
    fn verdict_sort_và_lọc_ngưỡng() {
        let cands = vec![cand(1, "alpha"), cand(2, "beta"), cand(3, "gamma")];
        let raw = r#"{"answers":{
            "best_symbol":{"type":"choice","choice":"s2","confidence":0.8,
              "probabilities":{"s1":0.05,"s2":0.7,"s3":0.2,"none_of_the_above":0.05}},
            "any_fits":{"type":"noul","noul":0.9}}}"#;
        let v = verdict_from_reply(raw, &cands, &cfg()).unwrap();
        assert!(!v.no_match);
        assert_eq!(
            v.ranking,
            vec![(2, 0.7), (3, 0.2), (1, 0.05)],
            "giữ đủ thứ tự; ngưỡng chỉ áp cho hạng 1"
        );
        assert_eq!(v.confidence, Some(0.8));
        assert!((v.none_probability - 0.05).abs() < 1e-6);
    }

    #[test]
    fn choice_none_thì_no_match_dù_confidence_cao() {
        let cands = vec![cand(1, "alpha")];
        let raw = r#"{"answers":{
            "best_symbol":{"type":"choice","choice":"none_of_the_above","confidence":0.99,
              "probabilities":{"s1":0.4,"none_of_the_above":0.6}},
            "any_fits":{"type":"noul","noul":0.2}}}"#;
        let v = verdict_from_reply(raw, &cands, &cfg()).unwrap();
        assert!(v.no_match);
        assert!(v.ranking.is_empty());
    }

    #[test]
    fn noul_thấp_không_chặn_kết_quả_choice() {
        let cands = vec![cand(1, "alpha"), cand(2, "beta")];
        let raw = r#"{"answers":{
            "best_symbol":{"type":"choice","choice":"s1","confidence":0.9,
              "probabilities":{"s1":0.6,"s2":0.2,"none_of_the_above":0.2}},
            "any_fits":{"type":"noul","noul":0.3}}}"#;
        let v = verdict_from_reply(raw, &cands, &cfg()).unwrap();
        assert!(!v.no_match, "Noul chỉ để báo cáo, không phải cổng");
        assert_eq!(v.ranking.first().map(|(id, _)| *id), Some(1));
        assert!(
            (v.fits - 0.3).abs() < 1e-6,
            "vẫn giữ điểm noul để chẩn đoán"
        );
    }

    #[test]
    fn mọi_xác_suất_dưới_ngưỡng_thì_no_match() {
        let cands = vec![cand(1, "alpha"), cand(2, "beta")];
        let raw = r#"{"answers":{
            "best_symbol":{"type":"choice","choice":"s1","confidence":0.5,
              "probabilities":{"s1":0.24,"s2":0.2,"none_of_the_above":0.2}},
            "any_fits":{"type":"noul","noul":0.8}}}"#;
        let v = verdict_from_reply(raw, &cands, &cfg()).unwrap();
        assert!(
            v.no_match,
            "hạng 1 = 0.24 < min_probability 0.25 → không khớp"
        );
    }

    #[test]
    fn state_có_repo_khi_cấu_hình() {
        assert_eq!(
            build_state("where is auth", Some("codegraph-rs")),
            "Repo: codegraph-rs. Request: where is auth"
        );
        assert_eq!(build_state("where is auth", None), "Request: where is auth");
        assert_eq!(
            build_state("where is auth", Some("  ")),
            "Request: where is auth"
        );
    }

    #[test]
    fn reply_thiếu_best_symbol_là_lỗi_để_fail_open() {
        let cands = vec![cand(1, "alpha")];
        let err = verdict_from_reply(r#"{"answers":{}}"#, &cands, &cfg()).unwrap_err();
        assert!(err.to_string().contains("best_symbol"));
    }

    #[test]
    fn describe_gồm_kind_tên_và_vị_trí() {
        let c = Candidate {
            id: 7,
            name: "load_config".into(),
            kind: SymbolKind::Function,
            file: "crates/a/src/config.rs".into(),
            line: 42,
            signature: Some("fn load_config() -> Config".into()),
        };
        assert_eq!(
            c.describe(),
            "function `load_config` in `crates/a/src/config.rs:42` — fn load_config() -> Config"
        );
        assert_eq!(c.key(), "s7");
    }
}
