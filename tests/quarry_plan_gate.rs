//! The plan gate, end to end through a real gateway and a real channel.
//!
//! # What this file is for that the unit tests are not
//!
//! `src/quarry/gate.rs` tests the sequencer against a hand-written responder.
//! `src/quarry/approval.rs` tests the state machine directly. Neither exercises the
//! part that can silently fail in production: the reply arrives as an **ordinary
//! inbound message**, on the same path as every other message, and has to be
//! recognised as an approval *before* the pipeline hands it to an agent. Nothing in
//! the unit tests would notice if that interception were removed — the gate would
//! keep working when driven directly and stop working entirely in chat.
//!
//! So these tests inject through `Gateway::handle_message` and read the replies back
//! off a `TestChannel`. No credentials, no network, no money: the agent manager is a
//! stub provider and quarry is a shell script.
//!
//! # The negative tests assert absence, not error
//!
//! On timeout and on cancel the assertions are that **no child process was started**
//! (a file the fake would have written does not exist) and that the outcome reports
//! **zero spend**. Asserting only that an error came back is what the acceptance
//! criterion for this work calls out by name: an error return with the money already
//! gone is the failure mode, not the fix.

use rustynail::channels::testchan::{CapturedMessages, TestChannel};
use rustynail::config::{Config, QuarryPolicyEntry};
use rustynail::gateway::Gateway;
use rustynail::quarry::{
    run_gated, ApprovedCaps, Decision, PlanDisclosure, RequestedCaps, RunRequest, Unavailable,
};
use rustynail::types::Message;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// ── A quarry that records being run ───────────────────────────────────────────

/// A fake quarry that writes a line to `invocations` before doing anything else.
///
/// The file is the whole point: "was a process spawned" becomes a fact on disk, so a
/// test can assert the *absence* of the spawn rather than inferring it from an absent
/// answer. An absent answer is equally consistent with a run that failed.
struct RecordingQuarry {
    _dir: tempfile::TempDir,
    path: PathBuf,
    invocations: PathBuf,
    runs_dir: PathBuf,
}

impl RecordingQuarry {
    fn build() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("recording-quarry");
        let invocations = dir.path().join("invocations.txt");
        let runs_dir = dir.path().join("runs");

        let script = format!(
            r#"#!/bin/sh
printf 'spawned\n' >> '{log}'
OUT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2;;
    *) shift;;
  esac
done
printf '%s\n' '{{"type":"quarry_stream","version":1,"producer":"quarry-go"}}'
printf '%s\n' '{{"type":"model","tier":"m-1","label":"m-1","state":"done","cost":0.05}}'
printf '%s\n' '{{"type":"answer","text":"two, Phobos and Deimos"}}'
printf '%s\n' '{{"type":"receipt","rows":[{{"label":"n0 q","kind":"llm","cost":0.05}}],"total":0.05}}'
printf '%s\n' '{{"type":"quarry_outcome","outcome":"complete","bound_by":"","gaps":0,"unfunded":0,"total_micros":50000,"cap_micros":5000000}}'
[ -n "$OUT" ] && printf '%s' '{{"RunID":"fake","BoundBy":"","Caps":{{"Spend":5000000,"Latency":0,"Due":"0001-01-01T00:00:00Z"}},"Unverified":null,"Outcomes":[{{"NodeID":"n0","Content":"a","Cost":50000,"Model":"m-1","Verified":true,"Children":null}}]}}' > "$OUT"
exit 0
"#,
            log = invocations.display()
        );
        std::fs::write(&path, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake");
        }

        Self {
            _dir: dir,
            path,
            invocations,
            runs_dir,
        }
    }

    /// How many times a quarry child was started.
    fn spawns(&self) -> usize {
        std::fs::read_to_string(&self.invocations)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }
}

// ── Gateway under test ────────────────────────────────────────────────────────

