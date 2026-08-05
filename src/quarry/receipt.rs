//! The reply a quarry run produces: the answer, and the receipt that goes with it.
//!
//! # The receipt is not optional and there is no flag to remove it
//!
//! An answer with no cost and no trust information attached is exactly the artifact
//! quarry exists to replace. So there is no `quarry.receipt.enabled`, no per-channel
//! exemption, and no length threshold past which the footer is dropped: a reply too
//! long for a platform is *chunked* by [`crate::gateway::chunker`], never truncated
//! to fit by discarding the part that says what it cost.
//!
//! The one thing that can be absent is a **figure quarry did not report**, and in
//! that case the footer says so in words. See the module's treatment of stability
//! below — "not measured" and "0% stable" are different claims and only one of them
//! is ever true here.
//!
//! # Every figure is read, none is re-derived
//!
//! quarry's terminal outcome event carries its own verdict: the classification, which
//! cap bit, and the gap and unfunded counts as separate integers. All of it computed by
//! `Classify()`, the same call that produced the process exit code. This module
//! reads those fields and does no arithmetic that could disagree with them.
//!
//! The record is corroboration and detail — it can name *which* nodes gapped, which
//! two integers cannot — but it is never the source of the verdict. Deriving
//! `Truncated()` here when quarry already sent us its answer would produce two
//! verdicts with no way to tell which was right.
//!
//! # Gaps and unfunded are never added together
//!
//! Only **time** produces a gap. A node the spend cap priced out has empty content
//! and no gap flag, because being priced out is planned degradation inside the
//! authority the sender granted (P9: it was disclosed before spend). quarry keeps
//! `ErrRecordedGap` and `ErrRecordedUnfunded` as separate sentinels for this reason,
//! and the footer keeps them as separate, differently-labelled sections.
//!
//! The failure mode is concrete: a sender shown one "2 nodes incomplete" line raises
//! whichever cap comes to mind. Raising the wrong one buys nothing — more money does
//! not buy back a node the clock cut off, and more time does not fund one the budget
//! could not afford.
//!
//! # The per-model breakdown is omitted, and the reason is upstream
//!
//! quarry's `ModelEvent` costs do not sum to the run total, and cannot today.
//! `executor.go`'s reduce path assigns `Cost` but never `ModelVersion`, so a reduce
//! node is itemised in the receipt and appears in no model event. On the
//! `live-partition` fixture — a real Bedrock run — that residual is 42042 of 80437
//! micro-units, **over half the run**.
//!
//! That is quarry#20. The fix changes what a `NodeOutcome` hashes to, so it is not a
//! patch a host can front-run, and upstream is explicit: *"do not close the gap by
//! inventing an untagged row."* Printing per-model spend beside a total would show a
//! sender two numbers that visibly do not tie, and silently making them tie means
//! fabricating a row. So the footer states total spend, and names the residual only
//! when one exists — see [`Receipt::model_spend_residual_micro_usd`].

use super::event::{ArtifactEvent, ModelEvent, Provenance, ReceiptEvent, RunEvent};
use super::supervisor::{RunOutcome, Termination};
use std::time::Duration;

// ── Stability ─────────────────────────────────────────────────────────────────

/// What can honestly be said about whether this run's claims replicate.
///
/// # Three-state, because a float cannot carry the third state
///
/// agate's schema declares `stability` non-nullable, so quarry has no in-band way to
/// send "not measured": its `StabilityKnown` flag is `json:"-"` and never reaches
/// the wire. quarry's workaround is to **omit the whole `provenance` object** when
/// the rate is unpublishable, which it is in three distinct cases:
///
/// - a single run — a stable-claim fraction is not defined for n=1 (P7: one run is
///   one sample, not a distribution);
/// - a rate of 0 reached with unassessed comparisons — "nobody could tell", not
///   "nothing replicated";
/// - a truncated comparison pass — the clustering is admittedly incomplete, so every
///   number derived from it is provisional.
///
/// All three would render as `0.0` and all three would be badged "nothing
/// replicated", which is silence converted into a finding. So a missing provenance
/// object becomes [`Self::NotMeasured`] here and is rendered in words, never as a
/// number.
#[derive(Debug, Clone, PartialEq)]
pub enum Stability {
    /// quarry declined to publish a rate. **Not zero.**
    ///
    /// The single most common case for a host: every ordinary `quarry run` is one
    /// run, and `cmd/quarry/run.go` calls `ProvenanceOf(rec, nil)`, so nothing but a
    /// replicate pass can produce a publishable rate at all.
    NotMeasured,
    /// quarry published a rate, 0..1.
    ///
    /// **Still not straightforwardly a measurement.** `StabilityIsFloor`,
    /// `Unassessed` and `ComparedBy` are all `json:"-"`, so a floor derived from
    /// unassessed comparisons and a real measured rate reach a host as the *same
    /// bare float*. A published rate is therefore a **lower bound at best**, and the
    /// renderer says "at least", never "exactly" — the honest reading of a number
    /// whose provenance was stripped at the wire.
    Published(f64),
}

impl Stability {
    /// Read from the artifact event's optional provenance.
    ///
    /// The `Option` is the whole signal: quarry omits the object rather than sending
    /// a zero, so `None` is [`Self::NotMeasured`] and there is no threshold, no
    /// epsilon, and no zero-check involved in the decision.
    pub fn from_provenance(prov: Option<&Provenance>) -> Self {
        match prov {
            None => Self::NotMeasured,
            Some(p) => Self::Published(p.stability),
        }
    }

    /// One line for the footer.
    ///
    /// The unmeasured branch carries quarry's own reason. A bare "not measured" reads
    /// as an omission the host could have avoided; naming P7 says it is a property of
    /// having run once.
    pub fn render(&self) -> String {
        match self {
            Self::NotMeasured => "Stability: not measured — one run is one sample, not a \
                 distribution (quarry P7). This is not 0%."
                .to_string(),
            // "At least" rather than a bare percentage: the floor flag is `json:"-"`,
            // so a floor and a measurement are indistinguishable from here. Claiming
            // the weaker of the two readings is the only one guaranteed true.
            Self::Published(rate) => format!(
                "Stability: at least {:.0}% of claims replicated.",
                rate * 100.0
            ),
        }
    }
}

