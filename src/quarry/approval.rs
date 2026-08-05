//! The plan gate: a quarry run waits for its sender to approve, in chat.
//!
//! A quarry run spends real money. Between deciding what to spend and spending it
//! there is exactly one moment where changing the answer is free, and this module
//! is that moment: the gateway states what it is about to do, and does nothing until
//! the sender says yes.
//!
//! # Nothing in this repo could wait for a human before this
//!
//! The brief for this work said to reuse the existing approval mechanism — the
//! shell tool's. It is not one. `src/tools/shell.rs` returns "call again with
//! approved=true" and reads `approved` from a **tool parameter the model supplies**.
//! Inside the ReAct loop the agent reads that sentence and calls again with
//! `approved=true`. No human is ever consulted. It is a speed bump for the model
//! wearing the costume of a boundary — the same shape of mistake as a shell
//! allowlist that allows everything.
//!
//! So this is built from nothing rather than layered on that. The registry here is
//! deliberately not quarry-specific in its mechanics (see [`PendingApproval`]);
//! moving the shell tool onto it is a separate change and explicitly not done here.
//!
//! # Silence is never consent
//!
//! There is no `default_approve` setting and no way to configure one. A timeout
//! **cancels**. This is the one inversion that would make every other guarantee in
//! the milestone worthless: a gate that approves when nobody answers is a gate that
//! spends money when the sender has gone to lunch.
//!
//! # What the gate can honestly say today
//!
//! quarry's P9 promises disclosure of planned degradation *before* spend. The
//! gateway cannot obtain that disclosure yet, and the plan message must not imply
//! otherwise:
//!
//! - There is **no plan-only mode** in quarry's CLI. `quarry run` plans and spends
//!   in one process; `main.go` dispatches `run`, `show` and `replay` and nothing
//!   else. There is no `--dry-run`.
//! - The `RunEvent` stream is **post-hoc**. `RunEvents(r RunRecord, …)` folds a
//!   *completed* record into events, so no event exists before the run finishes.
//!   Reading a plan off the stream is not merely unimplemented, it is backwards.
//! - `Probe()` exists in quarry's library and returns the depth-1 branching factor
//!   an estimate needs — but it is not wired into the CLI, and quarry is explicit
//!   that it **does spend**: "it is a real planner call". Using it to price the gate
//!   would spend money to decide whether to spend money, which defeats the
//!   zero-spend cancel this module guarantees.
//!
//! Therefore [`PlanDisclosure`] states the **caps in force** — which the gateway
//! knows exactly, having just granted them — and marks the cost estimate and the
//! exclusion list as unavailable, naming the reason. A fabricated estimate would be
//! worse than none: the sender would approve against a number nobody measured, and
//! `estimate.go` says of its own projections that they are advisory and that near
//! `m = 1` "any single number is theatre". When upstream Q1 lands, [`PlanDisclosure`]
//! grows the real fields and [`Unavailable`] stops being constructed.
//!
//! What the sender is asked to approve is therefore precise: **the cap**, not the
//! cost. Under P4 the cap is the contract — quarry plans to fit it — so the cap is
//! the number that actually bounds the bill, and it is the honest thing to gate on.

use crate::audit::{AuditEvent, AuditLogger};
use crate::quarry::policy::{CapAdjustment, Grant};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex};

// ── The reply vocabulary ──────────────────────────────────────────────────────

/// Words that approve. Compared lowercased, after trimming and stripping trailing
/// punctuation.
const APPROVE_WORDS: &[&str] = &["yes", "y", "approve", "approved", "ok", "okay", "go", "run"];

/// Words that cancel.
const CANCEL_WORDS: &[&str] = &["no", "n", "cancel", "stop", "abort", "nope", "nevermind"];

/// What a sender's reply meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// An explicit, positive approval.
    Approve,
    /// An explicit cancellation.
    Cancel,
    /// Neither. **Not** treated as either answer — the sender is re-prompted.
    ///
    /// This variant existing is the point. Mapping "maybe" or "how much?" onto
    /// approve spends money the sender did not agree to; mapping it onto cancel
    /// throws away a run they may still want. Re-prompting is the only reading that
    /// is wrong in neither direction.
    Unrecognised,
}

/// Classify a sender's reply.
///
/// Forgiving about shape, strict about meaning: case, surrounding whitespace and
/// trailing punctuation are ignored, but a reply containing anything beyond a single
/// recognised word is [`Reply::Unrecognised`]. "yes but only $2" is not an approval
/// of the plan that was shown — it is a different request, and treating it as
/// consent would approve caps the sender was arguing with.
pub fn classify_reply(text: &str) -> Reply {
    let cleaned: String = text
        .trim()
        .trim_end_matches(['.', '!', '?', ','])
        .to_lowercase();

    // Multi-word replies are deliberately not scanned for a keyword. "no, wait —
    // yes" and "yes, but cheaper" both contain an approve word and neither is one.
    if cleaned.contains(char::is_whitespace) {
        return Reply::Unrecognised;
    }
    if APPROVE_WORDS.contains(&cleaned.as_str()) {
        return Reply::Approve;
    }
    if CANCEL_WORDS.contains(&cleaned.as_str()) {
        return Reply::Cancel;
    }
    Reply::Unrecognised
}

