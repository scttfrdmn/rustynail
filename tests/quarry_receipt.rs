//! Receipts rendered from quarry's own captured runs, and delivered through the
//! gateway's real outbound path.
//!
//! # Why the corpus and not hand-written outcomes
//!
//! `src/quarry/receipt.rs`'s unit tests build `RunOutcome`s by hand, which proves the
//! renderer is self-consistent and nothing more. These read the bytes **quarry
//! produced** — the frozen corpus under `tests/fixtures/quarry/runevents/` — and
//! assert what a sender would actually see.
//!
//! That distinction has already earned its keep twice in this integration. The
//! `--events-json` flag was missing from argv for four commits while every test
//! stayed green, because the fake emitted its canned NDJSON regardless of argv. And
//! the record type could not deserialize a single real quarry record, because Go
//! writes a nil slice as `null` and every hand-written fixture spelled its empty
//! lists `[]`. Neither was findable from fixtures we authored.
//!
//! # The claims under test
//!
//! 1. **The receipt is never suppressed.** No config flag, no channel exemption, no
//!    length threshold. A footer too long for a platform is chunked.
//! 2. **Gaps and unfunded are separate sections** with opposite remedies, and their
//!    sum appears nowhere.
//! 3. **Absence is not zero**: a missing provenance object is "not measured", not 0%;
//!    a `cap_micros` of `-1` is no limit, not a limit of nothing; an empty `bound_by`
//!    is a measurement and is reported as one.
//! 4. **Spend is quarry's integer**, not a sum of the float rows.
//! 5. Delivery goes through the **real** formatter and chunker, on the tightest
//!    platform limit of the five (Teams, 1024).

use rustynail::gateway::chunker::MessageChunker;
use rustynail::gateway::dashboard::MessageStats;
use rustynail::gateway::formatter::ResponseFormatter;
use rustynail::quarry::receipt::Receipt;
use rustynail::quarry::{
    parse_line, terminal_outcome, RunEvent, RunOutcome, RunRecordSummary, StreamStats, Termination,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

// ── Corpus loading ────────────────────────────────────────────────────────────

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/quarry/runevents")
}

/// Every case that has both a stream and a record — i.e. every case captured from a
/// real run. `unknown-kind` is synthetic and `crashed` is severed mid-line; both are
/// exercised separately below.
const CASES: &[&str] = &[
    "complete",
    "deadline-only",
    "live-partition",
    "no-answer-spend",
    "no-answer-time",
    "time-truncated",
    "unicode",
    "unicode-long",
];

fn read_events(case: &str) -> (Vec<RunEvent>, StreamStats) {
    let path = corpus_dir().join(format!("{case}.ndjson"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{case}: {e}"));
    let mut events = Vec::new();
    let mut stats = StreamStats::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        stats.lines += 1;
        match parse_line(line) {
            Ok(e) => {
                stats.events += 1;
                events.push(e);
            }
            Err(e) => stats.bad_lines.push((i + 1, e)),
        }
    }
    (events, stats)
}

fn read_record(case: &str) -> Option<RunRecordSummary> {
    let path = corpus_dir().join(format!("{case}.json"));
    let bytes = std::fs::read(&path).ok()?;
    Some(
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|e| panic!("{case}: quarry's own record does not parse: {e}")),
    )
}

fn read_expected(case: &str) -> serde_json::Value {
    let path = corpus_dir().join(format!("{case}.expected.json"));
    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()
}