/// A gateway with a test channel registered, quarry pointed at the fake, and a
/// policy that permits a $5 spend cap.
///
/// The channel is constructed here rather than via `channels.test_channel` so the
/// test keeps a handle to the *same* captured buffer the gateway writes to — a second
/// `TestChannel` has a separate buffer, and draining it would read empty forever and
/// look like a silent gateway.
async fn gateway(
    fake: &RecordingQuarry,
    chunk_limit: Option<usize>,
    dedup: bool,
) -> (Gateway, CapturedMessages) {
    let mut chunking_limits = std::collections::HashMap::new();
    if let Some(limit) = chunk_limit {
        chunking_limits.insert("testchan".to_string(), limit);
    }

    // Deserialized from a near-empty document rather than `Default::default()`, which
    // `Config` does not implement. Every field but `agents.api_key` has a serde
    // default, so this is the same baseline the loader produces for a config file
    // that omits everything — and the key is never used, because the agent provider
    // is the stub and quarry is a shell script.
    let mut config: Config =
        serde_yaml::from_str("gateway: {}\nchannels: {}\nagents:\n  api_key: unused-in-tests")
            .expect("the empty config is the documented baseline");

    config.gateway.api_token = Some("test-token".to_string());
    config.gateway.chunking_enabled = chunk_limit.is_some();
    config.gateway.chunking_limits = chunking_limits;
    config.gateway.deduplication.enabled = dedup;
    config.gateway.deduplication.window_size = 256;
    config.agents.llm_provider = "stub".to_string();
    config.quarry.enabled = true;
    config.quarry.binary_path = fake.path.display().to_string();
    config.quarry.run_record_dir = fake.runs_dir.display().to_string();
    config.quarry.policy.default = Some(QuarryPolicyEntry {
        allowed_denominations: vec!["spend".into()],
        max_spend_micro_usd: Some(5_000_000),
        on_over_limit: "reduce".into(),
        ..Default::default()
    });

    let mut gw = Gateway::new(config);
    let channel = TestChannel::new("testchan-1".to_string());
    let captured = channel.captured_handle();
    gw.register_channel(Box::new(channel)).await;
    (gw, captured)
}

/// Drain the captured outbound messages, as text.
async fn drain(captured: &CapturedMessages) -> Vec<String> {
    TestChannel::drain_captured(captured)
        .await
        .into_iter()
        .map(|m| m.content)
        .collect()
}

fn disclosure(gw_grant: rustynail::quarry::Grant, expires_in: Duration) -> PlanDisclosure {
    PlanDisclosure {
        statement: "how many moons does mars have".to_string(),
        granted: gw_grant,
        adjustments: Vec::new(),
        caps_disclosures: Vec::new(),
        estimate: Err(Unavailable::EstimateWouldItselfSpend),
        exclusions: Err(Unavailable::NoUpstreamPlanMode),
        expires_in,
    }
}

/// Resolve a $5 request through the gateway's real policy, so the caps in the plan
/// are the caps policy actually granted rather than a literal in the test.
async fn grant(gw: &Gateway) -> rustynail::quarry::Grant {
    use rustynail::quarry::CapsPolicy;
    gw.quarry_policy()
        .await
        .resolve(
            "alice",
            "testchan-1",
            &RequestedCaps {
                spend_micro_usd: Some(5_000_000),
                ..Default::default()
            },
        )
        .expect("the test policy permits $5")
}

async fn request(gw: &Gateway) -> RunRequest {
    let mut r = RunRequest::new(
        "alice",
        "testchan-1",
        "how many moons does mars have",
        5_000_000,
    );
    r.env = gw.quarry_child_env().await.expect("token configured");
    r
}

