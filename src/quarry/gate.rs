//! Sequencing a quarry run behind its plan gate.
//!
//! [`super::approval`] holds the pending-approval state machine. This module is the
//! order of operations around it, and the order is the security property:
//!
//! 1. register the approval, so the earliest possible reply has somewhere to land
//! 2. send the plan
//! 3. wait
//! 4. spawn **only** on [`Decision::Approved`]
//!
//! Steps 1 and 2 are in that order deliberately. Sending first and registering
//! after loses a reply that arrives in the gap, which on a fast channel is a run
//! that expires despite having been approved, with nothing in the log to say why.
//!
//! # The outcome type reports spend, including when there was none
//!
//! [`GateOutcome::spend_micro_usd`] returns `0` for every unapproved decision. That
//! is not a convenience: the acceptance criterion for this work asks for the
//! *absence of the side effect* to be assertable — "an error return with the spend
//! already incurred is precisely the failure mode" — so a cancelled gate has to
//! produce something a test can read a zero off, rather than an `Err` that proves
//! only that the caller was told something went wrong.

use super::approval::{
    render_cancelled, render_expired, render_plan, render_superseded, ApprovalRegistry,
    ApprovedCaps, Decision, PlanDisclosure,
};
use super::supervisor::{RunOutcome, RunRequest, SpawnError, Supervisor};
use super::RunEvent;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Somewhere to put a message for the sender.
///
/// A trait rather than a `&dyn Channel` because the gateway's send path is not just
/// a channel call — it runs the text through the formatter and the chunker first,
/// and the plan message must take that same path or it arrives truncated on Teams.
/// It is also what lets the gate be tested without a gateway.
#[async_trait]
pub trait Responder: Send + Sync {
    /// Deliver `text` to the sender, formatted and chunked for their platform.
    async fn reply(&self, text: &str) -> anyhow::Result<()>;
}

/// Why a gated run produced no run.
#[derive(Debug)]
pub enum GateOutcome {
    /// The sender approved and the run executed. Carries whatever the supervisor
    /// reported, including a failed run — an approved run that crashed is still a
    /// run that happened.
    Ran(Box<RunOutcome>),
    /// The gate did not open. **No process was spawned and no spend occurred.**
    NotRun {
        /// Which non-approval settled it.
        decision: Decision,
    },
    /// The sender approved but the run could not start.
    ///
    /// Distinct from [`Self::NotRun`] because the sender agreed to spend and the
    /// failure is the host's, not theirs — the reply owed to them is an apology and
    /// a reason, not a confirmation that nothing was spent.
    SpawnFailed {
        /// Why the spawn was refused.
        error: SpawnError,
    },
}

impl GateOutcome {
    /// What this cost, in int64 micro-dollars.
    ///
    /// Zero for everything that did not run. Read from the receipt event for a run
    /// that did, so the number here is quarry's own accounting rather than the
    /// host's guess at it — a run whose receipt is missing reports `None` and is not
    /// silently reported as free.
    pub fn spend_micro_usd(&self) -> Option<i64> {
        match self {
            Self::Ran(outcome) => outcome.cost_micro_usd(),
            // A gate that never opened spent nothing, and that is a fact rather than
            // an absent measurement — so `Some(0)`, not `None`.
            Self::NotRun { .. } | Self::SpawnFailed { .. } => Some(0),
        }
    }

    /// Whether a quarry process was started.
    pub fn ran(&self) -> bool {
        matches!(self, Self::Ran(_))
    }

    /// The decision that settled the gate, when it did not open.
    pub fn decision(&self) -> Option<Decision> {
        match self {
            Self::NotRun { decision } => Some(*decision),
            _ => None,
        }
    }
}