/// Assemble a `RunOutcome` from a corpus case, classified from quarry's own terminal
/// event and exit code rather than from anything invented here.
fn outcome(case: &str) -> RunOutcome {
    let (events, stats) = read_events(case);
    let expected = read_expected(case);
    let code = expected["exit_code"].as_i64().unwrap() as i32;
    let has_answer = events.iter().any(|e| matches!(e, RunEvent::Answer(_)));

    // The corpus states the exit code, so the termination is derived from quarry's
    // own two statements — the code and the terminal event — and not guessed.
    let termination = match terminal_outcome(&events) {
        Some(t) => match t.outcome.as_str() {
            "complete" | "cap-bound-degradation" => Termination::Completed,
            "time-truncated" => Termination::Truncated {
                bound_by: Some(if t.bound_by.is_empty() {
                    "latency".to_string()
                } else {
                    t.bound_by.clone()
                }),
            },
            "no-answer" => Termination::NoAnswer,
            _ if has_answer && code == 0 => Termination::Completed,
            _ => Termination::Crashed { exit_code: code },
        },
        None => Termination::StreamIncomplete {
            events_read: stats.events,
        },
    };

    RunOutcome {
        run_id: format!("gw-{case}"),
        termination,
        events,
        stats,
        stderr: String::new(),
        run_dir: PathBuf::from("/tmp/rn-quarry").join(case),
        record: read_record(case),
        duration: Duration::from_secs(4),
    }
}

// ── The corpus is actually here ───────────────────────────────────────────────

#[test]
fn the_corpus_is_vendored_so_nothing_below_passes_vacuously() {
    for case in CASES {
        for suffix in ["ndjson", "json", "expected.json"] {
            let f = corpus_dir().join(format!("{case}.{suffix}"));
            assert!(f.is_file(), "missing {}", f.display());
        }
    }
}

// ── 1. The receipt is never suppressed ────────────────────────────────────────

/// Every captured run gets a receipt with spend, a trust statement, and a citation.
///
/// The blanket claim, over every real run quarry has been observed to produce —
/// including the two that answered nothing and the one whose budget funded a single
/// node.
#[test]
fn every_captured_run_gets_a_receipt_with_spend_trust_and_a_citation() {
    for case in CASES {
        let text = Receipt::from_outcome(&outcome(case)).render();
        assert!(text.contains("**Receipt**"), "{case}: no receipt\n{text}");
        assert!(text.contains("• Spend:"), "{case}: no spend line\n{text}");
        assert!(
            text.contains("How much to trust it"),
            "{case}: no trust section\n{text}"
        );
        assert!(
            text.contains("Full record"),
            "{case}: nothing citable — P8 needs a path\n{text}"
        );
    }
}

/// A severed stream still produces a receipt, and says the run was cut off.
///
/// `crashed` is cut mid-line inside the artifact event: no terminal event, so no
/// total. The receipt must report the spend as *unknown* — the money went out, so
/// rendering `$0.0000` is the one reading guaranteed to be false.
#[test]
fn a_severed_stream_still_gets_a_receipt_and_calls_its_spend_unknown() {
    let (events, stats) = read_events("crashed");
    let o = RunOutcome {
        run_id: "gw-crashed".to_string(),
        termination: Termination::StreamIncomplete {
            events_read: stats.events,
        },
        events,
        stats,
        stderr: String::new(),
        run_dir: PathBuf::from("/tmp/rn-quarry/crashed"),
        record: None,
        duration: Duration::from_secs(2),
    };
    let r = Receipt::from_outcome(&o);
    assert_eq!(r.spend_micro_usd, None);

    let text = r.render();
    assert!(text.contains("**Receipt**"), "{text}");
    assert!(text.contains("not reported"), "{text}");
    assert!(text.contains("unknown rather than zero"), "{text}");
    assert!(!text.contains("Spend: $0.0000"), "{text}");
    // And the partial answer it did produce is still delivered — it was paid for.
    assert!(r.answer.is_some(), "the answer before the cut was paid for");
}

// ── 2. Gaps and unfunded stay separate ────────────────────────────────────────

