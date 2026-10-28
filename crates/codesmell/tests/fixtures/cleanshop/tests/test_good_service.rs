// Clean test code: `calc_total` is called outside any macro so the extractor
// records the call (macro bodies are token trees, not call nodes).

#[test]
fn calc_total_is_covered() {
    let got = calc_total(1, 2);
    assert_eq!(got, 3);
}
