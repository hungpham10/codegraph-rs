//! End-to-end: compile một shared library thật → r2 extract → BinaryGraph
//! ingest → callees/callers/flow không rỗng. Bỏ qua nếu không có `cc`/`r2`.

#![cfg(feature = "binary")]

use camino::Utf8Path;

#[tokio::test]
async fn real_so_callees_flow() {
    if which_failed("cc") || which_failed("r2") {
        eprintln!("skip: cc hoặc r2 không có trong PATH");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let dir_path = Utf8Path::from_path(dir.path()).unwrap();

    let c = r#"
int helper(int x) { return x + 1; }
int entry_fn(int x) { return helper(x) * 2; }
"#;
    std::fs::write(dir.path().join("tiny.c"), c).unwrap();
    let so = dir.path().join("libtiny.so");
    let status = std::process::Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&so)
        .arg(dir.path().join("tiny.c"))
        .status()
        .expect("chạy cc");
    assert!(status.success(), "cc thất bại");

    let mut cfg = codegraph_extract::ExtractConfig::load(dir_path);
    // Tắt cache — retry phải extract lại thật, không trả kết quả cũ.
    cfg.binary.cache = false;

    // r2 đôi lúc analyze không recover được call ops của dylib (không xác định
    // được entrypoint) — retry extract tối đa 3 lần trước khi kết luận fail.
    let (_parsed, g) = {
        let mut ok = None;
        for attempt in 1..=3 {
            let (batch, skipped) = codegraph_binary::collect_binaries(dir_path, &cfg.binary);
            assert_eq!(skipped, 0);
            assert_eq!(batch.len(), 1, "phải tìm thấy libtiny.so");
            let g = codegraph_extract::BinaryGraph::open(None, 2_000_000_000)
                .await
                .unwrap();
            for p in &batch {
                g.ingest(p, 2_000_000_000).await.unwrap();
            }
            if extracted_has_calls(&g, &batch).await {
                ok = Some((batch, g));
                break;
            }
            eprintln!("attempt {attempt}: r2 không extract được call ops — retry");
        }
        ok.expect("r2 không extract được call ops nào sau 3 lần thử")
    };

    // Tìm entry_fn qua search tên.
    let page = g
        .search_name(
            "entry_fn",
            codegraph_extract::NameMatch::Contains,
            None,
            None,
            0,
            10,
        )
        .await
        .unwrap();
    assert_eq!(page.total, 1, "entry_fn phải được extract");
    let entry_id = page.rows[0].id;

    // callees — entry_fn gọi helper: không được rỗng (bug cũ: luôn rỗng vì
    // query nhầm vào GraphIndex chính).
    let callees = g.callees(entry_id).await.unwrap();
    assert!(
        callees.iter().any(|s| s.name.contains("helper")),
        "entry_fn phải gọi helper, callees = {:?}",
        callees.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // callers — helper được entry_fn gọi.
    let helper = callees
        .iter()
        .find(|s| s.name.contains("helper"))
        .expect("helper phải nằm trong callees");
    let callers = g.callers(helper.id, 1).await.unwrap();
    assert!(
        callers.iter().any(|s| s.name.contains("entry_fn")),
        "helper phải có caller entry_fn, callers = {:?}",
        callers.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // flow — chain có ít nhất symbol + 1 call site, chain_desc hiển thị tên.
    let flow = g.flow(entry_id).await.unwrap();
    assert_eq!(flow.symbol.name, page.rows[0].name);
    assert!(!flow.calls.is_empty(), "flow.calls không được rỗng");
    assert!(flow
        .chain_desc
        .iter()
        .any(|d| d.contains("helper") || d.contains("entry_fn")));
}

fn which_failed(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .is_err()
}

/// Extract được coi là thành công khi có ít nhất một chain chứa call site
/// (element ngoài self/marker).
async fn extracted_has_calls(
    g: &codegraph_extract::BinaryGraph,
    parsed: &[codegraph_graph::ParseResult],
) -> bool {
    for p in parsed {
        for local_id in p.chains.keys() {
            let id = 2_000_000_000 + local_id;
            if !g.callees(id).await.unwrap_or_default().is_empty() {
                return true;
            }
        }
    }
    false
}
