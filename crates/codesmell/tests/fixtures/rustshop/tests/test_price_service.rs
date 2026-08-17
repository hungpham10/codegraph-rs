// Integration test that exercises `compute_big` so it is considered "tested"
// (and therefore not flagged by testing.missing_test). `unreached_logic` has no
// such caller and is expected to be flagged.

#[test]
fn compute_big_is_covered() {
    let svc = PriceService {};
    let got = svc.compute_big(3);
    assert_eq!(got, 3 + 55);
}