// ── The receipt ───────────────────────────────────────────────────────────────

/// Everything the footer states, extracted from one run.
///
/// Built by [`Receipt::from_outcome`] and rendered by [`Receipt::render`]. Split in
/// two so a test can assert a figure without parsing prose, and so the dashboard can
/// use the same extraction the chat reply uses.
#[derive(Debug, Clone)]
pub struct Receipt {
    /// The root answer, when the run produced one.
    ///
    /// `None` is a real outcome, not a rendering failure: quarry omits the answer
    /// event entirely rather than emitting an empty string, so a run that could not
    /// answer still gets a reply — with a receipt saying what it cost to find that
    /// out.
    pub answer: Option<String>,
    /// Total spend in int64 micro-dollars, from the terminal event's `total_micros`.
    ///
    /// quarry's ledger integers, not a sum of the receipt's float rows. `None` when
    /// no terminal event arrived, in which case the run was killed and the figure is
    /// genuinely unknown rather than zero.
    pub spend_micro_usd: Option<i64>,
    /// The spend cap in force, from the record.
    pub cap_micro_usd: Option<i64>,
    /// Whether the cap in force was explicitly unlimited.
    pub cap_unlimited: bool,
    /// The latency cap in force, from the record.
    pub latency_cap: Option<Duration>,
    /// The deadline in force, from the record, as quarry wrote it.
    pub due: Option<String>,
    /// Which denomination actually bound the run: `spend`, `latency`, `due`, or
    /// `None` for none.
    ///
    /// `None` here is quarry's empty string, which is a **measurement** — no cap bound
    /// this run — and is reported as such rather than omitted.
    pub bound_by: Option<String>,
    /// Nodes **time** cut short. Never added to [`Self::unfunded`].
    pub gaps: u64,
    /// Nodes the spend cap priced out. Not gaps.
    pub unfunded: u64,
    /// The node IDs that gapped, when the record could be read.
    ///
    /// The detail the two integers cannot carry. Empty when the record is absent,
    /// which is why [`Self::gaps`] is the count that gets rendered and this only adds
    /// names to it.
    pub gap_nodes: Vec<String>,
    /// Node IDs no verifier assessed, from the record's own list.
    pub unverified_nodes: Vec<String>,
    /// What can be said about replication.
    pub stability: Stability,
    /// Claims an adversarial pass refuted, when provenance was published.
    pub adversarial_findings: Option<u64>,
    /// quarry's content-hash run ID, and where the record is retrievable.
    pub run_id: Option<String>,
    /// The record URL from the artifact event, when quarry made it addressable.
    pub record_url: Option<String>,
    /// The host's own path to the run directory, as a fallback citation.
    pub run_dir: String,
    /// Whether the itemised rows sum to the stated total.
    ///
    /// quarry's rule is that a receipt which does not add up is worse than no
    /// receipt, so a failure here is stated in the footer rather than swallowed.
    pub rows_reconcile: bool,
    /// Number of itemised rows.
    pub row_count: usize,
    /// Summed per-model spend, when any model event arrived.
    pub model_spend_micro_usd: Option<i64>,
    /// How the run ended, as the supervisor classified it.
    pub termination: Termination,
    /// Wall-clock duration.
    pub duration: Duration,
}