/// Inject `content` from `user_id` as an ordinary inbound message.
async fn inject(gw: &Gateway, user_id: &str, content: &str) {
    gw.handle_message(Message::new(
        "testchan-1".to_string(),
        user_id.to_string(),
        user_id.to_string(),
        content.to_string(),
    ))
    .await
    .expect("the pipeline accepted the message");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A `yes` typed into chat approves the run, and the run happens.
#[tokio::test]
async fn a_yes_in_chat_approves_the_run_and_it_spawns() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-approve",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    // Wait for registration rather than sleeping: a fixed sleep either flakes on a
    // loaded runner or slows every run to the worst case.
    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }

    inject(&gw, "alice", "yes").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    assert!(outcome.ran(), "an approved run must run: {outcome:?}");
    assert_eq!(fake.spawns(), 1);
    assert_eq!(outcome.spend_micro_usd(), Some(50_000));

    // The plan reached the channel, and the approval did not produce a chat reply of
    // its own — the run's own answer is the acknowledgement (#114).
    let sent = drain(&captured).await;
    assert_eq!(sent.len(), 1, "expected only the plan: {sent:?}");
    assert!(sent[0].contains("approve this run"), "{}", sent[0]);
}

/// **Silence spends nothing.** No process, and a receipt that reads zero.
#[tokio::test]
async fn silence_spawns_no_process_and_the_receipt_reads_zero() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, None, false).await;

    let plan = disclosure(grant(&gw).await, Duration::from_millis(60));
    let outcome = run_gated(
        &gw.quarry_approvals(),
        &gw.quarry(),
        &gw.responder("testchan-1"),
        "req-timeout",
        &plan,
        request(&gw).await,
        Duration::from_millis(60),
        None,
    )
    .await
    .expect("the gate itself did not fail");

    assert_eq!(outcome.decision(), Some(Decision::Expired));
    // The two assertions the criterion asks for, in the form it asks for: absence of
    // the effect, and a zero on the receipt.
    assert_eq!(fake.spawns(), 0, "a process ran without approval");
    assert_eq!(outcome.spend_micro_usd(), Some(0));

    let sent = drain(&captured).await;
    assert_eq!(sent.len(), 2, "plan then expiry notice: {sent:?}");
    assert!(sent[1].contains("Nothing was spent"), "{}", sent[1]);
}

/// A `no` typed into chat cancels, at zero spend.
#[tokio::test]
async fn a_no_in_chat_cancels_at_zero_spend() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-cancel",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }
    inject(&gw, "alice", "cancel").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    assert_eq!(outcome.decision(), Some(Decision::Cancelled));
    assert_eq!(fake.spawns(), 0, "a cancelled run spawned a process");
    assert_eq!(outcome.spend_micro_usd(), Some(0));
    assert!(drain(&captured).await[1].contains("Nothing was spent"));
}

/// **A bystander cannot approve.** A group channel is the normal case.
#[tokio::test]
async fn another_user_in_the_same_channel_cannot_approve() {
    let fake = RecordingQuarry::build();
    let (gw, _captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_millis(200));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-bystander",
            &plan,
            req,
            Duration::from_millis(200),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }
    // Bob is in the same channel and says yes, enthusiastically.
    inject(&gw, "bob", "yes").await;
    inject(&gw, "bob", "approve").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    assert_eq!(
        outcome.decision(),
        Some(Decision::Expired),
        "a bystander approved alice's spend"
    );
    assert_eq!(fake.spawns(), 0);
    assert_eq!(outcome.spend_micro_usd(), Some(0));
}

/// An unrecognised reply re-prompts, does not settle, and does not reach the agent.
#[tokio::test]
async fn an_unclear_reply_reprompts_and_the_run_still_waits() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-unclear",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }

    inject(&gw, "alice", "how much will that cost?").await;
    assert_eq!(
        gw.quarry_approvals().pending_count().await,
        1,
        "an unclear reply settled the approval"
    );

    // Then an actual answer.
    inject(&gw, "alice", "yes").await;
    let outcome = gate.await.expect("join").expect("gate ok");
    assert!(outcome.ran());

    let sent = drain(&captured).await;
    // Plan, re-prompt, and nothing from the agent: the question was swallowed by the
    // gate rather than answered by a chat completion while the run waited.
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent[1].contains("not read that as a yes or a no"),
        "{}",
        sent[1]
    );
    assert!(sent[1].contains("nothing has been spent"), "{}", sent[1]);
}