/// Each captured run's two counts are reported separately, and never summed.
///
/// Driven off quarry's own expectations file, so the numbers are not restated here.
/// `live-partition` has 5 unfunded and 0 gaps; `no-answer-time` has 4 gaps and 0
/// unfunded — between them they cover both denominations in isolation.
#[test]
fn the_two_denominations_are_reported_separately_across_the_whole_corpus() {
    for case in CASES {
        let expected = read_expected(case);
        let gaps = expected["gaps"].as_u64().unwrap();
        let unfunded = expected["unfunded"].as_u64().unwrap();
        let r = Receipt::from_outcome(&outcome(case));
        assert_eq!(r.gaps, gaps, "{case}: gap count");
        assert_eq!(r.unfunded, unfunded, "{case}: unfunded count");

        let text = r.render();
        assert_eq!(
            text.contains("Missing because time ran out"),
            gaps > 0,
            "{case}: time section present={} but gaps={gaps}\n{text}",
            text.contains("Missing because time ran out")
        );
        assert_eq!(
            text.contains("budget did not cover it"),
            unfunded > 0,
            "{case}: budget section present={} but unfunded={unfunded}\n{text}",
            text.contains("budget did not cover it")
        );

        // The sum must not appear as a node count anywhere. Only checked when it is
        // distinguishable from both parts, otherwise the assertion is vacuous.
        let sum = gaps + unfunded;
        if sum > 0 && sum != gaps && sum != unfunded {
            assert!(
                !text.contains(&format!("({sum} nodes)")),
                "{case}: gaps and unfunded were summed into {sum}\n{text}"
            );
        }
    }
}

/// `live-partition`: 5 nodes priced out, 0 gapped, and exit 0.
///
/// The case `--fake` cannot construct, and the one that catches a host describing
/// budget degradation as a failure or as time pressure. quarry's own note on it:
/// *"Exit 0: degradation is not a failure."*
#[test]
fn cap_bound_degradation_is_reported_as_planned_not_as_time_pressure_or_failure() {
    let text = Receipt::from_outcome(&outcome("live-partition")).render();
    assert!(text.contains("(5 nodes)"), "{text}");
    assert!(
        text.contains("planned degradation, not lost work"),
        "{text}"
    );
    assert!(!text.contains("time ran out"), "{text}");
    // `bound_by` is empty on this run, which is a measurement quarry emits on
    // purpose, so it must be stated rather than left out.
    assert!(text.contains("No limit was reached."), "{text}");
}

/// `no-answer-time`: 4 gaps, and it cost nothing.
///
/// The deadline bit before any spend, so this run has gaps *and* a zero total — the
/// pair that catches a host inferring either fact from the other.
#[test]
fn a_run_that_gapped_before_spending_reports_both_facts() {
    let r = Receipt::from_outcome(&outcome("no-answer-time"));
    assert_eq!(r.spend_micro_usd, Some(0));
    assert_eq!(r.gaps, 4);

    let text = r.render();
    assert!(text.contains("**No answer.**"), "{text}");
    assert!(text.contains("time ran out"), "{text}");
    assert!(text.contains("More time would recover these"), "{text}");
    assert!(!text.contains("budget did not cover it"), "{text}");
}

/// `no-answer-spend`: a cap of one micro-unit, one unfunded node, zero gaps.
///
/// The clock was never involved, so a footer mentioning time here would send the
/// sender to raise a cap that would buy nothing.
#[test]
fn a_run_priced_out_at_the_floor_never_mentions_time() {
    let r = Receipt::from_outcome(&outcome("no-answer-spend"));
    assert_eq!(r.gaps, 0);
    assert_eq!(r.unfunded, 1);
    assert_eq!(r.cap_micro_usd, Some(1));

    let text = r.render();
    assert!(!text.contains("time ran out"), "{text}");
    assert!(text.contains("higher spend limit"), "{text}");
    // A cap of one micro-unit is a real cap and renders as a figure.
    assert!(text.contains("of $0.0000"), "{text}");
}

/// `time-truncated`: the denomination is named, with the remedy that matches it.
#[test]
fn a_time_truncated_run_names_latency_and_offers_more_time() {
    let text = Receipt::from_outcome(&outcome("time-truncated")).render();
    assert!(text.contains("Stopped by: **latency**"), "{text}");
    assert!(text.contains("more time would let it go further"), "{text}");
    assert!(
        !text.contains("a higher spend limit would let it go further"),
        "the wrong cap was recommended\n{text}"
    );
}

// ── 3. Absence is not zero ────────────────────────────────────────────────────

