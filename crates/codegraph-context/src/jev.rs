//! Rerank candidate bằng Jev (TypeSafe SystemOne) — dùng chung cho context,
//! symbol search và document search.
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
//! Mọi lỗi (timeout, 4xx, JSON lạ, thiếu token) đều trả `used = false` để
//! caller **fail-open**: giữ nguyên thứ tự cũ, không làm hỏng tool.

use async_trait::async_trait;
use codegraph_core::{Error, JevConfig, Result, Symbol, NONE_OPTION};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Một ứng viên gửi cho Jev — mọi nguồn (symbol, doc node) quy về đây.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: u64,
    /// Dòng mô tả chính: kind + tên/khoá + vị trí (file:line hoặc doc path).
    pub label: String,
    /// Chi tiết thêm: signature (code) hoặc value (doc) — có thể đã bị cắt.
    pub detail: Option<String>,
}

impl Candidate {
    /// Candidate từ symbol code.
    pub fn symbol(s: &Symbol, strip: Option<&str>) -> Self {
        let sig = s
            .signature
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self {
            id: s.id,
            label: format!(
                "{} `{}` in `{}:{}`",
                s.kind.as_str(),
                s.name,
                crate::rel_path(&s.file, strip),
                s.line
            ),
            detail: sig,
        }
    }

    /// Candidate từ doc node.
    ///
    /// `value` chỉ được đưa vào `detail` khi `cfg.doc_include_values = true`:
    /// value giúp chọn đúng key (`spec.replicas` xuất hiện ở nhiều file) nhưng
    /// tốn token theo số candidate, nên là opt-in.
    pub fn doc(
        id: u64,
        key_path: &str,
        doc_path: &str,
        kind: &str,
        value: Option<&str>,
        cfg: &JevConfig,
    ) -> Self {
        let detail = if cfg.doc_include_values {
            value
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(|v| truncate_chars(v, cfg.doc_value_chars))
        } else {
            None
        };
        Self {
            id,
            label: format!("{kind} `{key_path}` in `{doc_path}`"),
            detail,
        }
    }

    /// Dòng mô tả đưa vào `criteria` của Choice.
    fn describe(&self) -> String {
        match &self.detail {
            Some(d) => format!("{} — {d}", self.label),
            None => self.label.clone(),
        }
    }

    /// Key option trong request/response — `s<id>` để không phụ thuộc tên.
    fn key(&self) -> String {
        format!("s{}", self.id)
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Trạng thái rerank — trả về cho client biết vì sao kết quả trông cũ.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankInfo {
    /// Jev có thực sự quyết định thứ tự không.
    pub used: bool,
    /// Lý do khi `used = false` (tắt, thiếu token, timeout...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Kết quả một lần rerank, đã fail-open.
#[derive(Debug, Clone)]
pub struct RankOutcome {
    pub used: bool,
    pub reason: Option<String>,
    /// `(id, probability)` sort giảm dần. Rỗng khi `used = false`.
    pub ranking: Vec<(u64, f32)>,
    pub no_match: bool,
    /// `confidence` của Choice (0–1) — chỉ để log, không dùng để quyết định.
    pub confidence: Option<f32>,
}

impl RankOutcome {
    /// Không rerank được — caller giữ thứ tự cũ.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            used: false,
            reason: Some(reason.into()),
            ranking: Vec::new(),
            no_match: false,
            confidence: None,
        }
    }

    /// `RerankInfo` để nhét vào response.
    pub fn info(&self) -> RerankInfo {
        RerankInfo {
            used: self.used,
            reason: self.reason.clone(),
        }
    }
}

/// Kết quả rerank thô từ model.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// `(id, probability)` đã sort giảm dần.
    pub ranking: Vec<(u64, f32)>,
    /// Không candidate nào đủ liên quan.
    pub no_match: bool,
    pub confidence: Option<f32>,
    /// Xác suất của option "không có cái nào".
    pub none_probability: f32,
    /// Điểm của câu Noul "có candidate nào khớp?".
    pub fits: f32,
}

impl Verdict {
    /// Verdict "không khớp" — dùng khi pool rỗng.
    pub fn no_match(fits: f32) -> Self {
        Self {
            ranking: Vec::new(),
            no_match: true,
            confidence: None,
            none_probability: 1.0,
            fits,
        }
    }
}

/// Bộ chọn candidate — tách trait để test không cần mạng.
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

