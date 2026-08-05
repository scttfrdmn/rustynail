//! Conformance against quarry's own frozen event-stream corpus.
//!
//! # Why these tests are different from every other quarry test here
//!
//! Everything else in this repo asserts our reader against fixtures *we* wrote,
//! which can only ever confirm we are self-consistent. This suite reads bytes
//! **quarry produced**, vendored verbatim under `tests/fixtures/quarry/runevents/`,
//! and compares our reading against quarry's own restatement of the same bytes in
//! integers. Two independent readings of one wire capture.
//!
//! That distinction is not academic. This integration shipped with `--events-json`
//! missing from its argv for four commits while every test stayed green, because the
//! fake emitted its canned NDJSON regardless of what it was invoked with. A fixture
//! we authored cannot fail us in the ways a captured one does.
//!
//! # What the corpus is for
//!
//! Upstream built it explicitly for both twins — its README names *"bucktooth (Go)
//! and rustynail (Rust)"* — and it is **frozen**, not regenerated: the
//! budget-degraded case is unreachable under `--fake` (the fake's per-call cost is
//! uniform, so affordability either funds every child or declines the split), and the
//! time-truncated cases are wall-clock races whose shape already changed once on the
//! capturing machine. Determinism is claimed for the *fold*, which is pure, not for
//! the runs. See the vendored `README.md` for provenance and the staleness rule.
//!
//! # The three rules under test
//!
//! 1. **Money is integers.** Convert each row with `round(× 1e6)` *before* summing.
//!    Two cases carry `float_sum_equals_total: false` and exist to fail a host that
//!    sums the floats.
//! 2. **Absence is not zero**, in three places: a missing `provenance` object means
//!    quarry declined to publish a stability rate, never that it was 0; `cap_micros:
//!    -1` means no spend cap, not a cap of nothing; `bound_by: ""` means no cap bound
//!    the run and is emitted *because it is a measurement*.
//! 3. **Gaps and unfunded are different denominations and are never summed.** Only
//!    time makes a gap. A host that added them would offer more time where money was
//!    needed.

use rustynail::quarry::{
    parse_line, stream_version, terminal_outcome, RunEvent, StreamStats, SUPPORTED_STREAM_VERSION,
};
use std::path::PathBuf;

// ── Corpus loading ────────────────────────────────────────────────────────────

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quarry/runevents")
}

/// Every case with an `.expected.json`, i.e. every case whose bytes parse into a
/// complete set.
///
/// `crashed` is deliberately excluded here and tested on its own: it is cut mid-line
/// inside the artifact event, so there is no expectation to state.
const CASES: &[&str] = &[
    "complete",
    "deadline-only",
    "live-partition",
    "no-answer-spend",
    "no-answer-time",
    "time-truncated",
    "unicode",
    "unicode-long",
    "unknown-kind",
];

/// Parse a case's `.ndjson` the way the supervisor parses a live stream: line by
/// line, tolerating what it must tolerate, counting what it skips.
fn read_stream(case: &str) -> (Vec<RunEvent>, StreamStats) {
    let path = corpus_dir().join(format!("{case}.ndjson"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("corpus case {case} unreadable at {}: {e}", path.display()));
    let mut events = Vec::new();
    let mut stats = StreamStats::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        stats.lines += 1;
        match parse_line(line) {
            Ok(event) => {
                if let RunEvent::Unknown { event_type, .. } = &event {
                    *stats.unknown_kinds.entry(event_type.clone()).or_insert(0) += 1;
                }
                stats.events += 1;
                events.push(event);
            }
            Err(e) => stats.bad_lines.push((i + 1, e)),
        }
    }
    (events, stats)
}