/// Ask the sender, then run — or don't.
///
/// `request_id` correlates the audit records; the caller supplies it so the same id
/// appears on the policy decision, the plan decision, and the run.
///
/// `events` is forwarded to the supervisor unchanged, so a caller rendering a live
/// tree sees events from the moment the run starts. It receives nothing at all when
/// the gate does not open, which is the observable form of "no model call was made".
#[allow(clippy::too_many_arguments)]
pub async fn run_gated(
    registry: &ApprovalRegistry,
    supervisor: &Arc<Supervisor>,
    responder: &dyn Responder,
    request_id: &str,
    disclosure: &PlanDisclosure,
    request: RunRequest,
    timeout: Duration,
    events: Option<mpsc::UnboundedSender<RunEvent>>,
) -> anyhow::Result<GateOutcome> {
    let caps = ApprovedCaps::from_grant(&disclosure.granted);

    // Register first. See the module docs — the gap between sending and registering
    // is where an approval goes missing.
    let ticket = registry
        .begin(
            request_id,
            &request.user_id,
            &request.channel_id,
            caps,
            timeout,
        )
        .await;

    // A send failure here is fatal to the gate, and deliberately so: the sender
    // never saw the plan, so there is nobody who could approve it. Returning `Err`
    // rather than waiting out the timeout means the caller reports a delivery
    // failure instead of "you did not answer in time".
    if let Err(e) = responder.reply(&render_plan(disclosure)).await {
        // The registration is left to expire rather than settled here. Settling it
        // would need a decision, and none of them is true: the sender neither
        // cancelled nor ran out of time.
        return Err(e.context("could not send the plan message, so nothing was run"));
    }

    let decision = ticket.wait().await;
    if !decision.may_spend() {
        // Say so, and say that nothing was spent. A silent cancellation is
        // indistinguishable from a run that is still going.
        let text = match decision {
            Decision::Cancelled => render_cancelled(),
            Decision::Expired => render_expired(),
            // The superseding request's own plan message is already on its way, so
            // this one is context rather than the whole story.
            Decision::Superseded => render_superseded(),
            // Unreachable while `may_spend` is an explicit match, and a compile
            // error there rather than a wrong message here is the point.
            Decision::Approved => unreachable!("may_spend() is false"),
        };
        responder.reply(&text).await?;
        return Ok(GateOutcome::NotRun { decision });
    }

    match supervisor.run(request, events, None).await {
        Ok(outcome) => Ok(GateOutcome::Ran(Box::new(outcome))),
        Err(error) => Ok(GateOutcome::SpawnFailed { error }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QuarryConfig;
    use crate::quarry::approval::Unavailable;
    use crate::quarry::fake::FakeBehavior;
    use crate::quarry::policy::{Grant, ScopeTags};
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Captures replies without a channel, and counts them.
    #[derive(Default)]
    struct Recorder {
        sent: Mutex<Vec<String>>,
        /// When set, `reply` fails — the "the sender never saw the plan" case.
        fail: bool,
    }

    #[async_trait]
    impl Responder for Recorder {
        async fn reply(&self, text: &str) -> anyhow::Result<()> {
            if self.fail {
                anyhow::bail!("channel is down");
            }
            self.sent.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    impl Recorder {
        fn texts(&self) -> Vec<String> {
            self.sent.lock().unwrap().clone()
        }
    }

    fn grant() -> Grant {
        Grant {
            spend_micro_usd: Some(5_000_000),
            latency: None,
            due: None,
            scope: ScopeTags::mint(BTreeMap::from([("user".to_string(), "alice".to_string())]))
                .expect("mint"),
            adjustments: Vec::new(),
        }
    }

    fn disclosure() -> PlanDisclosure {
        PlanDisclosure {
            statement: "how many moons does mars have".to_string(),
            granted: grant(),
            adjustments: Vec::new(),
            caps_disclosures: Vec::new(),
            estimate: Err(Unavailable::EstimateWouldItselfSpend),
            exclusions: Err(Unavailable::NoUpstreamPlanMode),
            expires_in: Duration::from_secs(300),
        }
    }

    /// A supervisor over a fake quarry that records every invocation, so "was a
    /// process spawned" is checkable rather than inferred.
    struct Harness {
        _dir: tempfile::TempDir,
        supervisor: Arc<Supervisor>,
        invocations: std::path::PathBuf,
    }

    fn harness() -> Harness {
        let dir = tempfile::tempdir().expect("temp dir");
        let invocations = dir.path().join("invocations.txt");

        // A shim in front of the fake appends one line per invocation before
        // delegating, so "was a child started" is a fact on disk rather than an
        // inference from the absence of events.
        let fake = FakeBehavior::happy().build();
        let shim = dir.path().join("shim");
        let script = format!(
            "#!/bin/sh\nprintf 'spawned\\n' >> '{log}'\nexec '{real}' \"$@\"\n",
            log = invocations.display(),
            real = fake.path.display()
        );
        std::fs::write(&shim, script).expect("write shim");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
                .expect("chmod shim");
        }
        // The fake's own temp dir must outlive this function; leak it deliberately
        // rather than have the shim point at a deleted path.
        std::mem::forget(fake);

        let config = QuarryConfig {
            enabled: true,
            binary_path: shim.display().to_string(),
            max_concurrent_runs: 4,
            run_record_dir: dir.path().join("runs").display().to_string(),
            run_timeout_seconds: 30,
            ..QuarryConfig::default()
        };

        Harness {
            _dir: dir,
            supervisor: Arc::new(Supervisor::new(config)),
            invocations,
        }
    }

    impl Harness {
        /// How many times a quarry child was started.
        fn spawns(&self) -> usize {
            std::fs::read_to_string(&self.invocations)
                .map(|s| s.lines().count())
                .unwrap_or(0)
        }
    }

    fn request() -> RunRequest {
        let mut r = RunRequest::new(
            "alice",
            "testchan-1",
            "how many moons does mars have",
            5_000_000,
        );
        r.env
            .insert("QUARRY_PROVIDER_URL".into(), "http://127.0.0.1:1/v1".into());
        r
    }

    /// The whole point, stated as one test: an unanswered gate spawns nothing and
    /// its receipt reads zero.
    #[tokio::test]
    async fn a_timeout_spawns_no_process_and_reports_zero_spend() {
        let h = harness();
        let registry = ApprovalRegistry::new();
        let responder = Recorder::default();
        let (tx, mut rx) = mpsc::unbounded_channel();

        let outcome = run_gated(
            &registry,
            &h.supervisor,
            &responder,
            "req-1",
            &disclosure(),
            request(),
            Duration::from_millis(40),
            Some(tx),
        )
        .await
        .expect("the gate itself did not fail");

        assert_eq!(outcome.decision(), Some(Decision::Expired));
        assert!(!outcome.ran());
        // Not "an error was returned" — the absence of the effect.
        assert_eq!(outcome.spend_micro_usd(), Some(0));
        assert_eq!(h.spawns(), 0, "a process was started without approval");
        assert!(rx.try_recv().is_err(), "an event arrived, so something ran");

        // And the sender was told, in terms that say nothing was spent.
        let texts = responder.texts();
        assert_eq!(texts.len(), 2, "plan then expiry: {texts:?}");
        assert!(texts[1].contains("Nothing was spent"), "{}", texts[1]);
    }

    #[tokio::test]
    async fn a_cancellation_spawns_no_process_and_reports_zero_spend() {
        let h = harness();
        let registry = Arc::new(ApprovalRegistry::new());
        let responder = Arc::new(Recorder::default());

        let (r, s, sup, resp) = (
            Arc::clone(&registry),
            "req-2".to_string(),
            Arc::clone(&h.supervisor),
            Arc::clone(&responder),
        );
        let gate = tokio::spawn(async move {
            run_gated(
                &r,
                &sup,
                resp.as_ref(),
                &s,
                &disclosure(),
                request(),
                Duration::from_secs(5),
                None,
            )
            .await
        });

        while registry.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }
        registry.submit_reply("alice", "testchan-1", "no").await;

        let outcome = gate.await.expect("join").expect("gate ok");
        assert_eq!(outcome.decision(), Some(Decision::Cancelled));
        assert_eq!(outcome.spend_micro_usd(), Some(0));
        assert_eq!(h.spawns(), 0, "a cancelled run spawned a process");
        assert!(responder.texts()[1].contains("Nothing was spent"));
    }

    #[tokio::test]
    async fn an_approval_runs_and_the_receipt_is_quarrys_own() {
        let h = harness();
        let registry = Arc::new(ApprovalRegistry::new());
        let responder = Arc::new(Recorder::default());

        let (r, sup, resp) = (
            Arc::clone(&registry),
            Arc::clone(&h.supervisor),
            Arc::clone(&responder),
        );
        let gate = tokio::spawn(async move {
            run_gated(
                &r,
                &sup,
                resp.as_ref(),
                "req-3",
                &disclosure(),
                request(),
                Duration::from_secs(10),
                None,
            )
            .await
        });

        while registry.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }
        registry.submit_reply("alice", "testchan-1", "yes").await;

        let outcome = gate.await.expect("join").expect("gate ok");
        assert!(outcome.ran(), "an approved run must run: {outcome:?}");
        assert_eq!(h.spawns(), 1);
        // 0.07 + 0.29 from the fake's receipt, in micro-dollars.
        assert_eq!(outcome.spend_micro_usd(), Some(360_000));
        // Only the plan was sent; the answer is the caller's to compose (#114).
        assert_eq!(responder.texts().len(), 1);
    }

    /// A plan that could not be delivered fails the gate rather than waiting out the
    /// timeout, and still spawns nothing.
    #[tokio::test]
    async fn an_undeliverable_plan_fails_without_running_anything() {
        let h = harness();
        let registry = ApprovalRegistry::new();
        let responder = Recorder {
            fail: true,
            ..Default::default()
        };

        let err = run_gated(
            &registry,
            &h.supervisor,
            &responder,
            "req-4",
            &disclosure(),
            request(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect_err("an undelivered plan cannot be approved");

        assert!(
            err.to_string().contains("nothing was run"),
            "the error must say nothing ran: {err}"
        );
        assert_eq!(h.spawns(), 0);
    }

    /// The approval is answerable before the plan is sent.
    ///
    /// Guards the ordering in `run_gated`: a reply racing the plan message must
    /// settle the gate rather than fall through as an ordinary message. Driven by a
    /// responder that replies *to itself* from inside `reply`, which is the tightest
    /// possible version of that race.
    #[tokio::test]
    async fn a_reply_racing_the_plan_message_still_settles_the_gate() {
        struct Racer {
            registry: Arc<ApprovalRegistry>,
            /// Only the *plan* send races. The cancellation message that follows has
            /// nothing left to answer, and replying to it would assert the opposite.
            raced: Mutex<bool>,
        }

        #[async_trait]
        impl Responder for Racer {
            async fn reply(&self, _text: &str) -> anyhow::Result<()> {
                {
                    let mut raced = self.raced.lock().unwrap();
                    if *raced {
                        return Ok(());
                    }
                    *raced = true;
                }
                // Answering during the send: only reachable because `run_gated`
                // registers before it sends.
                let outcome = self
                    .registry
                    .submit_reply("alice", "testchan-1", "no")
                    .await;
                assert!(
                    matches!(outcome, crate::quarry::ReplyOutcome::Settled { .. }),
                    "the approval was not yet answerable: {outcome:?}"
                );
                Ok(())
            }
        }

        let h = harness();
        let registry = Arc::new(ApprovalRegistry::new());
        let responder = Racer {
            registry: Arc::clone(&registry),
            raced: Mutex::new(false),
        };

        let outcome = run_gated(
            &registry,
            &h.supervisor,
            &responder,
            "req-5",
            &disclosure(),
            request(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("gate ok");

        assert_eq!(outcome.decision(), Some(Decision::Cancelled));
        assert_eq!(h.spawns(), 0);
    }
}