/// The accepted vocabulary, for the prompt text and the docs.
pub fn vocabulary_hint() -> String {
    format!(
        "Reply **{}** to run it, or **{}** to cancel.",
        APPROVE_WORDS[..3].join("` / `").replace('`', ""),
        CANCEL_WORDS[..3].join("` / `").replace('`', "")
    )
}

// ── What is disclosed ─────────────────────────────────────────────────────────

/// Why a field of the plan disclosure is absent.
///
/// Absence is reported with a reason rather than rendered as a zero or an
/// approximation. A sender who sees "estimated cost: $0.00" approves against a
/// measurement that was never taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// quarry's CLI has no plan-only mode: `quarry run` plans and spends in one
    /// process, and the event stream is folded from a completed record. There is
    /// nothing to read before the run.
    NoUpstreamPlanMode,
    /// quarry can estimate — `Probe()` plus the Galton-Watson projection — but the
    /// probe is a real planner call that spends. Pricing the gate with it would
    /// spend before the sender agreed to spend.
    EstimateWouldItselfSpend,
}

impl Unavailable {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoUpstreamPlanMode => "no_upstream_plan_mode",
            Self::EstimateWouldItselfSpend => "estimate_would_itself_spend",
        }
    }

    /// The sender-facing explanation.
    pub fn message(&self) -> &'static str {
        match self {
            Self::NoUpstreamPlanMode => {
                "quarry plans and spends in one step, so there is no plan to show you \
                 in advance yet"
            }
            Self::EstimateWouldItselfSpend => {
                "estimating the cost would itself cost money, so I would rather show \
                 you the limit than spend to guess the bill"
            }
        }
    }
}

/// Everything the sender is told before spend.
///
/// The caps are exact — the gateway granted them and quarry will plan to fit them
/// (P4, the cap is the contract). The cost estimate and the exclusion list are
/// [`Unavailable`] with a reason until upstream Q1 lands. See the module docs.
#[derive(Debug, Clone)]
pub struct PlanDisclosure {
    /// The problem, as it will be sent to quarry.
    pub statement: String,
    /// The caps in force, already clamped by policy.
    pub granted: Grant,
    /// Adjustments policy made to what the sender asked for. Restated here, at the
    /// gate, because this is the last point at which they are still free to fix.
    pub adjustments: Vec<CapAdjustment>,
    /// Disclosures carried over from caps parsing — notably the resolved deadline
    /// and its timezone. Surfaced *here* so a timezone misread is caught before
    /// spend rather than discovered in the bill.
    pub caps_disclosures: Vec<crate::quarry::Disclosure>,
    /// Why no cost estimate is shown.
    pub estimate: Result<(), Unavailable>,
    /// Why no exclusion list is shown.
    pub exclusions: Result<Vec<String>, Unavailable>,
    /// How long the sender has to answer.
    pub expires_in: Duration,
}

// ── A pending approval ────────────────────────────────────────────────────────

/// How a pending approval was settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The sender approved. The only path on which a run may spend.
    Approved,
    /// The sender cancelled.
    Cancelled,
    /// Nobody answered in time. **Cancels** — silence is not consent.
    Expired,
    /// A newer request from the same sender replaced this one.
    Superseded,
}

impl Decision {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    /// Whether a run may proceed to spend.
    ///
    /// Written as an explicit match rather than `!= Cancelled` so that adding a
    /// variant is a compile error here instead of silently joining the permitted
    /// set. A new terminal state defaulting to "may spend" is the failure this
    /// prevents.
    pub fn may_spend(&self) -> bool {
        match self {
            Self::Approved => true,
            Self::Cancelled | Self::Expired | Self::Superseded => false,
        }
    }
}

/// One approval awaiting its sender.
///
/// Keyed by `(channel_id, user_id)` in the registry, and carries `request_id` so a
/// late reply to a superseded request is not mistaken for a reply to the current
/// one.
struct PendingApproval {
    request_id: String,
    /// Who may answer. Compared exactly — see
    /// [`ApprovalRegistry::submit_reply`].
    user_id: String,
    channel_id: String,
    /// Wakes the waiting run. `None` once settled.
    settle: Option<oneshot::Sender<Decision>>,
    /// When this stops being answerable.
    deadline: Instant,
    /// The caps on offer, recorded in the audit event so the log says what was
    /// approved and not merely that something was.
    caps: ApprovedCaps,
}

/// The caps a decision applied to, for the audit record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovedCaps {
    pub spend_micro_usd: Option<i64>,
    pub latency_seconds: Option<u64>,
    pub due: Option<String>,
}