/// No corpus case publishes a provenance object, and none may render as 0% stable.
///
/// All eight carry `provenance_present: false`, because every one is a single run and
/// a stable-claim fraction is not defined for n=1 (P7). Rendering that as 0% would
/// convert quarry's silence into a finding of total irreproducibility.
#[test]
fn no_captured_run_publishes_stability_and_none_renders_as_zero_percent() {
    for case in CASES {
        let expected = read_expected(case);
        assert_eq!(
            expected["provenance_present"].as_bool(),
            Some(false),
            "{case}: the corpus now has a published provenance — the stability-present \
             path is finally testable, so add a case for it here"
        );
        let text = Receipt::from_outcome(&outcome(case)).render();
        assert!(text.contains("not measured"), "{case}\n{text}");
        assert!(text.contains("P7"), "{case}: the reason is missing\n{text}");
        assert!(
            !text.contains("0% of claims"),
            "{case}: silence rendered as a finding\n{text}"
        );
    }
}

/// `deadline-only`: `cap_micros` of `-1` is no limit, not a limit of nothing.
#[test]
fn an_unlimited_spend_cap_renders_as_no_limit() {
    let r = Receipt::from_outcome(&outcome("deadline-only"));
    assert!(r.cap_unlimited);

    let text = r.render();
    assert!(text.contains("of no limit"), "{text}");
    assert!(
        !text.contains("of $0.0000"),
        "an unlimited cap was rendered as a cap funding nothing\n{text}"
    );
}

/// No corpus record sets a real deadline, and none may render Go's year-1 zero time.
#[test]
fn gos_zero_deadline_never_reaches_the_footer() {
    for case in CASES {
        let text = Receipt::from_outcome(&outcome(case)).render();
        assert!(
            !text.contains("0001-01-01"),
            "{case}: Go's zero time rendered as a deadline\n{text}"
        );
    }
}

// ── 4. Spend is quarry's integer ──────────────────────────────────────────────

/// Each run's spend matches quarry's `total_micros`, to the unit.
///
/// `live-partition` carries `float_sum_equals_total: false`, so it fails a host that
/// sums the float rows — which is why the assertion is against the expectations file
/// rather than against a re-derivation.
#[test]
fn spend_matches_quarrys_own_integer_total_on_every_case() {
    for case in CASES {
        let expected = read_expected(case);
        let total = expected["total_micros"].as_i64().unwrap();
        let r = Receipt::from_outcome(&outcome(case));
        assert_eq!(r.spend_micro_usd, Some(total), "{case}: total spend");
    }
}

/// `live-partition`: the unattributable half of the spend is disclosed, not hidden.
///
/// 42042 of 80437 micro-units belong to no model, because quarry's reduce nodes spend
/// without recording a `ModelVersion` (quarry#20). The figure is over half the run, so
/// a footer that quietly reconciled it would be misstating most of the bill.
#[test]
fn the_unattributable_model_spend_is_disclosed_with_its_upstream_cause() {
    let expected = read_expected("live-partition");
    let r = Receipt::from_outcome(&outcome("live-partition"));

    assert_eq!(
        r.model_spend_micro_usd,
        expected["model_spend_micros"].as_i64()
    );
    assert_eq!(
        r.model_spend_residual_micro_usd(),
        expected["model_spend_unexplained_micros"].as_i64()
    );

    let text = r.render();
    assert!(text.contains("quarry#20"), "{text}");
    assert!(text.contains("The total above is still exact."), "{text}");
    // And quarry's total is printed unadjusted: 80437 micro-units.
    assert!(text.contains("$0.0804"), "{text}");
}

/// Every case's itemised rows reconcile against the stated total, in integers.
///
/// quarry's rule is that a receipt which does not add up is worse than no receipt, so
/// the corpus asserting `reconciles: true` everywhere means the warning line must
/// appear nowhere.
#[test]
fn no_captured_receipt_fails_to_reconcile() {
    for case in CASES {
        let expected = read_expected(case);
        let reconciles = expected["receipt"]["reconciles"].as_bool().unwrap_or(true);
        let r = Receipt::from_outcome(&outcome(case));
        assert_eq!(r.rows_reconcile, reconciles, "{case}: reconciliation");
        assert!(
            !r.render().contains("do not sum to its stated total"),
            "{case}: a reconciling receipt was flagged as broken"
        );
    }
}