impl Receipt {
    /// Extract everything the footer needs from a finished run.
    pub fn from_outcome(outcome: &RunOutcome) -> Self {
        let terminal = super::event::terminal_outcome(&outcome.events);
        let artifact = find_artifact(&outcome.events);
        let receipt_event = find_receipt(&outcome.events);
        let models = collect_models(&outcome.events);

        let model_spend = if models.is_empty() {
            None
        } else {
            Some(models.iter().map(|m| m.cost_micro_usd()).sum())
        };

        Self {
            answer: outcome.answer().map(str::to_string),
            // Read from quarry's integer field, not from the float rows. This is the
            // one figure on the stream that never needed reconciling.
            spend_micro_usd: terminal.map(|t| t.total_micros),
            cap_micro_usd: outcome
                .record
                .as_ref()
                .and_then(|r| r.caps.spend_micro_usd()),
            cap_unlimited: outcome
                .record
                .as_ref()
                .is_some_and(|r| r.caps.spend_unlimited()),
            latency_cap: outcome.record.as_ref().and_then(|r| r.caps.latency()),
            due: outcome
                .record
                .as_ref()
                .and_then(|r| r.caps.due().map(str::to_string)),
            bound_by: terminal.and_then(|t| non_empty(&t.bound_by)),
            gaps: terminal.map(|t| t.gaps).unwrap_or(0),
            unfunded: terminal.map(|t| t.unfunded).unwrap_or(0),
            gap_nodes: outcome
                .record
                .as_ref()
                .map(|r| {
                    r.gaps()
                        .iter()
                        .map(|n| n.node_id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            unverified_nodes: outcome
                .record
                .as_ref()
                .map(|r| r.unverified.clone())
                .unwrap_or_default(),
            stability: Stability::from_provenance(artifact.and_then(|a| a.provenance.as_ref())),
            adversarial_findings: artifact
                .and_then(|a| a.provenance.as_ref())
                .map(|p| p.adversarial_findings),
            run_id: artifact.map(|a| a.run_id.clone()),
            record_url: artifact.and_then(|a| non_empty(&a.url)),
            run_dir: outcome.run_dir.display().to_string(),
            // An absent receipt event reconciles trivially: there is nothing claimed
            // that could fail to add up. The count below is what distinguishes the
            // two, and the footer never says "reconciles" about a receipt it lacks.
            rows_reconcile: receipt_event.map(|r| r.rows_reconcile()).unwrap_or(true),
            row_count: receipt_event.map(|r| r.rows.len()).unwrap_or(0),
            model_spend_micro_usd: model_spend,
            termination: outcome.termination.clone(),
            duration: outcome.duration,
        }
    }

    /// Spend the model events cannot account for, in micro-dollars.
    ///
    /// `None` when there is nothing to compare — no total, or no model events. `Some`
    /// only when a real residual exists.
    ///
    /// This is quarry#20 and it is **disclosed rather than reconciled**. Reduce nodes
    /// spend without a `ModelVersion`, so they are itemised in the receipt and appear
    /// in no model event; on a real 25-row Bedrock run the residual is over half the
    /// total. A host cannot fix it — the fix changes what a `NodeOutcome` hashes to —
    /// and upstream forbids the tempting workaround by name: *"do not close the gap
    /// by inventing an untagged row."*
    pub fn model_spend_residual_micro_usd(&self) -> Option<i64> {
        let (total, model) = (self.spend_micro_usd?, self.model_spend_micro_usd?);
        match total - model {
            0 => None,
            n => Some(n),
        }
    }

    /// Whether the run stopped short of what it set out to do.
    ///
    /// Read from quarry's own classification rather than re-derived. Deliberately
    /// broader than "has gaps": a run priced out of every child has no gaps at all
    /// while being the clearest case of one that did not finish.
    pub fn truncated(&self) -> bool {
        matches!(
            self.termination,
            Termination::Truncated { .. } | Termination::NoAnswer
        ) || self.gaps > 0
            || self.unfunded > 0
            || self.bound_by.is_some()
    }

    /// The complete reply: the answer, then the receipt.
    ///
    /// Always both. A run with no answer still gets a footer — that is the case where
    /// knowing what was spent to learn nothing matters most.
    pub fn render(&self) -> String {
        let mut out = String::new();

        match &self.answer {
            Some(text) => {
                out.push_str(text.trim_end());
                out.push_str("\n\n");
            }
            None => {
                // Stated before the receipt rather than left to be inferred from a
                // blank reply. The footer below then says what the attempt cost.
                out.push_str("**No answer.** This run produced nothing usable.\n\n");
            }
        }

        out.push_str("---\n");
        out.push_str(&self.render_footer());
        out
    }

    /// The receipt alone, without the answer above it.
    pub fn render_footer(&self) -> String {
        let mut out = String::new();
        out.push_str("**Receipt**\n");

        // ── Spend, against the cap that governed it ───────────────────────────
        //
        // Stated as "of" the cap because under P4 the cap is the contract: quarry
        // plans to fit it, so the pair is the whole story about the bill.
        match self.spend_micro_usd {
            Some(spend) => {
                let against = if self.cap_unlimited {
                    " of no limit".to_string()
                } else {
                    match self.cap_micro_usd {
                        Some(cap) => format!(" of {}", render_spend(cap)),
                        None => String::new(),
                    }
                };
                out.push_str(&format!("• Spend: {}{against}\n", render_spend(spend)));
            }
            // Absent, not zero. A killed run's spend is unknown and reporting it as
            // free is the one reading guaranteed to be wrong — the money went out.
            None => out.push_str(
                "• Spend: **not reported** — the run ended without stating its total, \
                 so this is unknown rather than zero.\n",
            ),
        }

        if let Some(residual) = self.model_spend_residual_micro_usd() {
            // Disclosed, never reconciled. See the method's docs: the itemised rows
            // and the per-model figures cannot tie until quarry#20 lands, and closing
            // the gap here would mean inventing a row upstream forbids.
            out.push_str(&format!(
                "• Of that, {} is not attributable to a specific model — quarry's \
                 reduce steps spend without recording a model version (quarry#20). \
                 The total above is still exact.\n",
                render_spend(residual)
            ));
        }

        out.push_str(&format!("• Took: {}\n", render_duration(self.duration)));

        // ── Which cap bit ────────────────────────────────────────────────────
        //
        // Named explicitly, because raising the wrong cap buys nothing. Both branches
        // are printed: "nothing bound this run" is a measurement quarry emits on
        // purpose, and omitting it would leave the sender unable to tell a clean run
        // from one whose footer forgot to mention a limit.
        match &self.bound_by {
            Some(denomination) => out.push_str(&format!(
                "• Stopped by: **{}** — {}\n",
                denomination,
                remedy_for(denomination)
            )),
            None => out.push_str("• No limit was reached.\n"),
        }

        // ── Gaps and unfunded: two sections, never one ───────────────────────
        //
        // Separate headings with separate remedies. Merging them into one
        // "incomplete" line is the error this whole module is shaped around.
        if self.gaps > 0 {
            out.push_str(&format!(
                "\n**Missing because time ran out** ({} {})\n",
                self.gaps,
                plural(self.gaps, "node", "nodes")
            ));
            if !self.gap_nodes.is_empty() {
                out.push_str(&format!("• Nodes: {}\n", self.gap_nodes.join(", ")));
            }
            out.push_str("• More time would recover these. More money would not.\n");
        }

        if self.unfunded > 0 {
            out.push_str(&format!(
                "\n**Not attempted, because the budget did not cover it** ({} {})\n",
                self.unfunded,
                plural(self.unfunded, "node", "nodes")
            ));
            out.push_str(
                "• This is planned degradation, not lost work: quarry fitted its plan \
                 to the limit you approved.\n• A higher spend limit would fund these. \
                 More time would not.\n",
            );
        }

        // ── Trust ────────────────────────────────────────────────────────────
        out.push_str("\n**How much to trust it**\n");
        out.push_str(&format!("• {}\n", self.stability.render()));
        if !self.unverified_nodes.is_empty() {
            out.push_str(&format!(
                "• Unverified: {} {} — {}\n",
                self.unverified_nodes.len(),
                plural(self.unverified_nodes.len() as u64, "node", "nodes"),
                self.unverified_nodes.join(", ")
            ));
        }
        if let Some(findings) = self.adversarial_findings {
            if findings > 0 {
                out.push_str(&format!(
                    "• An adversarial pass refuted {} {}.\n",
                    findings,
                    plural(findings, "claim", "claims")
                ));
            }
        }

        // A receipt that does not add up is worse than no receipt, so say it rather
        // than render the numbers as if they agreed.
        if self.row_count > 0 && !self.rows_reconcile {
            out.push_str(
                "• ⚠️ The itemised lines in quarry's receipt do not sum to its stated \
                 total. Treat the breakdown as unreliable; the total is quarry's own \
                 figure.\n",
            );
        }

        // ── How the run ended, when it was not cleanly ────────────────────────
        if let Some(note) = termination_note(&self.termination) {
            out.push_str(&format!("\n**{note}**\n"));
        }

        // ── The citation (P8) ────────────────────────────────────────────────
        out.push_str("\n**Full record**\n");
        if let Some(run_id) = &self.run_id {
            out.push_str(&format!("• Run: `{run_id}`\n"));
        }
        match &self.record_url {
            Some(url) => out.push_str(&format!("• Record: {url}\n")),
            // The gateway's own directory, when quarry did not make the record
            // addressable. Something citable beats nothing: P8 is byte-identical
            // replay, and replay needs a path.
            None => out.push_str(&format!("• Record directory: `{}`\n", self.run_dir)),
        }

        out
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// What raising this denomination would actually buy.
///
/// The point of naming `BoundBy` at all: a sender told only "ran out" raises
/// whichever cap comes to mind, and three of the four choices buy nothing.
fn remedy_for(denomination: &str) -> &'static str {
    match denomination {
        "spend" => "a higher spend limit would let it go further. More time would not.",
        "latency" => "more time would let it go further. More money would not.",
        "due" => "a later deadline would let it go further. More money would not.",
        // quarry's denomination vocabulary could grow, and guessing a remedy for a
        // denomination we do not know would send the sender to raise the wrong cap —
        // the exact failure this function exists to prevent. Say less instead.
        _ => "raising that specific limit is what would let it go further.",
    }
}

/// A short note about a termination that was not clean, or `None` when it was.
///
/// Each one says what the sender can do, because these are the cases where a
/// reply that only reports an internal state is useless to them.
fn termination_note(t: &Termination) -> Option<String> {
    match t {
        // Both of these are already fully described by the sections above: the
        // denomination that bit, the gap and unfunded counts, and the remedies.
        // Repeating them here would be a second, vaguer statement of the same facts.
        Termination::Completed | Termination::Truncated { .. } | Termination::NoAnswer => None,
        Termination::TimedOut { after } => Some(format!(
            "Cut off by this gateway after {} — not by quarry. \
             Anything above was already paid for.",
            render_duration(*after)
        )),
        Termination::Cancelled => {
            Some("Cancelled while running. Anything above was already paid for.".to_string())
        }
        Termination::Crashed { exit_code } => Some(format!(
            "quarry failed (exit {exit_code}). Anything above is what it had reached \
             before failing, and is still citable."
        )),
        Termination::KilledBySignal { signal } => Some(match signal {
            Some(sig) => format!("quarry was killed by signal {sig}. Anything above is partial."),
            None => "quarry was killed by a signal. Anything above is partial.".to_string(),
        }),
        Termination::StreamMalformed => Some(
            "quarry produced no readable events, so this receipt is incomplete. \
             The record on disk is still the citable artifact."
                .to_string(),
        ),
        Termination::StreamVersionUnsupported { declared } => Some(format!(
            "quarry declared event-stream version {declared}, which this gateway does \
             not understand, so its events were not read. The record on disk is \
             unaffected and still citable."
        )),
        Termination::StreamIncomplete { events_read } => Some(format!(
            "quarry's event stream stopped after {events_read} {} without a closing \
             event, which means the run was cut off. Anything above is partial and \
             the spend may be understated.",
            plural(*events_read as u64, "event", "events")
        )),
        Termination::UsageError => Some(
            "quarry rejected how this gateway invoked it, so nothing ran and nothing \
             was spent. This is a fault here, not in your request."
                .to_string(),
        ),
    }
}

fn find_artifact(events: &[RunEvent]) -> Option<&ArtifactEvent> {
    events.iter().find_map(|e| match e {
        RunEvent::Artifact(a) => Some(a),
        _ => None,
    })
}

fn find_receipt(events: &[RunEvent]) -> Option<&ReceiptEvent> {
    events.iter().find_map(|e| match e {
        RunEvent::Receipt(r) => Some(r),
        _ => None,
    })
}

fn collect_models(events: &[RunEvent]) -> Vec<&ModelEvent> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Model(m) => Some(m),
            _ => None,
        })
        .collect()
}

/// `Some` for a non-empty string, `None` otherwise.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Micro-dollars as a currency figure.
///
/// Four decimal places, matching the plan gate's rendering, so the cap a sender
/// approved and the spend they are billed appear in the same form. Sub-hundredth
/// amounts are normal here: a `--fake` run costs `$0.0002`.
fn render_spend(micro: i64) -> String {
    if micro < 0 {
        return "no limit".to_string();
    }
    format!("${:.4}", micro as f64 / 1_000_000.0)
}

fn render_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return format!("{}ms", d.as_millis());
    }
    if secs >= 3600 {
        return format!("{}h{}m", secs / 3600, (secs % 3600) / 60);
    }
    if secs >= 60 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    format!("{secs}s")
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        one.to_string()
    } else {
        many.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarry::event::{
        AnswerEvent, ModelEvent, NodeOutcomeSummary, OutcomeEvent, Provenance, ReceiptRow,
        RecordCaps, RunRecordSummary, StreamEvent,
    };
    use crate::quarry::StreamStats;
    use std::path::PathBuf;

    // ── Fixtures ─────────────────────────────────────────────────────────────

    fn outcome_with(events: Vec<RunEvent>, termination: Termination) -> RunOutcome {
        RunOutcome {
            run_id: "gw-run-1".to_string(),
            termination,
            events,
            stats: StreamStats::default(),
            stderr: String::new(),
            run_dir: PathBuf::from("/tmp/rn-quarry/gw-run-1"),
            record: None,
            duration: Duration::from_secs(3),
        }
    }

    fn frame(version: u32) -> RunEvent {
        RunEvent::Stream(StreamEvent {
            version,
            producer: "quarry-go".to_string(),
        })
    }

    fn answer(text: &str) -> RunEvent {
        RunEvent::Answer(AnswerEvent {
            title: String::new(),
            text: text.to_string(),
        })
    }

    fn receipt(rows: &[(&str, f64)], total: f64) -> RunEvent {
        RunEvent::Receipt(ReceiptEvent {
            rows: rows
                .iter()
                .map(|(label, cost)| ReceiptRow {
                    label: label.to_string(),
                    kind: "llm".to_string(),
                    cost: *cost,
                })
                .collect(),
            total,
        })
    }

    fn artifact(run_id: &str, url: &str, prov: Option<Provenance>) -> RunEvent {
        RunEvent::Artifact(ArtifactEvent {
            run_id: run_id.to_string(),
            url: url.to_string(),
            provenance: prov,
        })
    }

    fn terminal(outcome: &str, bound_by: &str, gaps: u64, unfunded: u64, total: i64) -> RunEvent {
        RunEvent::Outcome(OutcomeEvent {
            outcome: outcome.to_string(),
            bound_by: bound_by.to_string(),
            gaps,
            unfunded,
            total_micros: total,
            cap_micros: 250_000,
        })
    }

    /// The `complete` corpus case, as events.
    fn complete_run() -> RunOutcome {
        let mut o = outcome_with(
            vec![
                frame(1),
                RunEvent::Model(ModelEvent {
                    tier: "fake@fake".to_string(),
                    label: "fake@fake".to_string(),
                    state: "done".to_string(),
                    cost: 0.000218,
                }),
                answer("Storage costs scale with capacity."),
                receipt(
                    &[
                        ("n0.0 What does storage cost", 0.000076),
                        ("n0.1 how does it scale", 0.000068),
                        ("n0.2 and what dominates the bill?", 0.000074),
                    ],
                    0.000218,
                ),
                artifact("14e5b754", "file:///tmp/quarry-run-14e5b754.json", None),
                terminal("complete", "", 0, 0, 218),
            ],
            Termination::Completed,
        );
        o.record = Some(RunRecordSummary {
            run_id: "14e5b754".to_string(),
            bound_by: String::new(),
            caps: RecordCaps {
                spend: 250_000,
                latency_nanos: 0,
                due: "0001-01-01T00:00:00Z".to_string(),
            },
            unverified: vec!["n0".to_string()],
            outcomes: vec![],
        });
        o
    }

    fn gapped_node(id: &str) -> NodeOutcomeSummary {
        NodeOutcomeSummary {
            node_id: id.to_string(),
            gap: true,
            ..Default::default()
        }
    }

    // ── The receipt is never suppressed ──────────────────────────────────────

    /// The central invariant: there is no input for which the footer disappears.
    ///
    /// Asserted over every `Termination`, including the ones where the gateway has
    /// almost nothing to report, because "if it does not fit, chunk it — do not drop
    /// it" is only meaningful if no code path drops it first.
    #[test]
    fn every_termination_still_produces_a_receipt() {
        let terminations = vec![
            Termination::Completed,
            Termination::Truncated {
                bound_by: Some("latency".to_string()),
            },
            Termination::NoAnswer,
            Termination::TimedOut {
                after: Duration::from_secs(60),
            },
            Termination::Cancelled,
            Termination::Crashed { exit_code: 1 },
            Termination::KilledBySignal { signal: Some(9) },
            Termination::StreamMalformed,
            Termination::StreamVersionUnsupported { declared: 2 },
            Termination::StreamIncomplete { events_read: 3 },
            Termination::UsageError,
        ];
        for t in terminations {
            let o = outcome_with(vec![], t.clone());
            let rendered = Receipt::from_outcome(&o).render();
            assert!(
                rendered.contains("**Receipt**"),
                "termination {t:?} produced no receipt: {rendered}"
            );
            assert!(
                rendered.contains("Full record"),
                "termination {t:?} produced no citation: {rendered}"
            );
        }
    }

    /// A run with no answer is still a reply, and the receipt is why.
    #[test]
    fn a_run_with_no_answer_reports_what_it_spent_finding_that_out() {
        let o = outcome_with(
            vec![
                frame(1),
                receipt(&[], 0.0),
                artifact("abc", "", None),
                terminal("no-answer", "", 0, 1, 0),
            ],
            Termination::NoAnswer,
        );
        let rendered = Receipt::from_outcome(&o).render();
        assert!(rendered.contains("**No answer.**"));
        assert!(rendered.contains("**Receipt**"));
        // The unfunded node is why there was no answer, and it must be named as a
        // budget shortfall rather than as missing work.
        assert!(rendered.contains("budget did not cover it"));
    }

    // ── Gaps and unfunded are never merged ───────────────────────────────────

    /// The distinction the whole footer is shaped around.
    ///
    /// A run with both must show two sections with two different remedies. One
    /// combined "3 nodes incomplete" line would be true and useless.
    #[test]
    fn gaps_and_unfunded_are_separate_sections_with_opposite_remedies() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("partial"),
                receipt(&[("n0.0 a", 0.0001)], 0.0001),
                artifact("abc", "", None),
                terminal("time-truncated", "latency", 2, 1, 100),
            ],
            Termination::Truncated {
                bound_by: Some("latency".to_string()),
            },
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.gaps, 2);
        assert_eq!(r.unfunded, 1);

        let text = r.render();
        let time_section = text
            .find("Missing because time ran out")
            .expect("no time section");
        let money_section = text
            .find("budget did not cover it")
            .expect("no budget section");
        assert_ne!(time_section, money_section);

        // The counts are stated separately and their sum appears nowhere. `3` as a
        // node count is the specific wrong number a merging host would print.
        assert!(text.contains("(2 nodes)"), "{text}");
        assert!(text.contains("(1 node)"), "{text}");
        assert!(
            !text.contains("3 nodes"),
            "gaps and unfunded were summed: {text}"
        );

        // Opposite remedies, each attached to its own section.
        assert!(text.contains("More time would recover these. More money would not."));
        assert!(text.contains("A higher spend limit would fund these. More time would not."));
    }

    /// `no-answer-spend` from the corpus: zero gaps, one unfunded.
    ///
    /// A host that summed the denominations would report time pressure on a run where
    /// the clock was never involved, and offer more of the wrong thing.
    #[test]
    fn a_priced_out_run_is_never_described_as_having_run_out_of_time() {
        let o = outcome_with(
            vec![
                frame(1),
                receipt(&[], 0.0),
                artifact("abc", "", None),
                terminal("no-answer", "", 0, 1, 0),
            ],
            Termination::NoAnswer,
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(!text.contains("time ran out"), "{text}");
        assert!(text.contains("budget did not cover it"), "{text}");
    }

    /// `no-answer-time` from the corpus: gaps with `total_micros` of 0.
    ///
    /// The deadline bit before anything was spent, so this run has gaps *and* cost
    /// nothing — the pair that catches a host inferring one from the other.
    #[test]
    fn gaps_with_zero_spend_report_both_facts() {
        let o = outcome_with(
            vec![
                frame(1),
                receipt(&[], 0.0),
                artifact("abc", "", None),
                terminal("no-answer", "latency", 4, 0, 0),
            ],
            Termination::NoAnswer,
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("$0.0000"), "{text}");
        assert!(text.contains("(4 nodes)"), "{text}");
        assert!(text.contains("time ran out"), "{text}");
    }

    // ── BoundBy ──────────────────────────────────────────────────────────────

    /// Naming the denomination is the point; naming the remedy is why.
    #[test]
    fn each_denomination_names_the_cap_worth_raising() {
        for (denomination, expect) in [
            ("spend", "a higher spend limit"),
            ("latency", "more time"),
            ("due", "a later deadline"),
        ] {
            let o = outcome_with(
                vec![
                    frame(1),
                    answer("a"),
                    artifact("abc", "", None),
                    terminal("time-truncated", denomination, 1, 0, 100),
                ],
                Termination::Truncated {
                    bound_by: Some(denomination.to_string()),
                },
            );
            let text = Receipt::from_outcome(&o).render();
            assert!(
                text.contains(&format!("Stopped by: **{denomination}**")),
                "{text}"
            );
            assert!(text.contains(expect), "{denomination}: {text}");
        }
    }

    /// An empty `bound_by` is a measurement quarry emits deliberately, so it is
    /// reported rather than dropped.
    ///
    /// `cap-bound-degradation` carries `bound_by: ""` in the real corpus, so this is
    /// not a hypothetical shape.
    #[test]
    fn no_cap_bound_is_stated_rather_than_omitted() {
        let r = Receipt::from_outcome(&complete_run());
        assert_eq!(r.bound_by, None);
        assert!(r.render().contains("No limit was reached."));
    }

    /// A denomination quarry adds later must not get a fabricated remedy.
    #[test]
    fn an_unknown_denomination_does_not_invent_a_remedy() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                terminal("some-future-outcome", "tokens", 0, 0, 100),
            ],
            Termination::Completed,
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("Stopped by: **tokens**"), "{text}");
        // Naming a specific wrong cap is the failure. Neither of the two concrete
        // remedies may appear for a denomination we do not know.
        assert!(!text.contains("more time would let it"), "{text}");
        assert!(
            !text.contains("a higher spend limit would let it"),
            "{text}"
        );
    }

    // ── Stability's three zero-cases ─────────────────────────────────────────

    /// Absent provenance is "not measured", and the footer says so in words.
    #[test]
    fn absent_provenance_renders_as_not_measured_and_never_as_zero_percent() {
        let r = Receipt::from_outcome(&complete_run());
        assert_eq!(r.stability, Stability::NotMeasured);

        let text = r.render();
        assert!(text.contains("not measured"), "{text}");
        assert!(text.contains("P7"), "{text}");
        // The specific misrendering: three distinct quarry cases all reduce to 0.0,
        // and printing that number badges "nothing replicated" for a run where
        // nothing was ever measured.
        assert!(!text.contains("0% of claims"), "{text}");
    }

    /// A published rate is rendered as a floor, because the flags that would
    /// distinguish a floor from a measurement are `json:"-"`.
    #[test]
    fn a_published_rate_is_rendered_as_a_lower_bound_not_a_measurement() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                artifact(
                    "abc",
                    "",
                    Some(Provenance {
                        record_hash: "abc".to_string(),
                        verified: 3,
                        unverified: 1,
                        stability: 0.75,
                        adversarial_findings: 0,
                    }),
                ),
                terminal("complete", "", 0, 0, 100),
            ],
            Termination::Completed,
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("at least 75%"), "{text}");
        // "exactly" would be a claim the stream cannot support: StabilityIsFloor,
        // Unassessed and ComparedBy never reach a consumer, so a floor and a
        // measurement arrive as the same bare float.
        assert!(!text.contains("exactly"), "{text}");
    }

    /// A *published* zero is a real finding and is published as one — the narrow case
    /// quarry deliberately does not suppress.
    ///
    /// Distinguishing this from an absent provenance is the whole three-state point:
    /// a rate of 0 with nothing unassessed means the comparator was asked about every
    /// pair and no claim replicated.
    #[test]
    fn a_published_zero_is_distinguishable_from_an_absent_one() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                artifact(
                    "abc",
                    "",
                    Some(Provenance {
                        record_hash: "abc".to_string(),
                        verified: 0,
                        unverified: 0,
                        stability: 0.0,
                        adversarial_findings: 0,
                    }),
                ),
                terminal("complete", "", 0, 0, 100),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.stability, Stability::Published(0.0));

        let text = r.render();
        assert!(text.contains("at least 0%"), "{text}");
        assert!(!text.contains("not measured"), "{text}");
    }

    // ── Spend, the cap, and the residual ─────────────────────────────────────

    /// Spend is stated against the cap that governed it, per P4.
    #[test]
    fn spend_is_stated_against_the_cap_in_force() {
        let r = Receipt::from_outcome(&complete_run());
        assert_eq!(r.spend_micro_usd, Some(218));
        assert!(r.render().contains("$0.0002 of $0.2500"), "{}", r.render());
    }

    /// Spend is read from quarry's `total_micros`, not summed from the receipt rows.
    ///
    /// The two figures are separate fields that can legitimately disagree: the rows
    /// are `f64` and each rounds independently, and quarry's own corpus carries a
    /// `float_sum_equals_total: false` case. This asserts which field is authoritative
    /// by giving them *different* values — the `complete` fixture cannot, because its
    /// rows happen to sum to its total either way, so a host summing rows would pass a
    /// test written on that shape alone.
    #[test]
    fn spend_is_read_from_the_integer_total_not_summed_from_the_rows() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                // Rows summing to 100 micro-units against a stated total of 218.
                receipt(&[("n0.0 a", 0.00005), ("n0.1 b", 0.00005)], 0.000218),
                artifact("abc", "", None),
                terminal("complete", "", 0, 0, 218),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.spend_micro_usd, Some(218));
        assert!(r.render().contains("Spend: $0.0002"), "{}", r.render());
        // And the disagreement is itself surfaced rather than resolved silently.
        assert!(!r.rows_reconcile);
    }

    /// Per-row rounding loses money, which is the other reason the rows are not the
    /// total: 25 rows of a third of a micro-unit each round to 0 individually and to
    /// 8 collectively.
    #[test]
    fn summing_the_rows_would_lose_spend_that_rounds_away_per_row() {
        let rows: Vec<(&str, f64)> = (0..25).map(|_| ("n0.x", 0.00000033)).collect();
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                receipt(&rows, 0.00000825),
                artifact("abc", "", None),
                terminal("complete", "", 0, 0, 8),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.spend_micro_usd, Some(8));
        // Each row rounds to zero on its own, so a row-summing host reports a
        // 25-model run as free.
        let summed: i64 = find_receipt(&o.events).unwrap().rows_micro_usd();
        assert_eq!(summed, 0);
    }

    /// A missing terminal event means the spend is unknown, and unknown is not zero.
    #[test]
    fn a_killed_run_reports_spend_as_unknown_rather_than_free() {
        let o = outcome_with(
            vec![frame(1), answer("partial"), artifact("abc", "", None)],
            Termination::StreamIncomplete { events_read: 3 },
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.spend_micro_usd, None);

        let text = r.render();
        assert!(text.contains("not reported"), "{text}");
        assert!(text.contains("unknown rather than zero"), "{text}");
        // Rendering `$0.0000` for a run whose money already went out is the one
        // reading guaranteed to be false.
        assert!(!text.contains("Spend: $0.0000"), "{text}");
    }

    /// `deadline-only` from the corpus: `cap_micros` of `-1`.
    #[test]
    fn an_unlimited_cap_renders_as_no_limit_rather_than_as_a_cap_of_zero() {
        let mut o = complete_run();
        o.record.as_mut().unwrap().caps.spend = -1;
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("of no limit"), "{text}");
        assert!(!text.contains("of $0.0000"), "{text}");
    }

    /// `live-partition`: the residual is disclosed, and the total is not adjusted.
    #[test]
    fn the_unattributed_model_spend_is_disclosed_and_the_total_left_exact() {
        let o = outcome_with(
            vec![
                frame(1),
                RunEvent::Model(ModelEvent {
                    tier: "bedrock".to_string(),
                    label: "claude".to_string(),
                    state: "done".to_string(),
                    cost: 0.038395,
                }),
                answer("a"),
                artifact("abc", "", None),
                terminal("cap-bound-degradation", "", 0, 5, 80437),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.model_spend_micro_usd, Some(38395));
        assert_eq!(r.model_spend_residual_micro_usd(), Some(42042));

        let text = r.render();
        // The total is quarry's, unchanged.
        assert!(text.contains("$0.0804"), "{text}");
        // And the gap is named, with its upstream cause, rather than papered over.
        assert!(text.contains("$0.0420"), "{text}");
        assert!(text.contains("quarry#20"), "{text}");
        assert!(text.contains("The total above is still exact."), "{text}");
    }

    /// No residual, no line. A footer that mentioned a $0.0000 discrepancy on every
    /// clean run would train a reader to skip the line that matters.
    #[test]
    fn a_run_whose_model_spend_ties_gets_no_residual_line() {
        let r = Receipt::from_outcome(&complete_run());
        assert_eq!(r.model_spend_residual_micro_usd(), None);
        assert!(!r.render().contains("quarry#20"));
    }

    /// A receipt that does not add up is worse than no receipt, so it is flagged.
    #[test]
    fn rows_that_do_not_reconcile_are_flagged_rather_than_rendered_as_if_they_did() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                // Rows totalling 100 against a stated total of 999.
                receipt(&[("n0.0 a", 0.0001)], 0.000999),
                artifact("abc", "", None),
                terminal("complete", "", 0, 0, 999),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert!(!r.rows_reconcile);
        assert!(r.render().contains("do not sum to its stated total"));
    }

    /// An empty receipt has nothing that could fail to add up, and must not be
    /// flagged as broken.
    ///
    /// `no-answer-spend` carries `"rows": []` with `reconciles: true`, so this is the
    /// corpus shape rather than an invented one.
    #[test]
    fn an_empty_receipt_is_not_reported_as_failing_to_reconcile() {
        let o = outcome_with(
            vec![
                frame(1),
                receipt(&[], 0.0),
                artifact("abc", "", None),
                terminal("no-answer", "", 0, 1, 0),
            ],
            Termination::NoAnswer,
        );
        let r = Receipt::from_outcome(&o);
        assert!(r.rows_reconcile);
        assert_eq!(r.row_count, 0);
        assert!(!r.render().contains("do not sum"));
    }

    // ── Citation (P8) ────────────────────────────────────────────────────────

    #[test]
    fn the_record_url_is_cited_when_quarry_made_one() {
        let text = Receipt::from_outcome(&complete_run()).render();
        assert!(
            text.contains("file:///tmp/quarry-run-14e5b754.json"),
            "{text}"
        );
        assert!(text.contains("`14e5b754`"), "{text}");
    }

    /// With no URL, the host's own run directory is cited. P8 needs a path.
    #[test]
    fn the_run_directory_is_cited_when_there_is_no_url() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                artifact("abc", "", None),
                terminal("complete", "", 0, 0, 1),
            ],
            Termination::Completed,
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("/tmp/rn-quarry/gw-run-1"), "{text}");
    }

    // ── Termination notes ────────────────────────────────────────────────────

    /// A crashed run still gets an honest partial reply, not an error string.
    #[test]
    fn a_crashed_run_still_produces_a_reply_with_its_partial_answer_and_a_receipt() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("as far as it got"),
                receipt(&[("n0.0 a", 0.0001)], 0.0001),
                artifact("abc", "file:///tmp/r.json", None),
            ],
            Termination::Crashed { exit_code: 1 },
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("as far as it got"), "{text}");
        assert!(text.contains("exit 1"), "{text}");
        assert!(text.contains("still citable"), "{text}");
        assert!(text.contains("**Receipt**"), "{text}");
    }

    /// Our own timeout is named as ours, not attributed to quarry.
    #[test]
    fn a_gateway_timeout_says_it_was_the_gateway() {
        let o = outcome_with(
            vec![frame(1), answer("partial")],
            Termination::TimedOut {
                after: Duration::from_secs(90),
            },
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("not by quarry"), "{text}");
        assert!(text.contains("1m30s"), "{text}");
    }

    /// A usage error is a host fault and says so, because a sender told "it failed"
    /// would retry a request that will fail identically forever.
    #[test]
    fn a_usage_error_is_reported_as_the_hosts_fault_with_nothing_spent() {
        let o = outcome_with(vec![], Termination::UsageError);
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("nothing ran and nothing was spent"), "{text}");
        assert!(text.contains("fault here, not in your request"), "{text}");
    }

    /// A refused stream version explains itself, and says the record survived.
    #[test]
    fn an_unsupported_stream_version_explains_that_the_record_is_unaffected() {
        let o = outcome_with(
            vec![],
            Termination::StreamVersionUnsupported { declared: 7 },
        );
        let text = Receipt::from_outcome(&o).render();
        assert!(text.contains("version 7"), "{text}");
        assert!(text.contains("still citable"), "{text}");
    }

    /// A clean run gets no termination note: the sections above already state
    /// everything, and a second vaguer statement of the same facts is noise.
    #[test]
    fn a_clean_run_gets_no_termination_note() {
        let text = Receipt::from_outcome(&complete_run()).render();
        assert!(!text.contains("Anything above"), "{text}");
    }

    // ── Record detail ────────────────────────────────────────────────────────

    /// The record names *which* nodes gapped — the detail two integers cannot carry.
    #[test]
    fn the_record_names_the_gapped_nodes_when_it_is_available() {
        let mut o = outcome_with(
            vec![
                frame(1),
                answer("partial"),
                terminal("time-truncated", "latency", 2, 0, 172),
            ],
            Termination::Truncated {
                bound_by: Some("latency".to_string()),
            },
        );
        o.record = Some(RunRecordSummary {
            run_id: "abc".to_string(),
            bound_by: "latency".to_string(),
            caps: RecordCaps::default(),
            unverified: vec![],
            outcomes: vec![gapped_node("n0.1"), gapped_node("n0.3")],
        });
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.gap_nodes, vec!["n0.1", "n0.3"]);
        assert!(r.render().contains("Nodes: n0.1, n0.3"));
    }

    /// Without a record, the count still renders. The gap count comes from the
    /// stream, so a missing record costs detail and not the fact.
    #[test]
    fn the_gap_count_survives_a_missing_record() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("partial"),
                terminal("time-truncated", "latency", 2, 0, 172),
            ],
            Termination::Truncated {
                bound_by: Some("latency".to_string()),
            },
        );
        assert!(o.record.is_none());
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.gaps, 2);
        assert!(r.gap_nodes.is_empty());
        assert!(r.render().contains("(2 nodes)"));
    }

    /// Go's zero `time.Time` is year 1, so an unset deadline must not be rendered as
    /// a deadline in the year 1.
    #[test]
    fn gos_zero_time_is_read_as_no_deadline_rather_than_as_the_year_one() {
        let caps = RecordCaps {
            spend: 250_000,
            latency_nanos: 0,
            due: "0001-01-01T00:00:00Z".to_string(),
        };
        assert_eq!(caps.due(), None);
        assert_eq!(caps.latency(), None);

        let text = Receipt::from_outcome(&complete_run()).render();
        assert!(!text.contains("0001-01-01"), "{text}");
    }

    /// Go marshals `time.Duration` as nanoseconds, so a 190ms latency cap must not be
    /// read as 190 million seconds.
    ///
    /// `time-truncated` carries `"Latency": 190000000` — six years if read as
    /// seconds, which is the direction that makes a tight cap look like no cap.
    #[test]
    fn a_latency_cap_is_read_as_nanoseconds_not_seconds() {
        let caps = RecordCaps {
            spend: 1_000_000,
            latency_nanos: 190_000_000,
            due: String::new(),
        };
        assert_eq!(caps.latency(), Some(Duration::from_millis(190)));
    }

    // ── truncated() ──────────────────────────────────────────────────────────

    /// `truncated()` is broader than "has gaps", matching quarry's own `Truncated()`.
    ///
    /// The `live-partition` shape: exit 0, no gaps, empty `bound_by`, and 5 unfunded
    /// nodes. A host keying on gaps alone would call this complete.
    #[test]
    fn a_run_with_no_gaps_and_no_bound_by_is_still_truncated_if_nodes_went_unfunded() {
        let o = outcome_with(
            vec![
                frame(1),
                answer("a"),
                terminal("cap-bound-degradation", "", 0, 5, 80437),
            ],
            Termination::Completed,
        );
        let r = Receipt::from_outcome(&o);
        assert_eq!(r.gaps, 0);
        assert_eq!(r.bound_by, None);
        assert!(r.truncated());
    }

    #[test]
    fn a_complete_run_is_not_truncated() {
        assert!(!Receipt::from_outcome(&complete_run()).truncated());
    }

    // ── Rendering shape ─────────────────────────────────────────────────────

    /// The answer comes first and the receipt after a rule, so a reader reaches the
    /// answer without scrolling past the accounting.
    #[test]
    fn the_answer_precedes_the_receipt() {
        let text = Receipt::from_outcome(&complete_run()).render();
        let answer_at = text.find("Storage costs scale").expect("no answer");
        let receipt_at = text.find("**Receipt**").expect("no receipt");
        assert!(answer_at < receipt_at, "{text}");
    }

    /// The footer alone is available without the answer, for the dashboard and for
    /// tests that assert a figure.
    #[test]
    fn the_footer_can_be_rendered_without_the_answer() {
        let footer = Receipt::from_outcome(&complete_run()).render_footer();
        assert!(footer.starts_with("**Receipt**"));
        assert!(!footer.contains("Storage costs scale"));
    }
}
