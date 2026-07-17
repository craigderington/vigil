#[test]
fn jitter_within_5pct() {
    for _ in 0..200 {
        let n = vigil::scheduler::next_run_with_jitter(0, 1000);
        assert!((950..=1050).contains(&n));
    }
}