// ── 5. Delivery through the real outbound path ────────────────────────────────

/// Teams' 1024-byte limit is the tightest of the five defaults, and the receipt
/// survives it whole.
///
/// The point is not that chunking works — that is `chunker`'s own test — but that the
/// footer is **never dropped to fit**. `live-partition`'s answer is 6798 runes, so on
/// Teams the reply is many chunks and the receipt is the last thing in it. A host that
/// truncated instead would deliver the answer and silently discard what it cost.
#[test]
fn on_teams_the_answer_is_chunked_and_the_receipt_arrives_intact() {
    let formatter = ResponseFormatter::new(true);
    let chunker = MessageChunker::new(HashMap::new());

    let reply = Receipt::from_outcome(&outcome("live-partition")).render();
    let formatted = formatter.format(&reply, "teams-general");
    let chunks = chunker.chunk("teams-general", &formatted);

    assert!(
        chunks.len() > 1,
        "a 6798-rune answer must not fit in 1024 bytes — this test is not exercising \
         chunking"
    );
    for (i, c) in chunks.iter().enumerate() {
        assert!(
            c.len() <= 1024,
            "chunk {i} is {} bytes, over Teams' limit",
            c.len()
        );
    }

    // Reassembled, the receipt's load-bearing figures are all present. Chunking may
    // split the footer across messages; it may not lose any of it.
    let reassembled: String = chunks.join("");
    assert!(
        reassembled.contains("**Receipt**"),
        "the receipt was dropped"
    );
    assert!(reassembled.contains("$0.0804"), "the total was dropped");
    assert!(
        reassembled.contains("(5 nodes)"),
        "the unfunded count was dropped"
    );
    assert!(
        reassembled.contains("not measured"),
        "the trust line was dropped"
    );
    assert!(
        reassembled.contains("quarry#20"),
        "the residual was dropped"
    );
    assert!(
        reassembled.contains("Full record"),
        "the citation was dropped"
    );
}

/// The tightest limit against the longest run, on every platform default.
///
/// Five platforms, one 6798-rune answer plus a footer. No chunk may exceed its limit
/// and the receipt must survive on all of them — a per-platform exemption is exactly
/// the kind of quiet omission the no-suppression rule exists to forbid.
#[test]
fn the_receipt_survives_every_platform_limit() {
    let formatter = ResponseFormatter::new(true);
    let chunker = MessageChunker::new(HashMap::new());
    let reply = Receipt::from_outcome(&outcome("live-partition")).render();

    for (channel, limit) in [
        ("discord-general", 2000),
        ("slack-general", 4000),
        ("teams-general", 1024),
        ("telegram-general", 4096),
        ("whatsapp-general", 4096),
    ] {
        let formatted = formatter.format(&reply, channel);
        let chunks = chunker.chunk(channel, &formatted);
        for (i, c) in chunks.iter().enumerate() {
            assert!(
                c.len() <= limit,
                "{channel}: chunk {i} is {} bytes, limit {limit}",
                c.len()
            );
        }
        // Markup differs per platform and that is the formatter working, not the
        // receipt being damaged: Slack, Telegram and WhatsApp each rewrite `**bold**`
        // into their own single-asterisk form, and Telegram MarkdownV2 additionally
        // backslash-escapes `.`, `-`, `#` and more — so a literal `$0.0804` becomes
        // `$0\.0804` there. Both are correct. Comparing the *content* with the markup
        // normalised away is what actually tests delivery.
        let delivered: String = chunks
            .join(" ")
            .chars()
            .filter(|c| *c != '\\' && *c != '*')
            .collect();
        assert!(
            delivered.contains("Receipt"),
            "{channel}: the receipt heading did not survive delivery"
        );
        assert!(
            delivered.contains("$0.0804"),
            "{channel}: the total did not survive delivery"
        );
        assert!(
            delivered.contains("quarry#20"),
            "{channel}: the residual disclosure did not survive delivery"
        );
        assert!(
            delivered.contains("not measured"),
            "{channel}: the trust line did not survive delivery"
        );
    }
}