/// Chạy rerank với fail-open: dùng `judge` được truyền (test) hoặc tự dựng
/// [`JevJudge`] từ config global. Mọi lỗi → `used = false` + `reason`.
pub async fn rank(
    query: &str,
    repo: Option<&str>,
    candidates: &[Candidate],
    cfg: &JevConfig,
    judge: Option<&dyn Judge>,
) -> RankOutcome {
    if candidates.is_empty() {
        return RankOutcome::unavailable("pool rỗng — không có gì để chọn");
    }
    let owned;
    let judge: &dyn Judge = match judge {
        Some(j) => j,
        None if cfg.is_ready() => {
            owned = JevJudge::new(cfg.clone());
            &owned
        }
        None => {
            return RankOutcome::unavailable(format!(
                "jev không sẵn sàng (enabled={}, env `{}` {})",
                cfg.enabled,
                cfg.api_key_env,
                if cfg.api_key().is_some() {
                    "có"
                } else {
                    "thiếu"
                }
            ));
        }
    };
    match judge.rank(query, repo, candidates).await {
        Ok(v) => RankOutcome {
            used: true,
            reason: None,
            ranking: v.ranking,
            no_match: v.no_match,
            confidence: v.confidence,
        },
        Err(err) => {
            tracing::warn!(error = %err, "Jev rerank thất bại — fail-open");
            RankOutcome::unavailable(format!("jev lỗi: {err}"))
        }
    }
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
    let reply: Reply =
        serde_json::from_str(raw).map_err(|e| Error::Other(format!("Jev trả JSON lỗi: {e}")))?;
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
            label: format!("function `{name}` in `src/lib.rs:10`"),
            detail: Some(format!("fn {name}()")),
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
    fn verdict_sort_theo_xác_suất() {
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
    fn symbol_describe_gồm_kind_tên_và_vị_trí() {
        let c = Candidate {
            id: 7,
            label: "function `load_config` in `crates/a/src/config.rs:42`".into(),
            detail: Some("fn load_config() -> Config".into()),
        };
        assert_eq!(
            c.describe(),
            "function `load_config` in `crates/a/src/config.rs:42` — fn load_config() -> Config"
        );
        assert_eq!(c.key(), "s7");
    }

    #[test]
    fn doc_candidate_không_có_value_khi_tắt() {
        let cfg = JevConfig::default();
        let c = Candidate::doc(
            9,
            "spec.replicas",
            "deploy/api.yaml",
            "Field",
            Some("3"),
            &cfg,
        );
        assert_eq!(c.label, "Field `spec.replicas` in `deploy/api.yaml`");
        assert_eq!(c.detail, None, "mặc định không đưa value (tốn token)");
    }

    #[test]
    fn doc_candidate_có_value_và_cắt_theo_config() {
        let cfg = JevConfig {
            doc_include_values: true,
            doc_value_chars: 5,
            ..JevConfig::default()
        };
        let c = Candidate::doc(
            9,
            "spec.replicas",
            "deploy/api.yaml",
            "Field",
            Some("abcdefghij"),
            &cfg,
        );
        assert_eq!(c.detail.as_deref(), Some("abcde…"));
        assert_eq!(
            c.describe(),
            "Field `spec.replicas` in `deploy/api.yaml` — abcde…"
        );

        let empty = Candidate::doc(10, "a.b", "x.yaml", "Field", Some("   "), &cfg);
        assert_eq!(empty.detail, None, "value rỗng → không thêm detail");
    }

    #[test]
    fn rank_fail_open_khi_judge_lỗi_hoặc_chưa_ready() {
        struct Fail;
        #[async_trait]
        impl Judge for Fail {
            async fn rank(&self, _q: &str, _r: Option<&str>, _c: &[Candidate]) -> Result<Verdict> {
                Err(Error::Other("timeout".into()))
            }
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let cands = vec![cand(1, "alpha")];
        let out = rt.block_on(rank("q", None, &cands, &cfg(), Some(&Fail)));
        assert!(!out.used);
        assert!(out.reason.unwrap_or_default().contains("timeout"));

        let mut off = cfg();
        off.enabled = false;
        let out = rt.block_on(rank("q", None, &cands, &off, None));
        assert!(!out.used);
        assert!(out.reason.unwrap_or_default().contains("enabled=false"));

        let out = rt.block_on(rank("q", None, &[], &cfg(), None));
        assert!(!out.used);
        assert!(out.reason.unwrap_or_default().contains("pool rỗng"));
    }
}