impl ApprovedCaps {
    /// Read the caps off a grant.
    pub fn from_grant(grant: &Grant) -> Self {
        Self {
            spend_micro_usd: grant.spend_micro_usd,
            latency_seconds: grant.latency.map(|d| d.as_secs()),
            due: grant.due.map(|d| d.to_rfc3339()),
        }
    }
}

/// The outcome of offering a reply to the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyOutcome {
    /// The reply settled a pending approval.
    Settled {
        request_id: String,
        decision: Decision,
    },
    /// A pending approval exists for this sender, but the reply was not a yes or a
    /// no. The caller re-prompts; the approval stays pending and its deadline does
    /// **not** move — an unrecognised reply cannot be used to extend the window
    /// indefinitely.
    NeedsClarification { request_id: String },
    /// This sender has nothing pending. The message is an ordinary one and the
    /// caller must handle it normally, **not** swallow it: "yes" with nothing
    /// pending is just a word.
    NothingPending,
}

// ── The registry ──────────────────────────────────────────────────────────────

/// Pending approvals, keyed by `(channel_id, user_id)`.
///
/// # Why the key includes the channel
///
/// A sender reachable on Discord and Slack can have one pending approval on each,
/// and a reply on one must not settle the other. The reply arrives with a channel
/// id, so keying on it is also what makes "only the originating sender can approve"
/// enforceable rather than aspirational.
///
/// # Not persisted, and not resumable
///
/// The registry is in-memory and a restart drops everything in it. That is the
/// intended behaviour, not a limitation to fix: an approval given before a restart
/// was given against caps and a policy that may since have changed, and silently
/// resuming would spend money on a plan nobody currently agrees to. A dropped
/// approval costs a re-ask; a resumed one costs money.
pub struct ApprovalRegistry {
    pending: Arc<Mutex<HashMap<(String, String), PendingApproval>>>,
    audit: Option<Arc<AuditLogger>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            audit: None,
        }
    }

    /// Attach an audit logger. Every decision is recorded, including timeouts.
    pub fn with_audit(mut self, audit: Option<Arc<AuditLogger>>) -> Self {
        self.audit = audit;
        self
    }

    /// How many approvals are currently outstanding. For tests and `/status`.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Whether this sender has an approval outstanding on this channel.
    ///
    /// Used by the message pipeline to exempt a reply from deduplication. An
    /// approval reply is one word, and a sender who approves two runs in a session
    /// sends the *same* word twice — which the dedup ring buffer would drop as a
    /// repeat, leaving the second run to expire unapproved with no visible cause.
    pub async fn has_pending(&self, user_id: &str, channel_id: &str) -> bool {
        self.pending
            .lock()
            .await
            .contains_key(&(channel_id.to_string(), user_id.to_string()))
    }

    /// Register an approval and wait for the sender to settle it.
    ///
    /// Returns when the sender answers, when `timeout` elapses, or when a newer
    /// request from the same sender supersedes this one. **Never returns
    /// [`Decision::Approved`] without an explicit positive reply.**
    ///
    /// Convenience for [`Self::begin`] followed by [`ApprovalTicket::wait`], for
    /// callers that have already sent the plan message. A caller that has *not* sent
    /// it yet must use [`Self::begin`] instead — see there for the race.
    pub async fn await_decision(
        &self,
        request_id: &str,
        user_id: &str,
        channel_id: &str,
        caps: ApprovedCaps,
        timeout: Duration,
    ) -> Decision {
        self.begin(request_id, user_id, channel_id, caps, timeout)
            .await
            .wait()
            .await
    }

    /// Register an approval, returning a ticket to wait on.
    ///
    /// Registration and waiting are separable because the plan message has to be
    /// sent *between* them. Sending first and registering after loses a reply that
    /// arrives in the gap — on a fast channel with a fast sender that is a run that
    /// expires despite having been approved, with nothing in the log to explain it.
    /// Registering first means the earliest possible reply already has somewhere to
    /// land.
    ///
    /// The plan message is the caller's job, not this module's, so that a send
    /// failure surfaces as a send failure rather than as a mysterious timeout.
    pub async fn begin(
        &self,
        request_id: &str,
        user_id: &str,
        channel_id: &str,
        caps: ApprovedCaps,
        timeout: Duration,
    ) -> ApprovalTicket {
        let (tx, rx) = oneshot::channel();
        let key = (channel_id.to_string(), user_id.to_string());

        {
            let mut pending = self.pending.lock().await;
            // Supersede semantics, stated: a second request from the same sender on
            // the same channel replaces the first, and the first is settled as
            // `Superseded` rather than left to time out. Refusing the new request
            // instead would let one un-answered approval block a sender until it
            // expired — and the likeliest reason for a second request is that they
            // want the second one.
            if let Some(mut old) = pending.remove(&key) {
                if let Some(settle) = old.settle.take() {
                    let _ = settle.send(Decision::Superseded);
                }
                self.log(
                    &old.request_id,
                    &old.user_id,
                    &old.channel_id,
                    Decision::Superseded,
                    &old.caps,
                );
            }
            pending.insert(
                key.clone(),
                PendingApproval {
                    request_id: request_id.to_string(),
                    user_id: user_id.to_string(),
                    channel_id: channel_id.to_string(),
                    settle: Some(tx),
                    deadline: Instant::now() + timeout,
                    caps: caps.clone(),
                },
            );
        }

        ApprovalTicket {
            pending: Arc::clone(&self.pending),
            audit: self.audit.clone(),
            request_id: request_id.to_string(),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            caps,
            timeout,
            settled: rx,
        }
    }

    /// Offer an inbound message as a reply to whatever that sender has pending.
    ///
    /// Returns [`ReplyOutcome::NothingPending`] when the sender has no pending
    /// approval, in which case the caller must process the message normally.
    pub async fn submit_reply(&self, user_id: &str, channel_id: &str, text: &str) -> ReplyOutcome {
        let key = (channel_id.to_string(), user_id.to_string());
        let mut pending = self.pending.lock().await;

        let entry = match pending.get_mut(&key) {
            Some(e) => e,
            None => return ReplyOutcome::NothingPending,
        };

        // Belt and braces: the key already contains both identities, so this can
        // only differ if a caller constructed the key from something other than the
        // message. Cheap to check, and the thing being protected is another user's
        // money.
        if entry.user_id != user_id || entry.channel_id != channel_id {
            return ReplyOutcome::NothingPending;
        }

        // An expired approval that has not yet been reaped must not be settleable.
        // Without this, a reply arriving in the gap between the deadline and the
        // waiting task's wake-up would approve a run the sender has already been
        // told expired.
        if Instant::now() >= entry.deadline {
            return ReplyOutcome::NothingPending;
        }

        let request_id = entry.request_id.clone();
        match classify_reply(text) {
            Reply::Unrecognised => ReplyOutcome::NeedsClarification { request_id },
            reply => {
                let decision = if reply == Reply::Approve {
                    Decision::Approved
                } else {
                    Decision::Cancelled
                };
                if let Some(settle) = entry.settle.take() {
                    let _ = settle.send(decision);
                }
                // Left in the map for `await_decision` to clear, so the request_id
                // check there stays meaningful.
                ReplyOutcome::Settled {
                    request_id,
                    decision,
                }
            }
        }
    }

    fn log(
        &self,
        request_id: &str,
        user_id: &str,
        channel_id: &str,
        decision: Decision,
        caps: &ApprovedCaps,
    ) {
        log_decision(
            self.audit.as_ref(),
            request_id,
            user_id,
            channel_id,
            decision,
            caps,
        );
    }
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn log_decision(
    audit: Option<&Arc<AuditLogger>>,
    request_id: &str,
    user_id: &str,
    channel_id: &str,
    decision: Decision,
    caps: &ApprovedCaps,
) {
    if let Some(al) = audit {
        al.log(AuditEvent::QuarryPlanDecision {
            request_id: request_id.to_string(),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            decision: decision.code().to_string(),
            spend_micro_usd: caps.spend_micro_usd,
            latency_seconds: caps.latency_seconds,
            due: caps.due.clone(),
        });
    }
}