/// A multi-byte answer is chunked on character boundaries, so no chunk is invalid
/// UTF-8 and no glyph is split.
///
/// `unicode` and `unicode-long` are in the corpus because quarry truncates receipt
/// labels by **rune** while platform limits are in **bytes** — the two units disagree,
/// and a byte-indexed cut through a 3-byte character produces a chunk a platform will
/// reject outright.
///
/// The invariant is *no character is broken*, not byte-exact reassembly: the chunker
/// breaks on whitespace and consumes the whitespace it broke on, which is deliberate
/// and correct for chat delivery. So this asserts on the non-whitespace content.
///
/// The limit is configured tight rather than left at Teams' 1024, because both these
/// replies fit inside 1024 bytes — at the platform default nothing would split and the
/// test would pass without ever exercising a boundary. 40 bytes forces a break every
/// few characters, including inside the CJK runs where every character is 3 bytes and
/// no split can land on a boundary by luck.
#[test]
fn a_multibyte_answer_is_never_cut_through_a_character() {
    let formatter = ResponseFormatter::new(true);
    let mut limits = HashMap::new();
    limits.insert("teams".to_string(), 40);
    let chunker = MessageChunker::new(limits);

    for case in ["unicode", "unicode-long"] {
        let reply = Receipt::from_outcome(&outcome(case)).render();
        let formatted = formatter.format(&reply, "teams-general");
        let chunks = chunker.chunk("teams-general", &formatted);
        assert!(
            chunks.len() > 1,
            "{case}: nothing was chunked, so this proves nothing about splitting"
        );
        for c in &chunks {
            assert!(c.len() <= 40, "{case}: chunk over limit: {} bytes", c.len());
            assert!(!c.contains('\u{FFFD}'), "{case}: a character was mangled");
        }
        // Every character survives, in order. A cut through a multi-byte sequence
        // could not produce two valid `String`s without losing or replacing bytes, so
        // an equal non-whitespace character sequence is the real proof.
        let strip = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
        assert_eq!(
            strip(&chunks.concat()),
            strip(&formatted),
            "{case}: content was lost or mangled in chunking"
        );
    }
}

/// An unknown event kind does not disturb the receipt.
///
/// `unknown-kind` places a `quarry_future_kind` line between the answer and the
/// receipt. quarry's union is open by design — "adding an event kind here cannot break
/// an existing consumer" — so the footer must render from the kinds it knows and say
/// nothing about the one it does not.
#[test]
fn an_unknown_event_kind_does_not_disturb_the_receipt() {
    let (events, stats) = read_events("unknown-kind");
    let expected = read_expected("unknown-kind");
    let o = RunOutcome {
        run_id: "gw-unknown-kind".to_string(),
        termination: Termination::Completed,
        events,
        stats,
        stderr: String::new(),
        run_dir: PathBuf::from("/tmp/rn-quarry/unknown-kind"),
        record: None,
        duration: Duration::from_secs(1),
    };
    let r = Receipt::from_outcome(&o);
    assert_eq!(
        r.spend_micro_usd,
        expected["total_micros"].as_i64(),
        "an unknown kind changed the total"
    );

    let text = r.render();
    assert!(text.contains("**Receipt**"), "{text}");
    assert!(
        !text.contains("quarry_future_kind"),
        "an unrecognised kind leaked into a sender-facing reply\n{text}"
    );
}

// ── The dashboard decomposition tree ──────────────────────────────────────────

