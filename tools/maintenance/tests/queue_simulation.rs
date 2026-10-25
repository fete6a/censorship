//! Deterministic cost simulation, not a production latency benchmark.
use std::collections::BTreeSet;
#[derive(Default, Debug, PartialEq)]
struct Calls {
    model: usize,
    d1: usize,
    policy: usize,
    acked: usize,
    retried: usize,
    ms: usize,
}
fn simulate(chats: &[i64], reuse: bool, fail: &[usize]) -> Calls {
    let mut cache = BTreeSet::new();
    let mut calls = Calls::default();
    for (i, chat) in chats.iter().enumerate() {
        calls.d1 += 4;
        calls.ms += 4 * 8;
        if !reuse || cache.insert(chat) {
            calls.policy += 1;
            calls.ms += 55;
        }
        calls.model += 1;
        calls.ms += 180;
        if fail.contains(&i) {
            calls.retried += 1;
        } else {
            calls.acked += 1;
        }
    }
    calls
}
#[test]
fn single_group_twenty_preserves_independent_model_reviews() {
    let a = simulate(&[-1001; 20], false, &[]);
    let b = simulate(&[-1001; 20], true, &[]);
    assert_eq!((a.model, b.model), (20, 20));
    assert_eq!((a.d1, b.d1), (80, 80));
    assert_eq!((a.policy, b.policy), (20, 1));
    assert_eq!((a.ms, b.ms), (5340, 4295));
    assert_eq!((a.acked, b.acked), (20, 20));
}
#[test]
fn two_groups_keep_retry_and_ack_isolation() {
    let chats = [vec![-1001; 12], vec![-1002; 8]].concat();
    let a = simulate(&chats, false, &[4, 17]);
    let b = simulate(&chats, true, &[4, 17]);
    assert_eq!((a.policy, b.policy), (20, 2));
    assert_eq!((a.model, b.model), (20, 20));
    assert_eq!((a.d1, b.d1), (80, 80));
    assert_eq!((a.retried, b.retried), (2, 2));
    assert_eq!((a.acked, b.acked), (18, 18));
    assert_eq!((a.ms, b.ms), (5340, 4350));
}