/// The plan message goes through the chunker, so a tight platform limit splits it
/// rather than truncating it.
///
/// Teams' limit is 1024 and a plan with adjustments and disclosures exceeds it. The
/// limit here is deliberately smaller so the split is unambiguous, and the assertion
/// is on reassembly plus the instruction surviving — a plan whose last line is cut
/// off is a plan a sender cannot answer.
#[tokio::test]
async fn a_long_plan_is_chunked_rather_than_truncated() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, Some(200), false).await;

    let plan = disclosure(grant(&gw).await, Duration::from_millis(60));
    let full = rustynail::quarry::render_plan(&plan);
    assert!(full.len() > 200, "the fixture must exceed the limit");

    let _ = run_gated(
        &gw.quarry_approvals(),
        &gw.quarry(),
        &gw.responder("testchan-1"),
        "req-chunk",
        &plan,
        request(&gw).await,
        Duration::from_millis(60),
        None,
    )
    .await
    .expect("gate ok");

    let sent = drain(&captured).await;
    assert!(sent.len() > 2, "the plan was not chunked: {sent:?}");
    for chunk in &sent {
        assert!(chunk.len() <= 200, "chunk over the limit: {}", chunk.len());
    }
    // The reply instruction is the one line that must survive, wherever it landed.
    assert!(
        sent.iter().any(|c| c.contains("to cancel")),
        "the instruction was lost: {sent:?}"
    );
}