/// The tree published to the dashboard is quarry's, node for node.
///
/// `live-partition` is a real 30-node Bedrock run. Every node in the event must come
/// from the record, and the two per-node classifications must match quarry's own
/// `Gaps()` and `Unfunded()` counts — not be re-derived from anything invented here.
/// The issue's constraint is explicit: no locally-invented event type presented as the
/// shared contract.
#[tokio::test]
async fn the_dashboard_tree_is_quarrys_own_nodes_and_edges() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();
    let o = outcome("live-partition");
    let record = o.record.as_ref().unwrap();

    stats.record_quarry_run(&o);
    let event = rx.try_recv().expect("no dashboard event published");
    let json = serde_json::to_value(&event).unwrap();

    assert_eq!(json["type"], "quarry_run");
    assert_eq!(json["outcome"], "cap-bound-degradation");
    assert_eq!(json["record_id"], record.run_id.as_str());
    assert_eq!(json["spend_micro_usd"], 80437);
    assert_eq!(json["cap_micro_usd"], 250_000);

    let nodes = json["nodes"].as_array().unwrap();
    assert_eq!(
        nodes.len(),
        record.outcomes.len(),
        "the tree must carry every node quarry recorded, no more and no fewer"
    );

    // Edges come from the record's own `Children`, so the total edge count must match.
    let record_edges: usize = record.outcomes.iter().map(|n| n.children.len()).sum();
    let event_edges: usize = nodes
        .iter()
        .map(|n| n["children"].as_array().unwrap().len())
        .sum();
    assert_eq!(event_edges, record_edges, "edges were invented or dropped");

    // And the per-node flags agree with quarry's two classifications.
    let gaps = nodes.iter().filter(|n| n["gap"] == true).count();
    let unfunded = nodes.iter().filter(|n| n["unfunded"] == true).count();
    assert_eq!(gaps, record.gaps().len());
    assert_eq!(unfunded, record.unfunded().len());
    assert_eq!(unfunded, 5, "live-partition has 5 nodes the cap priced out");
}

/// Gaps and unfunded stay separate integers on the wire, so the front end cannot
/// accidentally render one number.
#[tokio::test]
async fn the_dashboard_event_keeps_the_two_denominations_apart() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();

    for case in CASES {
        let expected = read_expected(case);
        stats.record_quarry_run(&outcome(case));
        let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
        assert_eq!(json["gaps"], expected["gaps"], "{case}: gaps");
        assert_eq!(json["unfunded"], expected["unfunded"], "{case}: unfunded");
        // Separate keys, so nothing downstream can sum them without doing so
        // deliberately.
        assert!(json.get("gaps").is_some() && json.get("unfunded").is_some());
        assert!(
            json.get("incomplete").is_none(),
            "{case}: a merged count appeared on the wire"
        );
    }
}

/// Stability is two fields, so "not measured" cannot arrive as a zero.
///
/// A single nullable float would work, but a `0.0` and a `null` are one typo apart in
/// the front end. The explicit boolean makes the distinction impossible to lose: every
/// corpus case is a single run, so every one must report `stability_measured: false`
/// with no rate at all.
#[tokio::test]
async fn the_dashboard_event_distinguishes_unmeasured_stability_from_zero() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();

    for case in CASES {
        stats.record_quarry_run(&outcome(case));
        let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            json["stability_measured"], false,
            "{case}: every corpus run is a single sample (P7)"
        );
        assert!(
            json["stability"].is_null(),
            "{case}: an unmeasured rate must be absent, not 0.0"
        );
    }
}

/// A run with no record still reaches the dashboard, with an empty tree.
///
/// A killed run is the one an operator most wants to see, so dropping it for lack of
/// node detail would hide exactly the wrong thing. The outcome, spend and cap are on
/// the event stream and survive independently of the record.
#[tokio::test]
async fn a_run_with_no_record_still_reaches_the_dashboard() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();

    let mut o = outcome("complete");
    o.record = None;
    stats.record_quarry_run(&o);

    let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    assert_eq!(json["type"], "quarry_run");
    assert_eq!(json["outcome"], "complete");
    // Still from the stream: the record was only ever corroboration.
    assert_eq!(json["spend_micro_usd"], 218);
    assert_eq!(json["nodes"].as_array().unwrap().len(), 0);
    // With no record there is no cap to state, and it must be absent rather than 0 —
    // a cap of zero funds nothing, which is a different claim from "unknown".
    assert!(json["cap_micro_usd"].is_null());
}

/// `deadline-only`'s unlimited cap crosses the wire as `-1`, not as null or 0.
#[tokio::test]
async fn an_unlimited_cap_crosses_the_wire_as_the_sentinel() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();
    stats.record_quarry_run(&outcome("deadline-only"));
    let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    assert_eq!(
        json["cap_micro_usd"], -1,
        "unlimited must stay distinguishable from both unset and a cap of nothing"
    );
}

