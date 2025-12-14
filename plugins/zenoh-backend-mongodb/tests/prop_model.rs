use proptest::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Put,
    Delete,
}

#[derive(Debug, Clone)]
struct Op {
    ts: i64,
    kind: OpKind,
    nonce: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FinalState {
    Tombstone(i64),
    Value(i64, u32),
}

// Apply the same LWW semantics as the Mongo backend uses:
// - timestamp_raw is the ordering key
// - for ties on timestamp_raw, the last operation wins (nonce keeps payload unique)
fn apply_ops(ops: &[Op]) -> FinalState {
    // ops is generated with len >= 1
    let max_ts = ops.iter().map(|o| o.ts).max().unwrap();
    // Among ops with max_ts, last one wins
    let winner = ops
        .iter()
        .rev()
        .find(|o| o.ts == max_ts)
        .expect("at least one op with max ts");

    match winner.kind {
        OpKind::Put => FinalState::Value(max_ts, winner.nonce),
        OpKind::Delete => FinalState::Tombstone(max_ts),
    }
}

prop_compose! {
    fn arb_op()(
        ts in 0i64..1_000_000,
        kind in prop_oneof![Just(OpKind::Put), Just(OpKind::Delete)],
        nonce in any::<u32>()
    ) -> Op {
        Op { ts, kind, nonce }
    }
}

proptest! {
    #[test]
    fn lww_last_of_max_ts_wins(ops in prop::collection::vec(arb_op(), 1..50)) {
        // Model: apply ops and ensure tie-break is "last among max ts".
        let mut seen_max_ts = None;
        let mut expected = None; // (OpKind, ts)

        for op in &ops {
            match seen_max_ts {
                None => {
                    seen_max_ts = Some(op.ts);
                    expected = Some((op.kind, op.ts, op.nonce));
                }
                Some(current_max) => {
                    if op.ts > current_max {
                        seen_max_ts = Some(op.ts);
                        expected = Some((op.kind, op.ts, op.nonce));
                    } else if op.ts == current_max {
                        // Tie on ts: later op wins
                        expected = Some((op.kind, op.ts, op.nonce));
                    }
                }
            }
        }

        let model_state = apply_ops(&ops);

        match (model_state.clone(), expected) {
            (FinalState::Value(ts1, nonce1), Some((OpKind::Put, ts2, nonce2))) => {
                prop_assert_eq!(ts1, ts2);
                prop_assert_eq!(nonce1, nonce2);
            }
            (FinalState::Tombstone(ts1), Some((OpKind::Delete, ts2, _))) => prop_assert_eq!(ts1, ts2),
            _ => panic!("unexpected state {:?} vs {:?}", model_state, expected),
        }
    }
}