// ── The ticket ────────────────────────────────────────────────────────────────

/// A registered approval, not yet settled.
///
/// Exists so the caller can send the plan message after the approval is
/// answerable — see [`ApprovalRegistry::begin`]. Dropping a ticket without calling
/// [`Self::wait`] leaves the entry in the map until it expires; that is a leak of
/// one small record for at most the timeout, and it is the safe direction, since
/// the alternative (settling on drop) would need a decision and none of them is
/// right.
pub struct ApprovalTicket {
    pending: Arc<Mutex<HashMap<(String, String), PendingApproval>>>,
    audit: Option<Arc<AuditLogger>>,
    request_id: String,
    user_id: String,
    channel_id: String,
    caps: ApprovedCaps,
    timeout: Duration,
    settled: oneshot::Receiver<Decision>,
}

impl ApprovalTicket {
    /// The request id this ticket registered, for correlating log lines.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Wait for the sender, the clock, or a superseding request.
    ///
    /// **Never returns [`Decision::Approved`] without an explicit positive reply.**
    pub async fn wait(self) -> Decision {
        let key = (self.channel_id.clone(), self.user_id.clone());

        let decision = match tokio::time::timeout(self.timeout, self.settled).await {
            Ok(Ok(d)) => d,
            // The sender-facing default. Both error paths land here: the timer
            // fired, or the registry entry was dropped without settling. Neither is
            // consent, and there is no configuration that makes either one consent.
            Ok(Err(_)) | Err(_) => Decision::Expired,
        };

        // Clear our own entry — but only if it is still ours. A superseding request
        // has already replaced it, and removing by key alone would delete the new
        // sender's pending approval while resolving the old one.
        {
            let mut pending = self.pending.lock().await;
            if pending.get(&key).map(|p| p.request_id.as_str()) == Some(self.request_id.as_str()) {
                pending.remove(&key);
            }
        }

        // A superseded approval was already logged by the request that replaced it.
        if decision != Decision::Superseded {
            log_decision(
                self.audit.as_ref(),
                &self.request_id,
                &self.user_id,
                &self.channel_id,
                decision,
                &self.caps,
            );
        }
        decision
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the plan message a sender approves or cancels.
///
/// Plain text with light markdown: it has to be readable on all twelve channels,
/// and Block Kit or Discord components would each work on exactly one. The caller
/// passes the result through the formatter and the chunker.
pub fn render_plan(disclosure: &PlanDisclosure) -> String {
    let mut out = String::new();
    out.push_str("**Before I spend anything — approve this run?**\n\n");
    out.push_str(&format!("> {}\n\n", disclosure.statement.trim()));

    out.push_str("**Limits in force**\n");
    let g = &disclosure.granted;
    if let Some(micro) = g.spend_micro_usd {
        out.push_str(&format!("• Spend: at most {}\n", render_spend(micro)));
    }
    if let Some(latency) = g.latency {
        out.push_str(&format!("• Time: at most {}\n", render_duration(latency)));
    }
    if let Some(due) = g.due {
        out.push_str(&format!("• Deadline: {}\n", due.to_rfc3339()));
    }
    // Under P4 the cap is the contract: quarry fits its plan to the cap rather than
    // discovering the cap partway through. Saying so is what makes gating on the
    // cap — rather than on an estimate — meaningful to the sender.
    out.push_str(
        "\nquarry plans to fit these limits, so the spend limit is the most this can cost.\n",
    );

    if !disclosure.adjustments.is_empty() {
        out.push_str("\n**I had to change what you asked for**\n");
        for adj in &disclosure.adjustments {
            out.push_str(&format!("• {}\n", adj.message()));
        }
    }

    if !disclosure.caps_disclosures.is_empty() {
        out.push_str("\n**Check these**\n");
        for d in &disclosure.caps_disclosures {
            out.push_str(&format!("• {}\n", render_caps_disclosure(d)));
        }
    }

    // Both absences are stated. A silent omission would read as "there is nothing
    // to exclude" and "the cost is negligible", neither of which is known.
    if let Err(reason) = &disclosure.estimate {
        out.push_str(&format!("\n**No cost estimate:** {}.\n", reason.message()));
    }
    match &disclosure.exclusions {
        Ok(items) if !items.is_empty() => {
            out.push_str("\n**Will not be covered**\n");
            for item in items {
                out.push_str(&format!("• {item}\n"));
            }
        }
        Ok(_) => {}
        Err(reason) => {
            out.push_str(&format!(
                "**What it will skip is also not known yet:** {}.\n",
                reason.message()
            ));
        }
    }

    out.push_str(&format!(
        "\nThis offer expires in {}, and expiring cancels it — I will not run \
         anything unless you say so.\n{}",
        render_duration(disclosure.expires_in),
        vocabulary_hint()
    ));
    out
}

/// The re-prompt for a reply that was neither yes nor no.
pub fn render_clarification() -> String {
    format!(
        "I did not read that as a yes or a no, so **nothing has run and nothing has \
         been spent**. {}",
        vocabulary_hint()
    )
}

/// The message sent when an approval expires unanswered.
pub fn render_expired() -> String {
    "That run expired without approval, so I cancelled it. **Nothing was spent.** \
     Ask again if you still want it."
        .to_string()
}

/// The message sent when the sender cancels.
pub fn render_cancelled() -> String {
    "Cancelled. **Nothing was spent.**".to_string()
}

/// The message sent when a newer request replaces a pending one.
pub fn render_superseded() -> String {
    "Replacing your previous request, which I cancelled without spending anything.".to_string()
}

fn render_spend(micro: i64) -> String {
    if micro < 0 {
        return "no limit".to_string();
    }
    format!("${:.4}", micro as f64 / 1_000_000.0)
}

fn render_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

fn render_caps_disclosure(d: &crate::quarry::Disclosure) -> String {
    use crate::quarry::Disclosure as D;
    match d {
        D::DueHasNoUpstreamFlag {
            resolved,
            equivalent_latency,
        } => format!(
            "Your deadline ({}) becomes a {} time limit — quarry has no deadline flag yet, \
             so this run cannot use the cheaper off-peak pricing a deadline would allow.",
            resolved.to_rfc3339(),
            render_duration(*equivalent_latency)
        ),
        D::DeadlineResolvedIn {
            timezone,
            source,
            local,
        } => {
            let why = match source {
                crate::quarry::TimezoneSource::SenderPreference => "your saved timezone",
                crate::quarry::TimezoneSource::ConfigDefault => "this server's default timezone",
                crate::quarry::TimezoneSource::UtcFallback => {
                    "UTC, because no timezone is set for you"
                }
            };
            format!("Deadline read as {local} ({timezone}) — {why}.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarry::ScopeTags;
    use std::collections::BTreeMap;

    fn scope() -> ScopeTags {
        ScopeTags::mint(BTreeMap::from([
            ("user".to_string(), "alice".to_string()),
            ("channel".to_string(), "discord-1".to_string()),
        ]))
        .expect("mint")
    }

    fn grant() -> Grant {
        Grant {
            spend_micro_usd: Some(5_000_000),
            latency: None,
            due: None,
            scope: scope(),
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

    // ── The reply vocabulary ─────────────────────────────────────────────────

    #[test]
    fn the_documented_vocabulary_is_accepted_in_any_case_or_punctuation() {
        for word in ["yes", "YES", " y ", "Approve", "ok!", "okay", "go", "run"] {
            assert_eq!(classify_reply(word), Reply::Approve, "{word:?}");
        }
        for word in ["no", "N", "cancel", "STOP.", "abort", "nope", "nevermind"] {
            assert_eq!(classify_reply(word), Reply::Cancel, "{word:?}");
        }
    }

    /// An unrecognised reply is neither answer.
    ///
    /// The failure this prevents is the tempting `contains("yes")` implementation:
    /// "no, wait — yes I mean cheaper" contains both words, and treating it as
    /// consent spends money on caps the sender was arguing with.
    #[test]
    fn an_unrecognised_reply_is_neither_approval_nor_cancellation() {
        for text in [
            "maybe",
            "how much will that cost?",
            "yes but only $2",
            "no, wait — yes",
            "yesterday",
            "",
            "   ",
            "sure why not",
            "👍",
        ] {
            assert_eq!(classify_reply(text), Reply::Unrecognised, "{text:?}");
        }
    }

    // ── Silence is not consent ───────────────────────────────────────────────

    /// A timeout cancels, and the run is told so.
    #[tokio::test]
    async fn silence_expires_the_approval_rather_than_granting_it() {
        let reg = ApprovalRegistry::new();
        let decision = reg
            .await_decision(
                "r1",
                "alice",
                "discord-1",
                ApprovedCaps::from_grant(&grant()),
                Duration::from_millis(30),
            )
            .await;
        assert_eq!(decision, Decision::Expired);
        assert!(!decision.may_spend(), "an expired approval must not spend");
        assert_eq!(reg.pending_count().await, 0, "the entry must be reaped");
    }

    /// Only `Approved` permits spend. Guards against a future variant defaulting
    /// into the permitted set.
    #[test]
    fn only_an_explicit_approval_permits_spend() {
        assert!(Decision::Approved.may_spend());
        for d in [Decision::Cancelled, Decision::Expired, Decision::Superseded] {
            assert!(!d.may_spend(), "{d:?} must not permit spend");
        }
    }

    #[tokio::test]
    async fn an_explicit_yes_approves_and_an_explicit_no_cancels() {
        for (reply, expected) in [("yes", Decision::Approved), ("no", Decision::Cancelled)] {
            let reg = Arc::new(ApprovalRegistry::new());
            let r = reg.clone();
            let waiter = tokio::spawn(async move {
                r.await_decision(
                    "r1",
                    "alice",
                    "discord-1",
                    ApprovedCaps::from_grant(&grant()),
                    Duration::from_secs(5),
                )
                .await
            });

            // Wait for the registration to land rather than sleeping a fixed amount.
            while reg.pending_count().await == 0 {
                tokio::task::yield_now().await;
            }

            let outcome = reg.submit_reply("alice", "discord-1", reply).await;
            assert_eq!(
                outcome,
                ReplyOutcome::Settled {
                    request_id: "r1".to_string(),
                    decision: expected
                }
            );
            assert_eq!(waiter.await.unwrap(), expected);
            assert_eq!(reg.pending_count().await, 0);
        }
    }

    /// An unrecognised reply leaves the approval pending and does not extend it.
    #[tokio::test]
    async fn an_unrecognised_reply_reprompts_without_settling_or_extending() {
        let reg = Arc::new(ApprovalRegistry::new());
        let r = reg.clone();
        let waiter = tokio::spawn(async move {
            r.await_decision(
                "r1",
                "alice",
                "discord-1",
                ApprovedCaps::from_grant(&grant()),
                Duration::from_millis(120),
            )
            .await
        });
        while reg.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }

        for _ in 0..3 {
            assert_eq!(
                reg.submit_reply("alice", "discord-1", "how much?").await,
                ReplyOutcome::NeedsClarification {
                    request_id: "r1".to_string()
                }
            );
        }

        // Still pending, and the original deadline still governs: three
        // clarifications did not buy an extension.
        assert_eq!(reg.pending_count().await, 1);
        assert_eq!(waiter.await.unwrap(), Decision::Expired);
    }

    // ── Only the originating sender ──────────────────────────────────────────

    /// Another user in the same channel cannot approve someone else's run.
    ///
    /// A group Discord channel is the normal case, not the edge case: everyone in
    /// it sees the plan message, so "reply yes" is an instruction a bystander can
    /// follow, against someone else's budget.
    #[tokio::test]
    async fn a_bystander_in_the_same_channel_cannot_approve() {
        let reg = Arc::new(ApprovalRegistry::new());
        let r = reg.clone();
        let waiter = tokio::spawn(async move {
            r.await_decision(
                "r1",
                "alice",
                "discord-1",
                ApprovedCaps::from_grant(&grant()),
                Duration::from_millis(120),
            )
            .await
        });
        while reg.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }

        // Mallory, in the same channel, says yes.
        assert_eq!(
            reg.submit_reply("mallory", "discord-1", "yes").await,
            ReplyOutcome::NothingPending,
            "a bystander's reply must not be treated as a reply to alice's approval"
        );

        // Alice's approval is untouched, and expires unapproved.
        assert_eq!(reg.pending_count().await, 1);
        let decision = waiter.await.unwrap();
        assert_eq!(decision, Decision::Expired);
        assert!(!decision.may_spend());
    }

    /// The same user replying on a different channel does not settle it either.
    #[tokio::test]
    async fn a_reply_on_another_channel_does_not_settle_this_one() {
        let reg = Arc::new(ApprovalRegistry::new());
        let r = reg.clone();
        let waiter = tokio::spawn(async move {
            r.await_decision(
                "r1",
                "alice",
                "discord-1",
                ApprovedCaps::from_grant(&grant()),
                Duration::from_millis(120),
            )
            .await
        });
        while reg.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }

        assert_eq!(
            reg.submit_reply("alice", "slack-1", "yes").await,
            ReplyOutcome::NothingPending
        );
        assert_eq!(waiter.await.unwrap(), Decision::Expired);
    }

    // ── Concurrency ─────────────────────────────────────────────────────────

    /// Two senders' approvals are independent, and each reply settles only its own.
    #[tokio::test]
    async fn concurrent_approvals_for_different_senders_do_not_interfere() {
        let reg = Arc::new(ApprovalRegistry::new());

        let a = {
            let r = reg.clone();
            tokio::spawn(async move {
                r.await_decision(
                    "ra",
                    "alice",
                    "discord-1",
                    ApprovedCaps::from_grant(&grant()),
                    Duration::from_secs(5),
                )
                .await
            })
        };
        let b = {
            let r = reg.clone();
            tokio::spawn(async move {
                r.await_decision(
                    "rb",
                    "bob",
                    "discord-1",
                    ApprovedCaps::from_grant(&grant()),
                    Duration::from_secs(5),
                )
                .await
            })
        };
        while reg.pending_count().await < 2 {
            tokio::task::yield_now().await;
        }

        reg.submit_reply("alice", "discord-1", "yes").await;
        reg.submit_reply("bob", "discord-1", "no").await;

        assert_eq!(a.await.unwrap(), Decision::Approved);
        assert_eq!(b.await.unwrap(), Decision::Cancelled);
    }

    /// A second request from the same sender supersedes the first — the documented
    /// semantics — and the first is settled rather than left dangling.
    #[tokio::test]
    async fn a_second_request_supersedes_the_first_without_spending() {
        let reg = Arc::new(ApprovalRegistry::new());

        let first = {
            let r = reg.clone();
            tokio::spawn(async move {
                r.await_decision(
                    "r1",
                    "alice",
                    "discord-1",
                    ApprovedCaps::from_grant(&grant()),
                    Duration::from_secs(5),
                )
                .await
            })
        };
        while reg.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }

        let second = {
            let r = reg.clone();
            tokio::spawn(async move {
                r.await_decision(
                    "r2",
                    "alice",
                    "discord-1",
                    ApprovedCaps::from_grant(&grant()),
                    Duration::from_secs(5),
                )
                .await
            })
        };

        let first_decision = first.await.unwrap();
        assert_eq!(first_decision, Decision::Superseded);
        assert!(
            !first_decision.may_spend(),
            "a superseded request must not spend"
        );

        // The reply settles the *second* request, not the first.
        while reg.pending_count().await == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            reg.submit_reply("alice", "discord-1", "yes").await,
            ReplyOutcome::Settled {
                request_id: "r2".to_string(),
                decision: Decision::Approved
            }
        );
        assert_eq!(second.await.unwrap(), Decision::Approved);
    }

    /// A reply arriving after the deadline but before the reaper cannot approve.
    #[tokio::test]
    async fn a_reply_after_the_deadline_cannot_approve() {
        let reg = ApprovalRegistry::new();
        // Register directly with an already-expired deadline: this is the state the
        // registry is in between the deadline passing and the waiting task waking.
        {
            let (tx, _rx) = oneshot::channel();
            reg.pending.lock().await.insert(
                ("discord-1".to_string(), "alice".to_string()),
                PendingApproval {
                    request_id: "r1".to_string(),
                    user_id: "alice".to_string(),
                    channel_id: "discord-1".to_string(),
                    settle: Some(tx),
                    deadline: Instant::now() - Duration::from_secs(1),
                    caps: ApprovedCaps::from_grant(&grant()),
                },
            );
        }
        assert_eq!(
            reg.submit_reply("alice", "discord-1", "yes").await,
            ReplyOutcome::NothingPending,
            "an expired approval must not be settleable by a late yes"
        );
    }

    /// A stray "yes" with nothing pending is an ordinary message.
    ///
    /// Returning anything else would make the registry swallow it, and the sender's
    /// message would vanish with no reply.
    #[tokio::test]
    async fn a_reply_with_nothing_pending_is_an_ordinary_message() {
        let reg = ApprovalRegistry::new();
        assert_eq!(
            reg.submit_reply("alice", "discord-1", "yes").await,
            ReplyOutcome::NothingPending
        );
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    #[test]
    fn the_plan_states_the_caps_and_that_expiry_cancels() {
        let text = render_plan(&disclosure());
        assert!(
            text.contains("$5.0000"),
            "the spend cap must be stated: {text}"
        );
        assert!(
            text.contains("expires in 5m") && text.contains("expiring cancels it"),
            "the sender must be told how long they have and that silence cancels: {text}"
        );
        assert!(
            text.to_lowercase().contains("yes") && text.to_lowercase().contains("cancel"),
            "the accepted vocabulary must be shown: {text}"
        );
    }

    /// An absent estimate says so, with the reason, and never renders as zero.
    ///
    /// "$0.00" would be approved against a measurement nobody took.
    #[test]
    fn an_unavailable_estimate_is_stated_as_unavailable_not_as_zero() {
        let text = render_plan(&disclosure());
        assert!(text.contains("No cost estimate"), "{text}");
        assert!(
            text.contains("estimating the cost would itself cost money"),
            "the reason must be given: {text}"
        );
        assert!(
            !text.contains("$0.00") && !text.contains("estimated cost: $0"),
            "an absent estimate must never render as a zero: {text}"
        );
    }

    /// Policy adjustments are restated at the gate, where they are still free to fix.
    #[test]
    fn a_reduced_cap_is_disclosed_in_the_plan() {
        use crate::quarry::Denomination;
        let mut d = disclosure();
        d.adjustments = vec![CapAdjustment::Reduced {
            denomination: Denomination::Spend,
            requested: "$50.0000".to_string(),
            granted: "$5.0000".to_string(),
        }];
        let text = render_plan(&d);
        assert!(text.contains("change what you asked for"), "{text}");
        assert!(
            text.contains("$50.0000") && text.contains("$5.0000"),
            "{text}"
        );
    }

    /// The resolved deadline and its timezone source reach the sender before spend.
    ///
    /// This is the acceptance criterion that a timezone misread from caps parsing
    /// is caught *here* — the last point before money moves.
    #[test]
    fn the_resolved_deadline_and_its_timezone_are_shown_before_spend() {
        let mut d = disclosure();
        d.caps_disclosures = vec![crate::quarry::Disclosure::DeadlineResolvedIn {
            timezone: "America/New_York".to_string(),
            source: crate::quarry::TimezoneSource::UtcFallback,
            local: "2026-08-04 17:00".to_string(),
        }];
        let text = render_plan(&d);
        assert!(text.contains("2026-08-04 17:00"), "{text}");
        assert!(
            text.contains("no timezone is set for you"),
            "a UTC fallback must say so — only the sender can tell it is wrong: {text}"
        );
    }

    /// Every plan message survives the tightest platform limit intact.
    ///
    /// Teams' limit is 1024 bytes. A plan that overflows must chunk rather than
    /// truncate, and the approval instruction must survive — a chunked plan whose
    /// last chunk was dropped leaves the sender without the vocabulary to answer.
    #[test]
    fn a_long_plan_chunks_for_teams_without_losing_the_instruction() {
        use crate::gateway::chunker::MessageChunker;
        let mut d = disclosure();
        d.statement = "why ".repeat(400);
        let text = render_plan(&d);
        assert!(text.len() > 1024, "the fixture must actually overflow");

        let chunks = MessageChunker::new(std::collections::HashMap::new()).chunk("teams-1", &text);
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.len() <= 1024, "chunk over the Teams limit: {}", c.len());
        }
        let rejoined = chunks.join(" ");
        assert!(
            rejoined.contains("cancel"),
            "the reply vocabulary must survive chunking: {rejoined}"
        );
    }

    #[test]
    fn decision_codes_are_unique_and_stable() {
        let codes: Vec<&str> = [
            Decision::Approved,
            Decision::Cancelled,
            Decision::Expired,
            Decision::Superseded,
        ]
        .iter()
        .map(|d| d.code())
        .collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "codes must be unique: {codes:?}");
    }

    #[test]
    fn unavailable_codes_are_unique() {
        assert_ne!(
            Unavailable::NoUpstreamPlanMode.code(),
            Unavailable::EstimateWouldItselfSpend.code()
        );
    }
}