/// Node problems are truncated by character, not by byte.
///
/// A byte-index truncation panics when it lands inside a multi-byte sequence, and
/// quarry's problems routinely carry `—` and non-Latin scripts. `unicode-long` exists
/// for exactly this class of bug.
#[tokio::test]
async fn node_problems_are_truncated_by_character_not_byte() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();
    stats.record_quarry_run(&outcome("unicode-long"));
    let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();

    for node in json["nodes"].as_array().unwrap() {
        let problem = node["problem"].as_str().unwrap();
        assert!(
            problem.chars().count() <= 160,
            "problem is {} characters, over the 160 limit",
            problem.chars().count()
        );
        assert!(
            !problem.contains('\u{FFFD}'),
            "a character was cut in half: {problem}"
        );
    }
}

/// The verifier's three states survive as three states.
///
/// `None` is "no verifier assessed", which is not `Some(false)`. Collapsing them would
/// badge unexamined work as refuted, and `live-partition` has 12 unassessed nodes for
/// this to be visible on.
#[tokio::test]
async fn the_verified_field_keeps_unassessed_distinct_from_refuted() {
    let stats = MessageStats::new();
    let mut rx = stats.subscribe();
    let o = outcome("live-partition");
    let record = o.record.as_ref().unwrap();
    stats.record_quarry_run(&o);

    let json = serde_json::to_value(rx.try_recv().unwrap()).unwrap();
    let nodes = json["nodes"].as_array().unwrap();

    let unassessed = nodes.iter().filter(|n| n["verified"].is_null()).count();
    let refuted = nodes.iter().filter(|n| n["verified"] == false).count();
    let expected_unassessed = record
        .outcomes
        .iter()
        .filter(|n| n.verified.is_none())
        .count();

    assert_eq!(unassessed, expected_unassessed);
    assert!(
        unassessed > 0,
        "live-partition has unassessed nodes; without any this proves nothing"
    );
    assert_eq!(
        refuted, 0,
        "no node on this run was refuted, so any refuted badge is a collapsed null"
    );
}

/// The front end handles every field this event carries.
///
/// Cheap and worth it: the union and the page are in different languages with no shared
/// schema, so a field added to the Rust enum and never rendered is invisible until
/// somebody notices the dashboard is missing information. This catches the omission at
/// build time instead.
#[test]
fn the_dashboard_page_renders_every_field_of_the_quarry_event() {
    let html = include_str!("../src/gateway/dashboard.html");
    assert!(
        html.contains("'quarry_run'"),
        "the page does not dispatch on the quarry_run event at all"
    );
    for field in [
        "outcome",
        "bound_by",
        "spend_micro_usd",
        "cap_micro_usd",
        "gaps",
        "unfunded",
        "stability_measured",
        "stability",
        "nodes",
        "record_id",
    ] {
        assert!(
            html.contains(&format!("data.{field}")) || html.contains(&format!("node.{field}")),
            "the dashboard page never reads `{field}`"
        );
    }
    for field in ["gap", "cache_hit", "verified", "children", "cost_micro_usd"] {
        assert!(
            html.contains(&format!("node.{field}")),
            "the dashboard page never reads node.{field}"
        );
    }
}

/// Node problems reach the page escaped.
///
/// A quarry node problem is sender-supplied text that has round-tripped through an
/// LLM, and the page writes it into `innerHTML`. Without escaping, a chat message could
/// put a script tag into an operator's dashboard.
#[test]
fn the_dashboard_page_escapes_node_text_before_interpolating_it() {
    let html = include_str!("../src/gateway/dashboard.html");
    assert!(
        html.contains("function esc("),
        "no escaping helper exists on the page"
    );
    assert!(
        html.contains("esc(node.problem)"),
        "node problems are interpolated into innerHTML unescaped"
    );
    assert!(
        html.contains("esc(data.outcome") && html.contains("esc(data.bound_by)"),
        "quarry's own outcome strings are interpolated unescaped"
    );
}
