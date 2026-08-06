//! Integration test: chạy pipeline extract → index → query trên 1 fixture repo
//! tạm, khẳng định đủ 3 phase chạy được, trả số liệu hợp lệ (không cần Criterion).

use std::io::Write;

use camino::Utf8PathBuf;
use codegraph_bench::{orchestrator, sample_query_names, BenchOptions};

/// Dựng fixture repo temp với vài ngôn ngữ, trả `(dir, root)`.
fn fixture() -> (tempfile::TempDir, Utf8PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent.as_std_path()).unwrap();
        }
        let mut f = std::fs::File::create(path.as_std_path()).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    };
    write(
        "src/lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 { a + b }\npub fn sub(a: i32, b: i32) -> i32 { a - b }\n",
    );
    write(
        "main.go",
        "package main\nfunc greet(name string) string { return \"hi \" + name }\nfunc run() { _ = greet(\"x\") }\n",
    );
    write(
        "app.py",
        "def hello(who):\n    return f\"hi {who}\"\n\ndef main():\n    print(hello(\"world\"))\n",
    );
    (dir, root)
}

#[test]
fn pipeline_runs_all_three_phases() {
    let (_dir, root) = fixture();
    let opts = BenchOptions::default();
    let orch = orchestrator(&opts);
    let repo = codegraph_bench::Repo { name: "fixture".into(), root };

    let times = codegraph_bench::measure_repo(&orch, &opts, &repo).unwrap();

    assert!(times.files >= 3, "phải parse được 3 file, thực tế {}", times.files);
    assert!(times.symbols > 0);
    assert!(times.extract_ms >= 0.0);
    assert!(times.index_ms >= 0.0);
    assert!(times.query_ms >= 0.0);
    // Có function/method để query → ít nhất 1 phép đã chạy.
    assert!(times.query_ops > 0, "query phase phải chạy ≥1 phép");
}

#[test]
fn sample_query_names_returns_function_names() {
    let (_dir, root) = fixture();
    let orch = orchestrator(&BenchOptions::default());
    let (parsed, _) = orch.parse_project(&root).unwrap();
    let names = sample_query_names(&parsed, 100);
    // add/sub/greet/hello/main là function — nên có trong danh sách mẫu.
    assert!(names.contains(&"add".to_string()), "names={names:?}");
    assert!(names.contains(&"greet".to_string()));
    assert!(names.len() <= 100);
}