/// Two senders' approvals do not interfere, and each answers only their own.
#[tokio::test]
async fn concurrent_gates_for_two_senders_stay_separate() {
    let fake = RecordingQuarry::build();
    let (gw, _captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let mut gates = Vec::new();
    for user in ["alice", "bob"] {
        let g = Arc::clone(&gw);
        let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
        let mut req = request(&gw).await;
        req.user_id = user.to_string();
        gates.push((
            user,
            tokio::spawn(async move {
                run_gated(
                    &g.quarry_approvals(),
                    &g.quarry(),
                    &g.responder("testchan-1"),
                    &format!("req-{user}"),
                    &plan,
                    req,
                    Duration::from_secs(10),
                    None,
                )
                .await
            }),
        ));
    }

    while gw.quarry_approvals().pending_count().await < 2 {
        tokio::task::yield_now().await;
    }

    // Alice approves, Bob cancels.
    inject(&gw, "alice", "yes").await;
    inject(&gw, "bob", "no").await;

    for (user, gate) in gates {
        let outcome = gate.await.expect("join").expect("gate ok");
        match user {
            "alice" => assert!(outcome.ran(), "alice's approval did not run"),
            _ => {
                assert_eq!(outcome.decision(), Some(Decision::Cancelled));
                assert_eq!(outcome.spend_micro_usd(), Some(0));
            }
        }
    }
    assert_eq!(fake.spawns(), 1, "bob's cancellation spawned something");
}

/// A repeated approval word is not swallowed by the deduplicator.
///
/// Two runs in one session are approved with the byte-identical word `yes`. The
/// dedup ring buffer hashes `user_id:content`, so without the pending-approval
/// exemption the second `yes` is dropped as a repeat and the second run expires with
/// nothing in the log to explain it.
#[tokio::test]
async fn a_second_identical_approval_is_not_dropped_as_a_duplicate() {
    let fake = RecordingQuarry::build();
    // Dedup on, which is what makes this test meaningful.
    let (gw, _captured) = gateway(&fake, None, true).await;
    let gw = Arc::new(gw);

    for (i, request_id) in ["req-first", "req-second"].iter().enumerate() {
        let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
        let req = request(&gw).await;
        let g = Arc::clone(&gw);
        let id = request_id.to_string();
        let gate = tokio::spawn(async move {
            run_gated(
                &g.quarry_approvals(),
                &g.quarry(),
                &g.responder("testchan-1"),
                &id,
                &plan,
                req,
                Duration::from_secs(10),
                None,
            )
            .await
        });

        while gw.quarry_approvals().pending_count().await == 0 {
            tokio::task::yield_now().await;
        }
        inject(&gw, "alice", "yes").await;

        let outcome = gate.await.expect("join").expect("gate ok");
        assert!(
            outcome.ran(),
            "approval {} was dropped as a duplicate: {outcome:?}",
            i + 1
        );
    }
    assert_eq!(fake.spawns(), 2);
}

/// The approved caps are what policy granted, not what the sender asked for.
///
/// A sender asking for $50 under a $5 policy is approving $5, and the audit record
/// and the plan message must both say $5. Approving a figure the run cannot use is
/// how a sender comes to believe a cap they never had.
#[tokio::test]
async fn the_plan_states_the_granted_caps_not_the_requested_ones() {
    use rustynail::quarry::CapsPolicy;

    let fake = RecordingQuarry::build();
    let (gw, _captured) = gateway(&fake, None, false).await;

    let granted = gw
        .quarry_policy()
        .await
        .resolve(
            "alice",
            "testchan-1",
            &RequestedCaps {
                spend_micro_usd: Some(50_000_000),
                ..Default::default()
            },
        )
        .expect("reduce, not refuse");
    assert_eq!(granted.spend_micro_usd, Some(5_000_000), "policy clamps");

    let approved = ApprovedCaps::from_grant(&granted);
    assert_eq!(approved.spend_micro_usd, Some(5_000_000));

    let mut plan = disclosure(granted.clone(), Duration::from_secs(300));
    plan.adjustments = granted.adjustments.clone();
    let text = rustynail::quarry::render_plan(&plan);

    // The limits section carries the grant and only the grant. The $50 appears
    // exactly once, in the adjustment line, where naming it is the point — a
    // reduction the sender cannot see is the quiet degradation P9 forbids.
    let limits = text
        .split("**I had to change what you asked for**")
        .next()
        .expect("the limits section precedes the adjustments");
    assert!(limits.contains("at most $5.0000"), "{limits}");
    assert!(
        !limits.contains("$50.0000"),
        "the request leaked into the limits: {limits}"
    );
    assert!(
        text.contains("you asked for spend $50.0000, policy allows $5.0000"),
        "the clamp was silent: {text}"
    );
}

// ── Delivery: the answer and its receipt, together ────────────────────────────

/// An approved run's answer reaches the sender **with its receipt attached**.
///
/// `Gateway::deliver_quarry_outcome` is the only path an outcome takes to a sender,
/// and this is the property that makes it worth being the only one: the answer and
/// the receipt are rendered together by one call with no option to emit just the
/// first. A second delivery site would eventually send a bare answer, which is the
/// artifact quarry exists to replace.
#[tokio::test]
async fn a_delivered_run_carries_its_receipt_and_not_just_the_answer() {
    let fake = RecordingQuarry::build();
    let (gw, captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-deliver",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }
    inject(&gw, "alice", "yes").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    let run = match outcome {
        rustynail::quarry::GateOutcome::Ran(o) => o,
        other => panic!("expected a run: {other:?}"),
    };

    // Drain the plan message first so what remains is only the delivery.
    let _plan = drain(&captured).await;
    gw.deliver_quarry_outcome("testchan-1", &run)
        .await
        .expect("delivery through the real outbound path");

    let sent = drain(&captured).await.join("\n");
    assert!(
        sent.contains("two, Phobos and Deimos"),
        "the answer did not arrive: {sent}"
    );
    // The receipt, not merely a cost figure: the heading, the spend, the trust
    // statement, and something citable.
    assert!(sent.contains("**Receipt**"), "no receipt: {sent}");
    assert!(sent.contains("$0.0500"), "no spend figure: {sent}");
    assert!(
        sent.contains("How much to trust it"),
        "no trust section: {sent}"
    );
    assert!(sent.contains("Full record"), "nothing citable: {sent}");
    // The fake publishes no provenance, which is the common case — and it must read
    // as unmeasured rather than as a zero.
    assert!(
        sent.contains("not measured"),
        "stability read as 0%: {sent}"
    );
    assert!(
        !sent.contains("0% of claims"),
        "silence was rendered as a finding: {sent}"
    );
}

/// Delivery on Teams' 1024-byte limit chunks the reply and keeps the receipt.
///
/// The tightest of the five platform defaults, driven through the gateway's own
/// chunker rather than a chunker the test constructs — a delivery path that bypassed
/// the chunker would arrive truncated here, with the footer being the part cut off.
#[tokio::test]
async fn delivery_under_a_tight_platform_limit_chunks_rather_than_dropping_the_receipt() {
    let fake = RecordingQuarry::build();
    // 200 bytes: tighter than any real platform, so the footer alone spans several
    // messages and "it happened to fit" cannot be why this passes.
    let (gw, captured) = gateway(&fake, Some(200), false).await;
    let gw = Arc::new(gw);

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-deliver-chunked",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }
    inject(&gw, "alice", "yes").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    let run = match outcome {
        rustynail::quarry::GateOutcome::Ran(o) => o,
        other => panic!("expected a run: {other:?}"),
    };

    let _plan = drain(&captured).await;
    gw.deliver_quarry_outcome("testchan-1", &run)
        .await
        .expect("delivery");

    let chunks = drain(&captured).await;
    assert!(
        chunks.len() > 1,
        "nothing was chunked at a 200-byte limit, so this proves nothing: {chunks:?}"
    );
    for (i, c) in chunks.iter().enumerate() {
        assert!(c.len() <= 200, "chunk {i} is {} bytes: {c}", c.len());
    }
    let reassembled = chunks.join(" ");
    assert!(
        reassembled.contains("**Receipt**"),
        "the receipt was dropped to fit: {reassembled}"
    );
    assert!(
        reassembled.contains("Full record"),
        "the citation was dropped to fit: {reassembled}"
    );
}

/// Delivering an outcome publishes it to the dashboard too.
///
/// One call does both, so an operator's view and the sender's reply cannot disagree
/// about what a run cost. Subscribing before delivery is what makes this assertable:
/// the broadcast drops messages with no subscribers, so a subscribe-after would read
/// empty and look like a silent gateway.
#[tokio::test]
async fn delivering_a_run_also_publishes_its_tree_to_the_dashboard() {
    let fake = RecordingQuarry::build();
    let (gw, _captured) = gateway(&fake, None, false).await;
    let gw = Arc::new(gw);

    let mut events = gw.stats().subscribe();

    let plan = disclosure(grant(&gw).await, Duration::from_secs(10));
    let req = request(&gw).await;
    let g = Arc::clone(&gw);
    let gate = tokio::spawn(async move {
        run_gated(
            &g.quarry_approvals(),
            &g.quarry(),
            &g.responder("testchan-1"),
            "req-deliver-dash",
            &plan,
            req,
            Duration::from_secs(10),
            None,
        )
        .await
    });

    while gw.quarry_approvals().pending_count().await == 0 {
        tokio::task::yield_now().await;
    }
    inject(&gw, "alice", "yes").await;

    let outcome = gate.await.expect("join").expect("gate ok");
    let run = match outcome {
        rustynail::quarry::GateOutcome::Ran(o) => o,
        other => panic!("expected a run: {other:?}"),
    };

    gw.deliver_quarry_outcome("testchan-1", &run)
        .await
        .expect("delivery");

    // Walk past the message events the plan and the reply generated.
    let mut found = None;
    while let Ok(ev) = events.try_recv() {
        if let rustynail::gateway::dashboard::DashboardEvent::QuarryRun { .. } = ev {
            found = Some(ev);
            break;
        }
    }
    let ev = found.expect("delivery must publish a QuarryRun event");
    let rustynail::gateway::dashboard::DashboardEvent::QuarryRun {
        spend_micro_usd,
        nodes,
        stability_measured,
        ..
    } = ev
    else {
        unreachable!("matched above");
    };
    assert_eq!(spend_micro_usd, Some(50_000));
    assert_eq!(nodes.len(), 1, "the fake's record has one node");
    assert_eq!(nodes[0].id, "n0");
    assert!(
        !stability_measured,
        "the fake publishes no provenance, so nothing is measured"
    );
}