/// quarry's own restatement of the same stream, in integers.
fn read_expected(case: &str) -> serde_json::Value {
    let path = corpus_dir().join(format!("{case}.expected.json"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("expectations for {case}: {e}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("expectations for {case}: {e}"))
}

fn i64_at(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_i64())
        .unwrap_or_else(|| panic!("expectation missing i64 `{key}`"))
}

// ── The corpus is actually here ───────────────────────────────────────────────

#[test]
fn the_corpus_is_vendored_and_complete() {
    // The guard that stops every test below from passing vacuously. A missing corpus
    // would otherwise make each `read_stream` panic — but a *partial* one would
    // silently narrow coverage, which is the failure mode that reads as success.
    let dir = corpus_dir();
    assert!(dir.is_dir(), "corpus not vendored at {}", dir.display());
    for case in CASES {
        for suffix in ["ndjson", "expected.json"] {
            let f = dir.join(format!("{case}.{suffix}"));
            assert!(f.is_file(), "missing corpus file {}", f.display());
        }
    }
    assert!(
        dir.join("crashed.ndjson").is_file(),
        "the mid-line truncation case is what proves a severed stream is detected"
    );
    assert!(
        !dir.join("crashed.expected.json").exists(),
        "the crashed case must have no expectations: its bytes do not parse into a \
         complete set, so there is nothing to state"
    );
}

// ── Framing ───────────────────────────────────────────────────────────────────

#[test]
fn every_case_declares_the_version_we_implement() {
    // The version is on the first line so a host can *refuse* a stream it cannot
    // read. If a re-vendored corpus ever declares 2, this test failing is the point:
    // it means a field changed meaning and our reader needs review, not a bumped
    // constant.
    for case in CASES {
        let (events, _) = read_stream(case);
        let declared = stream_version(&events)
            .unwrap_or_else(|| panic!("{case}: no quarry_stream header — the frame never opened"));
        assert_eq!(
            declared, SUPPORTED_STREAM_VERSION,
            "{case}: declares stream version {declared}"
        );
        assert_eq!(
            declared,
            i64_at(&read_expected(case), "stream_version") as u32
        );
    }
}

#[test]
fn every_case_closes_its_frame_and_the_verdict_matches() {
    // The terminal event is quarry's own classification, and where it exists we take
    // it over anything we could infer. Its absence is the only in-band way to tell a
    // killed run from a completed one, which is why every intact case must have it.
    for case in CASES {
        let (events, _) = read_stream(case);
        let expected = read_expected(case);
        let outcome = terminal_outcome(&events).unwrap_or_else(|| {
            panic!("{case}: no terminal quarry_outcome — the frame never closed")
        });

        assert_eq!(
            outcome.outcome,
            expected["outcome"].as_str().unwrap(),
            "{case}: outcome"
        );
        assert_eq!(
            outcome.bound_by,
            expected["bound_by"].as_str().unwrap(),
            "{case}: bound_by"
        );
        assert_eq!(
            outcome.gaps as i64,
            i64_at(&expected, "gaps"),
            "{case}: gaps"
        );
        assert_eq!(
            outcome.unfunded as i64,
            i64_at(&expected, "unfunded"),
            "{case}: unfunded"
        );
    }
}

#[test]
fn an_unknown_kind_mid_stream_does_not_disturb_the_frame() {
    // `unknown-kind` puts a `quarry_future_kind` event between the answer and the
    // receipt: adding a *kind* is a minor change a host must tolerate, and only a
    // changed field or a changed meaning bumps the version.
    //
    // Note this case does NOT place the new kind after the outcome, so it cannot
    // prove the terminal event is found by scanning backwards — that lives in
    // `supervisor.rs`'s `the_terminal_event_is_found_by_scanning_backwards`, on a
    // hand-built stream, precisely because the corpus has no fixture for it. A
    // trailing unknown kind is the case a reader taking `events.last()` breaks on,
    // and it is worth asking upstream for.
    let (events, stats) = read_stream("unknown-kind");
    assert!(
        stats.unknown_kinds.contains_key("quarry_future_kind"),
        "the synthetic future kind must survive as Unknown, not be rejected"
    );
    assert!(
        stats.bad_lines.is_empty(),
        "an unknown kind is not a bad line — quarry's union is open by design"
    );
    assert_eq!(
        terminal_outcome(&events).map(|o| o.outcome.as_str()),
        Some("complete")
    );
    assert!(
        !stats.unknown_kinds.contains_key("quarry_stream")
            && !stats.unknown_kinds.contains_key("quarry_outcome"),
        "the frame's own events are first-class here — reading them as unknown kinds \
         is exactly what discarded the version and the terminal verdict"
    );
}

#[test]
fn every_case_carries_the_event_types_quarry_says_it_does() {
    // Six kinds, not four. An earlier reading of this integration knew only agate's
    // four and dropped quarry's two framing events into `Unknown`, discarding both the
    // version and the verdict.
    for case in CASES {
        let (events, _) = read_stream(case);
        let expected = read_expected(case);
        let want: Vec<&str> = expected["event_types"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for kind in &want {
            assert!(
                events.iter().any(|e| e.event_type() == *kind),
                "{case}: expected a `{kind}` event"
            );
        }
        assert_eq!(
            events.first().map(|e| e.event_type()),
            Some("quarry_stream"),
            "{case}: the frame's header is always first"
        );
    }
}

#[test]
fn a_producer_is_named_on_every_stream() {
    // Recorded because there is a parallel Python quarry: the two agree on behaviour
    // but are not the same code, and a host reading a vendored capture months later
    // needs to know which wrote it.
    for case in CASES {
        let (events, _) = read_stream(case);
        let producer = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Stream(s) => Some(s.producer.clone()),
                _ => None,
            })
            .unwrap_or_default();
        assert_eq!(
            producer,
            read_expected(case)["producer"].as_str().unwrap(),
            "{case}: producer"
        );
    }
}

// ── Money is integers ─────────────────────────────────────────────────────────

#[test]
fn spend_is_read_as_an_integer_and_never_re_derived_from_floats() {
    // `total_micros` is the one figure on this stream that is not a float, carried on
    // quarry's own event precisely so a host has nothing to reconcile. Reading it
    // rather than summing the receipt is the whole rule.
    for case in CASES {
        let (events, _) = read_stream(case);
        let outcome = terminal_outcome(&events).unwrap();
        assert_eq!(
            outcome.total_micros,
            i64_at(&read_expected(case), "total_micros"),
            "{case}: total_micros must come off the wire as an integer"
        );
    }
}

#[test]
fn rows_are_summed_in_micro_units_not_in_float64() {
    // The two cases that fail a host which sums the floats: `complete`'s three rows
    // already do not sum to its total in float64, and `live-partition` misses by
    // 1.4e-17. Converting each row with `round(× 1e6)` FIRST is what makes the
    // comparison exact — and `int()` instead of `round()` is the named defect.
    let mut saw_a_float_trap = false;
    for case in CASES {
        let (events, _) = read_stream(case);
        let expected = read_expected(case);
        let receipt = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Receipt(r) => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{case}: no receipt event"));

        assert_eq!(
            receipt.total_micro_usd(),
            i64_at(&expected["receipt"], "total_micros"),
            "{case}: receipt total in micro-units"
        );
        assert_eq!(
            receipt.rows_reconcile(),
            expected["receipt"]["reconciles"].as_bool().unwrap(),
            "{case}: rows must reconcile against the stated total in integers"
        );

        // Where quarry says the floats do NOT sum, prove ours do not either — a host
        // whose float sum happened to match would not be exercising this at all.
        if !expected["receipt"]["float_sum_equals_total"]
            .as_bool()
            .unwrap()
        {
            saw_a_float_trap = true;
            let float_sum: f64 = receipt.rows.iter().map(|r| r.cost).sum();
            assert_ne!(
                float_sum, receipt.total,
                "{case}: quarry says the float sum differs from the total; if it \
                 matches here the corpus has been regenerated and this trap is gone"
            );
            assert!(
                receipt.rows_reconcile(),
                "{case}: and yet the INTEGER sum must still be exact — that is the \
                 entire point of converting before summing"
            );
        }
    }
    assert!(
        saw_a_float_trap,
        "at least one case must carry float_sum_equals_total: false, or this test \
         proves nothing about integer conversion"
    );
}

#[test]
fn a_round_dollar_cost_still_parses_as_a_number() {
    // `unknown-kind` is the only case with round-dollar costs, which Go serialises as
    // `"cost":1` with no decimal point. A reader that required a fractional part —
    // or that typed the field as anything but a float — breaks on it.
    let (events, _) = read_stream("unknown-kind");
    let receipt = events
        .iter()
        .find_map(|e| match e {
            RunEvent::Receipt(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(
        receipt
            .rows
            .iter()
            .any(|r| r.cost.fract() == 0.0 && r.cost > 0.0),
        "the round-dollar row is what this case exists for"
    );
    assert_eq!(receipt.total_micro_usd(), 3_000_000);
}

// ── Absence is not zero ───────────────────────────────────────────────────────

#[test]
fn an_unlimited_cap_is_minus_one_and_not_a_cap_of_nothing() {
    // `deadline-only` is the ONLY case with `cap_micros: -1`, added upstream because
    // the rule was documented with no fixture behind it. Reading -1 as zero would
    // report an uncapped run that spent anything as infinitely overspent.
    let (events, _) = read_stream("deadline-only");
    let outcome = terminal_outcome(&events).unwrap();
    assert_eq!(outcome.cap_micros, -1);
    assert!(!outcome.has_spend_cap(), "-1 is the absence of a cap");
    assert!(
        outcome.total_micros > 0,
        "and the run did spend, which is what makes a zero reading dangerous"
    );

    // Every other case has a real cap, including `no-answer-spend`'s cap of 1
    // micro-unit — a cap that funds nothing, which is a cap, not an absence.
    for case in CASES.iter().filter(|c| **c != "deadline-only") {
        let (events, _) = read_stream(case);
        let outcome = terminal_outcome(&events).unwrap();
        assert!(outcome.has_spend_cap(), "{case}: has a real cap");
        assert_eq!(
            outcome.cap_micros,
            i64_at(&read_expected(case), "cap_micros"),
            "{case}: cap_micros"
        );
    }
}

#[test]
fn a_missing_provenance_object_means_declined_not_a_stability_of_zero() {
    // quarry has no in-band way to say "not measured" — `stability` is a
    // non-nullable number on both twins, and its `StabilityKnown` flag is
    // `json:"-"` — so it omits the whole object when the rate is unpublishable.
    // Three distinct cases produce that: a single run (no distribution exists), a
    // rate of 0 reached with unassessed nodes, and an unverifiable tree. Rendering
    // any of them as "stability 0%" reports a measured failure where quarry declined
    // to measure.
    //
    // **Every case in this corpus omits it**, and that is not an oversight: each is
    // one run, and P7 says one run is one sample, so no distribution exists to
    // publish. So this test pins the *declining* side thoroughly and the
    // publishing side not at all — a coverage gap in the corpus, not in the reader,
    // and one worth raising upstream. Asserted as a fact so that a re-vendored
    // corpus which DOES carry a provenance object fails here and gets a
    // deliberate look, rather than quietly widening what this test covers.
    for case in CASES {
        let (events, _) = read_stream(case);
        let artifact = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Artifact(a) => Some(a),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{case}: no artifact event"));
        let present = read_expected(case)["provenance_present"].as_bool().unwrap();
        assert_eq!(
            artifact.provenance.is_some(),
            present,
            "{case}: provenance presence"
        );
        assert!(
            !present,
            "{case}: a corpus case now publishes provenance — the stability-present \
             path is newly testable and this test should be extended to cover it, \
             rather than this assertion relaxed"
        );
        assert!(
            artifact.provenance.is_none(),
            "{case}: and our reader must report it as absent, never as a stability of 0"
        );
    }
}

#[test]
fn an_empty_bound_by_is_a_measurement_and_not_a_missing_field() {
    // quarry emits `bound_by: ""` rather than omitting it, because "no cap bound this
    // run" is a finding. Which cap bit is the difference between a useful remedy and
    // a useless one — raising the wrong cap buys nothing — so the empty case must be
    // distinguishable from "we do not know".
    let (events, _) = read_stream("complete");
    assert_eq!(terminal_outcome(&events).unwrap().bound_by, "");

    // And where a cap DID bite, it is named.
    let (events, _) = read_stream("time-truncated");
    assert_eq!(terminal_outcome(&events).unwrap().bound_by, "latency");
}

// ── Gaps and unfunded are different denominations ─────────────────────────────

#[test]
fn gaps_and_unfunded_are_never_summed() {
    // Only *time* produces a gap. Being priced out is planned degradation inside the
    // authority granted, disclosed before spend — not missing work. The two cases
    // below are the same failure to a host that added them, and opposite remedies to
    // a caller: `no-answer-time` needs more time, `no-answer-spend` needs money.
    let (time_events, _) = read_stream("no-answer-time");
    let time = terminal_outcome(&time_events).unwrap();
    assert_eq!((time.gaps, time.unfunded), (4, 0));
    assert_eq!(time.bound_by, "latency");

    let (spend_events, _) = read_stream("no-answer-spend");
    let spend = terminal_outcome(&spend_events).unwrap();
    assert_eq!((spend.gaps, spend.unfunded), (0, 1));

    assert_ne!(
        (time.gaps, time.unfunded),
        (spend.gaps, spend.unfunded),
        "these two must not be confusable"
    );
    assert_eq!(
        time.gaps + time.unfunded,
        spend.gaps + spend.unfunded + 3,
        "a host that summed them would see 4 and 1 — different numbers, but both \
         read as 'incomplete nodes', which is the mislabelling that sends a caller \
         to the wrong remedy"
    );
}

#[test]
fn cap_bound_degradation_is_a_success_and_names_no_denomination() {
    // `live-partition` is a real Bedrock run with 5 unfunded nodes, and it exits 0
    // with `outcome: cap-bound-degradation` and an EMPTY `bound_by`. quarry does not
    // report spend as having "bound" a run it planned to fit: the cap is the
    // contract, so being priced out of a branch is the plan working.
    //
    // This is why a framed run never yields a spend *truncation* — a distinction that
    // is invisible unless a real degraded capture exists, which is exactly why this
    // case could not be generated under `--fake`.
    let (events, _) = read_stream("live-partition");
    let outcome = terminal_outcome(&events).unwrap();
    assert_eq!(outcome.outcome, "cap-bound-degradation");
    assert_eq!(outcome.unfunded, 5);
    assert_eq!(outcome.gaps, 0, "no time was involved");
    assert_eq!(
        outcome.bound_by, "",
        "degradation inside authority names no denomination"
    );
    assert_eq!(i64_at(&read_expected("live-partition"), "exit_code"), 0);
}

#[test]
fn a_time_truncated_run_still_carries_the_answer_it_reached() {
    // The result quarry promises to return: a partial answer with its gaps named. An
    // earlier classifier called exit 3 a crash and threw this away.
    let (events, _) = read_stream("time-truncated");
    let outcome = terminal_outcome(&events).unwrap();
    assert_eq!(outcome.outcome, "time-truncated");
    assert_eq!(outcome.gaps, 2);
    assert_eq!(outcome.unfunded, 0, "time made these, not money");

    let answer = events.iter().find_map(|e| match e {
        RunEvent::Answer(a) => Some(a),
        _ => None,
    });
    let answer = answer.expect("a time-truncated run can still have answered");
    assert!(
        !answer.text.trim().is_empty(),
        "and the partial answer is worth showing"
    );
    assert_eq!(i64_at(&read_expected("time-truncated"), "exit_code"), 3);
}

#[test]
fn a_no_answer_run_has_an_empty_receipt_rather_than_none() {
    // `"rows":[]`, never `null`. The distinction matters to a host that would render
    // "no receipt" where the truth is "a receipt for nothing" — the run happened, and
    // it is still citable.
    for case in ["no-answer-time", "no-answer-spend"] {
        let (events, _) = read_stream(case);
        let receipt = events
            .iter()
            .find_map(|e| match e {
                RunEvent::Receipt(r) => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{case}: the receipt event is still emitted"));
        assert!(receipt.rows.is_empty(), "{case}: nothing was spent on");
        assert_eq!(receipt.total_micro_usd(), 0);
        assert_eq!(terminal_outcome(&events).unwrap().total_micros, 0);
        assert_eq!(i64_at(&read_expected(case), "exit_code"), 4);
    }
}

// ── Text: runes, not bytes ────────────────────────────────────────────────────

#[test]
fn answer_lengths_are_counted_in_runes_not_bytes() {
    // Every length in the corpus is a rune count. On `unicode-long` the two differ
    // by a wide margin, so a byte count silently passes on ASCII cases and fails
    // here — which is what makes this case worth having.
    for case in CASES {
        let (events, _) = read_stream(case);
        let expected = read_expected(case);
        let answer = events.iter().find_map(|e| match e {
            RunEvent::Answer(a) => Some(a),
            _ => None,
        });
        assert_eq!(
            answer.is_some(),
            expected["answer_present"].as_bool().unwrap(),
            "{case}: answer presence"
        );
        if let Some(answer) = answer {
            let runes = answer.text.chars().count() as i64;
            assert_eq!(
                runes,
                i64_at(&expected, "answer_runes"),
                "{case}: answer runes"
            );
        }
    }

    // The guard: on the unicode case a byte count is not the rune count, so this test
    // is genuinely discriminating rather than incidentally true.
    let (events, _) = read_stream("unicode-long");
    let text = events
        .iter()
        .find_map(|e| match e {
            RunEvent::Answer(a) => Some(a.text.clone()),
            _ => None,
        })
        .unwrap();
    assert_ne!(
        text.len(),
        text.chars().count(),
        "if these were equal the corpus would no longer be exercising multi-byte text"
    );
}

#[test]
fn receipt_labels_preserve_the_bytes_quarry_wrote() {
    // Two things at once. HTML escaping is OFF, so `&` and `<` stay literal — Go's
    // default encoder would have written `&`, and a host comparing against a
    // record hash would then disagree with quarry about its own bytes.
    //
    // And truncation is at 60 RUNES: `unicode-long` carries both shapes that separate
    // implementations — labels over the limit that must be cut, and one of 47 runes /
    // 139 bytes that must NOT be. A byte-based limit cuts that one and produces a
    // label quarry never wrote.
    let (events, _) = read_stream("unicode-long");
    let expected = read_expected("unicode-long");
    let receipt = events
        .iter()
        .find_map(|e| match e {
            RunEvent::Receipt(r) => Some(r),
            _ => None,
        })
        .unwrap();

    let want = expected["receipt"]["rows"].as_array().unwrap();
    assert_eq!(receipt.rows.len(), want.len());
    for (got, want) in receipt.rows.iter().zip(want) {
        assert_eq!(
            got.label,
            want["label"].as_str().unwrap(),
            "a label must survive byte-for-byte"
        );
        assert_eq!(got.cost_micro_usd(), i64_at(want, "cost_micros"));
    }

    // The label that is long in bytes but short in runes, unabridged.
    let long_in_bytes = receipt
        .rows
        .iter()
        .find(|r| r.label.chars().count() <= 60 && r.label.len() > 60)
        .expect("the 47-rune / 139-byte label is what distinguishes runes from bytes");
    assert!(
        !long_in_bytes.label.contains('…'),
        "a rune-short label must not be truncated, however long it is in bytes"
    );

    // And nothing was escaped on the way through.
    let (uni_events, _) = read_stream("unicode");
    let uni_receipt = uni_events
        .iter()
        .find_map(|e| match e {
            RunEvent::Receipt(r) => Some(r),
            _ => None,
        })
        .unwrap();
    for row in &uni_receipt.rows {
        assert!(
            !row.label.contains("\\u00"),
            "an escaped sequence reached us as literal text: {}",
            row.label
        );
    }
}

// ── A severed stream ──────────────────────────────────────────────────────────

#[test]
fn a_stream_cut_mid_line_is_incomplete_not_merely_bad() {
    // `crashed` is cut INSIDE the artifact event's JSON. Two things must hold: the
    // partial line is skipped rather than fatal, because the events before it were
    // paid for and are still worth reporting; and the missing terminal event is what
    // says the run was killed. NDJSON yields complete lines whether or not the
    // producer finished, so this absence is the only in-band signal there is.
    let (events, stats) = read_stream("crashed");
    assert!(
        stats.events > 0,
        "the events before the cut survive — they were paid for"
    );
    assert_eq!(
        stats.bad_lines.len(),
        1,
        "exactly the severed line is skipped"
    );
    assert!(
        terminal_outcome(&events).is_none(),
        "no terminal event: this run was killed, and must never read as complete"
    );
    assert_eq!(
        stream_version(&events),
        Some(SUPPORTED_STREAM_VERSION),
        "the frame opened, which is what makes the missing closer meaningful rather \
         than merely absent"
    );
}
