//! quarry's `RunEvent` stream: the wire types and a line-at-a-time NDJSON parser.
//!
//! # The union is open
//!
//! quarry's `runevent.go` declares `RunEvent` as a marker interface rather than a
//! closed sum type, for a stated reason: agate's SPA reducer tolerates unknown
//! types, so "adding an event kind here cannot break an existing consumer." This
//! parser honours that. An unrecognised `type` becomes [`RunEvent::Unknown`] with
//! its raw JSON preserved — never an error, never dropped. A host that rejected
//! unknown kinds would make quarry's forward-compatibility guarantee false on
//! this side of the boundary, and would break on the next event kind quarry adds.
//!
//! # Field names are pinned by hand
//!
//! There is no shared IDL. These structs mirror `runevent.go` exactly, including
//! the nested `provenance` object, whose Python and TypeScript twins are
//! `extra="forbid"` — an unrecognised key there is a hard validation error, not an
//! ignored field. We are a *reader*, so an unknown key is harmless to us, but a
//! **rename upstream is a break here**. That is why the fixture corpus carries
//! byte-level samples rather than round-tripping our own serialisation.
//!
//! # Costs
//!
//! Costs arrive as float USD at 6 decimal places — quarry converts from its
//! internal int64 micro-units exactly once, at this wire edge. We convert straight
//! back with `round(usd × 1e6)`, never truncation, so the number that entered
//! quarry's ledger is the number that lands in ours. Truncating would lose up to a
//! micro-unit per row.
//!
//! # The stream we read is *framed*, and the frame is the host's half
//!
//! quarry has two folds, and the difference is easy to get wrong because only one
//! of them is named after the type. `RunEvents` produces agate's four events —
//! `model`, `answer`, `receipt`, `artifact` — and carries no `Gap`, no `BoundBy`
//! and no truncation, because agate's schema has no gap representation.
//! **`HostRunEvents` wraps those four in a frame**, and `cmd/quarry/run.go` calls
//! the framed one:
//!
//! ```text
//! {"type":"quarry_stream","version":1,"producer":"quarry-go"}   first
//!   … agate's four events, byte-identical …
//! {"type":"quarry_outcome","outcome":…,"bound_by":…,"gaps":…}   last
//! ```
//!
//! Both frame kinds are namespaced `quarry_*` precisely because they are *not* part
//! of agate's union: agate's models declare `extra="forbid"` and have nowhere to put
//! a gap, so — in quarry's words — "the ONE fact a supervising host most needs …
//! cannot ride on any event agate accepts." We are the host it was added for, so
//! these two get first-class variants rather than being folded into
//! [`RunEvent::Unknown`] with the genuinely unknown kinds.
//!
//! [`StreamEvent::version`] exists so a host can **refuse** a stream, which it
//! cannot do by inspecting events it has never seen. [`OutcomeEvent`] carries the
//! classification, the denomination that bit, and the gap and unfunded counts as
//! *separate integers* — and its **absence** is the only in-band signal that a run
//! was killed, since NDJSON yields whole lines either way.
//!
//! [`OutcomeEvent::total_micros`] is the one figure on the stream that is not a
//! float: quarry's own ledger integers, carried so a host has nothing to reconcile.
//! Prefer it over summing the receipt's rows.
//!
//! The record file is still read, but as corroboration rather than as the sole
//! source — see [`RunRecordSummary`]. Deriving the verdict ourselves from
//! `Truncated()` when quarry already sent us `Classify()`'s answer would be a
//! second derivation that can disagree with the first.

use crate::quarry::caps::UNLIMITED_MICRO_USD;
use serde::Deserialize;
use std::collections::BTreeMap;

// ── Cost conversion ───────────────────────────────────────────────────────────

/// Convert quarry's wire-edge USD float into int64 micro-dollars.
///
/// `round`, never `int()`. quarry's integration doc calls this out by name:
/// truncation "loses the last micro-unit and would desync the local debit." The
/// same rule governs `/v1/chat/completions`'s `cost.micro_usd`, so a receipt
/// summed here and a completion metered there agree to the unit.
pub fn usd_to_micro(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

// ── Event types (mirror of quarry's runevent.go) ──────────────────────────────

/// One distinct pinned model version that produced output in this run.
///
/// One event per *version*, not per node — quarry dedupes by label before
/// emitting, so a wide tree does not flood the stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelEvent {
    /// agate's roster vocabulary. quarry has no tiers, so it duplicates `label`
    /// here rather than inventing one — that would be a false claim about a roster
    /// it does not participate in.
    #[serde(default)]
    pub tier: String,
    /// The explicit versioned model ID.
    pub label: String,
    /// Always `"done"`: the stream is a post-hoc projection, not a live feed.
    #[serde(default)]
    pub state: String,
    /// Summed spend for this version, in USD as it arrived.
    #[serde(default)]
    pub cost: f64,
}

impl ModelEvent {
    /// Spend attributed to this model version, in int64 micro-dollars.
    pub fn cost_micro_usd(&self) -> i64 {
        usd_to_micro(self.cost)
    }
}

/// The root node's reduced answer.
///
/// **Absent when the root produced nothing.** quarry omits this event entirely
/// rather than emitting an empty string, so its absence is meaningful: it is the
/// difference between a run that answered and one that could not. The supervisor
/// keys its no-answer classification on exactly that.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AnswerEvent {
    #[serde(default)]
    pub title: String,
    pub text: String,
}

/// One itemised cost line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ReceiptRow {
    pub label: String,
    /// Always `"llm"` from quarry today. Do not assume: quarry's Python twin has
    /// `embedding`/`retrieval`/`compute` kinds that its TypeScript twin rejected
    /// (agate#265 C2), so the vocabulary is actively in motion upstream. Kept as a
    /// plain string rather than an enum for that reason.
    pub kind: String,
    #[serde(default)]
    pub cost: f64,
}

impl ReceiptRow {
    /// This row's cost in int64 micro-dollars.
    ///
    /// Convert each row with this **before** summing, never afterwards: two of
    /// quarry's own fixtures carry rows that do not sum to the stated total in
    /// float64, and they exist to fail a host that adds the floats first.
    pub fn cost_micro_usd(&self) -> i64 {
        usd_to_micro(self.cost)
    }
}

/// The itemised receipt closing the run.
///
/// `rows` account for *all* of `total`: every node that spent gets a row,
/// internal reduce nodes included. quarry's stated rule — "a receipt that does
/// not add up is worse than no receipt." [`ReceiptEvent::rows_reconcile`] checks
/// it, because a host that renders a receipt should notice if it stops adding up.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ReceiptEvent {
    #[serde(default)]
    pub rows: Vec<ReceiptRow>,
    #[serde(default)]
    pub total: f64,
}

impl ReceiptEvent {
    /// Total spend in int64 micro-dollars.
    pub fn total_micro_usd(&self) -> i64 {
        usd_to_micro(self.total)
    }

    /// Summed row costs in int64 micro-dollars.
    pub fn rows_micro_usd(&self) -> i64 {
        self.rows.iter().map(|r| usd_to_micro(r.cost)).sum()
    }

    /// Whether the itemised rows sum to the stated total, to the micro-unit.
    ///
    /// Summed in integer micro-units rather than by adding the `f64` costs, so
    /// float representation error cannot make an exact comparison flap.
    pub fn rows_reconcile(&self) -> bool {
        self.rows_micro_usd() == self.total_micro_usd()
    }
}

/// The trust summary quarry attaches to its artifact event.
///
/// # `stability` cannot be trusted to mean "measured"
///
/// The field is declared as a non-nullable number on both of agate's twins, so
/// quarry has **no in-band way to say "not measured"** — its `StabilityKnown` flag
/// is `json:"-"`. quarry's chosen workaround is to omit the whole `provenance`
/// object when the rate is unpublishable, which it is in three distinct cases: a
/// single run (no distribution exists), a rate of 0 reached with unassessed
/// comparisons ("nobody could tell", not "nothing replicated"), and a truncated
/// comparison pass.
///
/// So: **a present `stability` of `0.0` still cannot be read as "nothing
/// replicated"** by a host that did not itself establish which case it is in.
/// Render the number with its provenance, or not at all.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Provenance {
    /// The `RunRecord` content hash — the same value as `ArtifactEvent::run_id`.
    pub record_hash: String,
    /// Nodes a verifier checked *and* passed.
    #[serde(default)]
    pub verified: u64,
    /// Nodes no verifier assessed — what was **not** checked.
    #[serde(default)]
    pub unverified: u64,
    /// Stable-claim fraction, 0..1. See the type docs before rendering this.
    #[serde(default)]
    pub stability: f64,
    /// Claims an adversary refuted.
    #[serde(default)]
    pub adversarial_findings: u64,
}

/// Points at the citable record, and optionally summarises how much to trust it.
///
/// Emitted **even for an empty or failed run**: the record is identified by its
/// content hash regardless, and a run that produced nothing is exactly the kind
/// that must stay citable. That unconditional emission is what makes this event's
/// presence a usable signal — see `supervisor::Termination`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ArtifactEvent {
    pub run_id: String,
    /// Where the full `RunRecord` is retrievable. May be empty when the record is
    /// not yet addressable.
    #[serde(default)]
    pub url: String,
    /// Absent when quarry judged its own stability estimate unpublishable.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

// ── The frame (quarry's own events, not agate's) ───────────────────────────────

/// The stream contract version this build understands.
///
/// A stream declaring anything else is **refused**, not folded. That is the whole
/// purpose of the version line: quarry's rule is that adding an event *kind* is a
/// minor change and does not bump this, while changing or removing a *field*, or
/// changing what an existing kind means, is major and does. So a version we do not
/// know is by definition a change we cannot absorb by skipping — the events we do
/// recognise may no longer mean what we think.
pub const SUPPORTED_STREAM_VERSION: u32 = 1;

/// Opens a framed stream, declaring the contract version.
///
/// **First line, and it must be**: a host cannot refuse a stream it does not
/// understand by inspecting events it has never seen, so the version precedes
/// anything a consumer would try to fold.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEvent {
    pub version: u32,
    /// Which implementation wrote the stream — `"quarry-go"` for the Go binary.
    ///
    /// Recorded because there is a parallel Python quarry: the two agree on
    /// behaviour but are not the same code, and a host reading a vendored fixture
    /// months later needs to know which produced it.
    #[serde(default)]
    pub producer: String,
}

/// Closes a framed stream, stating how the run ended.
///
/// # Its absence is as load-bearing as its content
///
/// NDJSON yields complete lines whether or not the producer finished, so a run cut
/// off after the artifact event looks exactly like one that finished cleanly. This
/// terminal marker is the **only in-band way** to tell a killed run from a
/// completed one — and a host reading a captured stream from a file has no exit code
/// to fall back on.
///
/// # Gaps and unfunded are two denominations, never a sum
///
/// [`Self::gaps`] counts nodes **time** cut short; [`Self::unfunded`] counts nodes
/// the spend cap priced out. quarry keeps them apart deliberately: only time
/// produces a gap, and being priced out is *planned degradation inside authority*
/// (disclosed before spend), not missing work. Adding them together would offer a
/// sender more time when what they needed was money — the mislabelling quarry's two
/// separate error sentinels exist to prevent.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OutcomeEvent {
    /// quarry's own classification: `complete`, `time-truncated`,
    /// `cap-bound-degradation`, or `no-answer`.
    ///
    /// Read rather than re-derived. quarry computes this from the record with a
    /// documented precedence — no-answer first, then time, then spend — and a second
    /// derivation here could disagree with the exit code, which comes from the same
    /// call upstream.
    #[serde(default)]
    pub outcome: String,
    /// The denomination that actually bit: `spend`, `latency`, `due`, or empty.
    ///
    /// **Empty is meaningful, not missing** — it means no cap bound this run. Which
    /// cap bit is the difference between a useful remedy and a useless one: raising
    /// the wrong cap buys nothing.
    #[serde(default)]
    pub bound_by: String,
    /// Nodes **time** cut short. Zero is a measurement here, not an absence.
    #[serde(default)]
    pub gaps: u64,
    /// Nodes the spend cap priced out. Not gaps. Never added to [`Self::gaps`].
    #[serde(default)]
    pub unfunded: u64,
    /// The run's spend in integer micro-units — quarry's ledger, not a float.
    ///
    /// The one figure on this stream that is not a float, carried on quarry's own
    /// event precisely so a host has nothing to reconcile. Prefer this over summing
    /// [`ReceiptEvent::rows`].
    #[serde(default)]
    pub total_micros: i64,
    /// The spend cap, or `-1` for unlimited.
    ///
    /// **`-1`, not `0`.** Zero reads as a cap of nothing, which would make an
    /// uncapped run look infinitely overspent rather than unlimited.
    ///
    /// Note this is the one field here that must **not** default to zero when absent
    /// from the wire. Every other field is a count or a string where zero and empty
    /// are honest measurements; for this one, `#[serde(default)]` would silently turn
    /// a missing cap into the tightest cap expressible.
    #[serde(default = "unlimited_cap")]
    pub cap_micros: i64,
}

/// [`UNLIMITED_MICRO_USD`], as serde's default for a missing `cap_micros`.
fn unlimited_cap() -> i64 {
    UNLIMITED_MICRO_USD
}

impl Default for OutcomeEvent {
    /// Defaults to **no spend cap**, matching the wire default above rather than
    /// `i64::default()`.
    fn default() -> Self {
        Self {
            outcome: String::new(),
            bound_by: String::new(),
            gaps: 0,
            unfunded: 0,
            total_micros: 0,
            cap_micros: unlimited_cap(),
        }
    }
}

impl OutcomeEvent {
    /// Whether a spend cap was in force.
    ///
    /// A cap of zero is a real cap that funds nothing, and says so: only
    /// [`UNLIMITED_MICRO_USD`] is the absence of one.
    pub fn has_spend_cap(&self) -> bool {
        self.cap_micros != UNLIMITED_MICRO_USD
    }
}

/// One event from quarry's stream.
///
/// [`RunEvent::Unknown`] is not an error path — see the module docs on the open
/// union. [`RunEvent::Stream`] and [`RunEvent::Outcome`] are quarry's own framing
/// events rather than agate's, and are first-class here because they carry the facts
/// a supervising host exists to read.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Stream(StreamEvent),
    Outcome(OutcomeEvent),
    Model(ModelEvent),
    Answer(AnswerEvent),
    Receipt(ReceiptEvent),
    Artifact(ArtifactEvent),
    /// An event kind this build does not know. Forwarded with its raw JSON so a
    /// consumer can still display or log it, and so a minor upgrade of quarry does
    /// not require a matching upgrade here.
    Unknown {
        event_type: String,
        raw: serde_json::Value,
    },
}

impl RunEvent {
    /// The wire `type` discriminant.
    pub fn event_type(&self) -> &str {
        match self {
            Self::Stream(_) => "quarry_stream",
            Self::Outcome(_) => "quarry_outcome",
            Self::Model(_) => "model",
            Self::Answer(_) => "answer",
            Self::Receipt(_) => "receipt",
            Self::Artifact(_) => "artifact",
            Self::Unknown { event_type, .. } => event_type,
        }
    }
}

/// The terminal outcome event in a stream, if one arrived.
///
/// Scans **backwards**, and does not read the last line. quarry's rule is that
/// adding an event kind is a minor bump a host must tolerate, so a future kind may
/// follow the outcome — keying on position would break on exactly the change the
/// open union promises is safe. Upstream's own `TerminalOutcome` scans backwards for
/// the same reason.
///
/// `None` means the stream had no terminal event, which for a stream read to EOF
/// means **the run was killed**. It must never be defaulted to "complete".
pub fn terminal_outcome(events: &[RunEvent]) -> Option<&OutcomeEvent> {
    events.iter().rev().find_map(|e| match e {
        RunEvent::Outcome(o) => Some(o),
        _ => None,
    })
}

/// The declared stream version, if the opening frame arrived.
pub fn stream_version(events: &[RunEvent]) -> Option<u32> {
    events.iter().find_map(|e| match e {
        RunEvent::Stream(s) => Some(s.version),
        _ => None,
    })
}

// ── Line parsing ──────────────────────────────────────────────────────────────

/// Why a single line could not be turned into an event.
///
/// Kept distinct from a stream-level failure: one bad line is skipped and
/// recorded, whereas a stream that never yields a single event is a contract
/// mismatch. See `supervisor::Termination::StreamMalformed`.
#[derive(Debug, Clone, PartialEq)]
pub enum LineError {
    /// Not valid JSON at all.
    NotJson(String),
    /// Valid JSON, but not a JSON object.
    NotObject,
    /// A JSON object with no `type` field, or a non-string one. Without a
    /// discriminant it cannot even be forwarded as [`RunEvent::Unknown`].
    MissingType,
    /// The `type` is known but the payload did not match its shape.
    BadShape { event_type: String, detail: String },
}

impl std::fmt::Display for LineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(e) => write!(f, "not JSON: {e}"),
            Self::NotObject => write!(f, "JSON value is not an object"),
            Self::MissingType => write!(f, "no string `type` field"),
            Self::BadShape { event_type, detail } => {
                write!(f, "malformed `{event_type}` event: {detail}")
            }
        }
    }
}

/// Parse one NDJSON line into an event.
///
/// Dispatches on `type` by hand rather than via `#[serde(tag = "type")]`, because
/// serde's unknown-variant fallback only accepts unit variants and we need to keep
/// the raw JSON of an unknown event. That open-union requirement drives the
/// implementation.
pub fn parse_line(line: &str) -> Result<RunEvent, LineError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| LineError::NotJson(e.to_string()))?;
    if !value.is_object() {
        return Err(LineError::NotObject);
    }
    let event_type = value
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or(LineError::MissingType)?
        .to_string();

    let bad = |e: serde_json::Error| LineError::BadShape {
        event_type: event_type.clone(),
        detail: e.to_string(),
    };

    match event_type.as_str() {
        "quarry_stream" => serde_json::from_value(value)
            .map(RunEvent::Stream)
            .map_err(bad),
        "quarry_outcome" => serde_json::from_value(value)
            .map(RunEvent::Outcome)
            .map_err(bad),
        "model" => serde_json::from_value(value)
            .map(RunEvent::Model)
            .map_err(bad),
        "answer" => serde_json::from_value(value)
            .map(RunEvent::Answer)
            .map_err(bad),
        "receipt" => serde_json::from_value(value)
            .map(RunEvent::Receipt)
            .map_err(bad),
        "artifact" => serde_json::from_value(value)
            .map(RunEvent::Artifact)
            .map_err(bad),
        _ => Ok(RunEvent::Unknown {
            event_type,
            raw: value,
        }),
    }
}

// ── Run record summary ────────────────────────────────────────────────────────

/// The part of quarry's `RunRecord` needed to classify how a run ended.
///
/// # Why this exists, now that the frame carries the verdict
///
/// The distinction quarry is most insistent about is this one:
///
/// > Only TIME is a gap. A node that could not be *afforded* is planned
/// > degradation, recorded with empty content and no `Gap` flag.
///
/// quarry keeps `ErrRecordedGap` and `ErrRecordedUnfunded` as separate sentinels
/// because reusing one "would relabel spend degradation as time truncation", and
/// getting it backwards sends the wrong repair signal — raising the wrong cap buys
/// nothing.
///
/// **[`OutcomeEvent`] is the authority for that verdict**, not this type. It carries
/// `outcome`, `bound_by`, `gaps` and `unfunded` directly, computed by quarry's own
/// `Classify()` — the same call that produced the process exit code. Re-deriving the
/// verdict here from `Truncated()` would be a second derivation that can disagree
/// with the first, and there would be no way to tell which was right.
///
/// This type survives as **corroboration and detail**: the per-node view the
/// terminal event summarises into two integers, still useful for a receipt that
/// wants to name *which* nodes gapped. It is also the fallback when a stream
/// arrives with no terminal event at all — but in that case the run was killed, and
/// the record is being read to salvage what is knowable, not to declare success.
///
/// # Field naming
///
/// `RunRecord` and `NodeOutcome` carry **no serde tags upstream** — Go's encoder
/// emits exported Go identifiers verbatim. So the wire keys are `RunID`,
/// `BoundBy`, `Outcomes`, `NodeID`, `Gap`, and so on, in Go's casing.
/// Deserialising these as snake_case would silently match nothing and report every
/// run as complete.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RunRecordSummary {
    /// Content hash of the record — its identity.
    #[serde(rename = "RunID", default)]
    pub run_id: String,
    /// Which cap actually bit: `"spend"`, `"latency"`, `"due"`, or empty for none.
    #[serde(rename = "BoundBy", default)]
    pub bound_by: String,
    #[serde(rename = "Outcomes", default)]
    pub outcomes: Vec<NodeOutcomeSummary>,
}

/// Per-node fields needed for the truncation verdict.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NodeOutcomeSummary {
    #[serde(rename = "NodeID", default)]
    pub node_id: String,
    #[serde(rename = "Content", default)]
    pub content: String,
    #[serde(rename = "Cost", default)]
    pub cost: i64,
    /// Set only for **time** truncation. Never for a node the cap priced out.
    #[serde(rename = "Gap", default)]
    pub gap: bool,
    #[serde(rename = "CacheHit", default)]
    pub cache_hit: bool,
    #[serde(rename = "Model", default)]
    pub model: String,
    /// `None` means no verifier assessed this node — distinct from `Some(false)`.
    #[serde(rename = "Verified", default)]
    pub verified: Option<bool>,
    #[serde(rename = "Children", default)]
    pub children: Vec<String>,
}

impl RunRecordSummary {
    /// Nodes truncated by **time**. A field-for-field port of quarry's `Gaps()`.
    pub fn gaps(&self) -> Vec<&NodeOutcomeSummary> {
        self.outcomes.iter().filter(|o| o.gap).collect()
    }

    /// Nodes the cap could not afford: they reached no model and produced
    /// nothing, but they are **not** gaps.
    ///
    /// Ported from quarry's `Unfunded()`. The discriminator is the absence of a
    /// *model*, and the verdict check matters: a node that was solved and
    /// verified-empty stays out, because an empty answer is a result rather than a
    /// shortfall.
    pub fn unfunded(&self) -> Vec<&NodeOutcomeSummary> {
        self.outcomes
            .iter()
            .filter(|o| {
                !o.gap
                    && !o.cache_hit
                    && o.children.is_empty()
                    && o.model.is_empty()
                    && o.content.is_empty()
                    && o.verified.is_none()
            })
            .collect()
    }

    /// Whether the run stopped short of what it set out to do.
    ///
    /// Deliberately **broader than [`Self::gaps`]** — quarry's `Truncated()` is
    /// too, for the same reason: a run that hit its spend cap and dropped half its
    /// children has no gaps at all, while being the clearest possible case of a run
    /// that did not finish. Three signals, any one sufficient: a gap, `BoundBy`
    /// set, or an unfunded node.
    pub fn truncated(&self) -> bool {
        !self.bound_by.is_empty()
            || self.outcomes.iter().any(|o| o.gap)
            || !self.unfunded().is_empty()
    }

    /// Total spend across all nodes, in micro-dollars.
    ///
    /// Guards on quarry's `Unlimited` sentinel: costs are int64 micro-units where
    /// `-1` means *unlimited*, not "minus one micro-dollar". Summing it raw would
    /// understate the total.
    pub fn total_cost_micro_usd(&self) -> i64 {
        self.outcomes
            .iter()
            .filter(|o| o.cost >= 0)
            .map(|o| o.cost)
            .sum()
    }
}

// ── Stream statistics ─────────────────────────────────────────────────────────

/// What a supervised run's event stream contained.
///
/// Carried on the run outcome so a caller can tell a clean stream from one that
/// needed recovery, without re-reading the events.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamStats {
    /// Lines read from stdout, parsed and skipped alike.
    pub lines: usize,
    /// Events successfully parsed, including [`RunEvent::Unknown`].
    pub events: usize,
    /// Event kinds this build does not know, counted by `type`.
    pub unknown_kinds: BTreeMap<String, usize>,
    /// Lines that could not be parsed, each with its 1-based line number.
    pub bad_lines: Vec<(usize, LineError)>,
    /// Whether the supervisor stopped reading stdout before it reached EOF.
    ///
    /// Happens when the child is killed but a *descendant* still holds the write
    /// end of the pipe — a `sleep`, a spawned verifier — so EOF never arrives and
    /// blindly reading to the end would hang past our own timeout. The supervisor
    /// gives the drain a bounded grace period and then abandons it.
    ///
    /// When this is set, the counts above are a **lower bound**, not a total. That
    /// is worth carrying rather than hiding: a caller comparing an event count
    /// against a receipt needs to know the stream was cut off on our side.
    pub read_abandoned: bool,
}

impl StreamStats {
    /// Whether every line read parsed into an event.
    ///
    /// Does not consider [`Self::read_abandoned`] — a stream can be entirely
    /// well-formed and still have been cut short by a kill. Those are separate
    /// facts and a caller may care about either.
    pub fn clean(&self) -> bool {
        self.bad_lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── The frame ─────────────────────────────────────────────────────────────

    #[test]
    fn an_absent_cap_micros_deserialises_to_unlimited_and_not_to_zero() {
        // Every other field on this event may honestly default to zero, because zero
        // is a measurement for a count and empty is a measurement for `bound_by`. For
        // this one field it is a lie in the dangerous direction: `#[serde(default)]`
        // would turn a cap quarry did not state into the tightest cap expressible,
        // and a run that spent anything would read as having blown its budget.
        let line = r#"{"type":"quarry_outcome","outcome":"complete","total_micros":250}"#;
        let event = match parse_line(line).expect("parses") {
            RunEvent::Outcome(o) => o,
            other => panic!("expected an outcome event, got {other:?}"),
        };
        assert_eq!(event.cap_micros, UNLIMITED_MICRO_USD);
        assert!(!event.has_spend_cap(), "an unstated cap is no cap");

        // And the explicit forms, which must not collapse into each other.
        let unlimited = parse_outcome(r#"{"type":"quarry_outcome","cap_micros":-1}"#);
        assert!(!unlimited.has_spend_cap());
        let funds_nothing = parse_outcome(r#"{"type":"quarry_outcome","cap_micros":0}"#);
        assert!(
            funds_nothing.has_spend_cap(),
            "a cap of zero is a real cap that funds nothing, not the absence of one"
        );
        let real = parse_outcome(r#"{"type":"quarry_outcome","cap_micros":250000}"#);
        assert!(real.has_spend_cap());
    }

    #[test]
    fn the_default_outcome_event_is_uncapped_rather_than_capped_at_zero() {
        // `#[derive(Default)]` would give `cap_micros: 0` here and disagree with the
        // wire default above — so the two would differ depending on whether a field
        // was absent or the value was constructed, which is the kind of split that
        // shows up as a receipt claiming an overspend that never happened.
        assert_eq!(OutcomeEvent::default().cap_micros, UNLIMITED_MICRO_USD);
        assert!(!OutcomeEvent::default().has_spend_cap());
    }

    #[test]
    fn an_empty_bound_by_is_carried_rather_than_dropped() {
        // quarry emits `""` because "no cap bound this run" is a finding, not an
        // omission. Which cap bit is the difference between a useful remedy and a
        // useless one.
        let event =
            parse_outcome(r#"{"type":"quarry_outcome","outcome":"complete","bound_by":""}"#);
        assert_eq!(event.bound_by, "");
        let bound = parse_outcome(r#"{"type":"quarry_outcome","bound_by":"latency"}"#);
        assert_eq!(bound.bound_by, "latency");
    }

    fn parse_outcome(line: &str) -> OutcomeEvent {
        match parse_line(line).expect("parses") {
            RunEvent::Outcome(o) => o,
            other => panic!("expected an outcome event, got {other:?}"),
        }
    }

    // ── Cost conversion ───────────────────────────────────────────────────────

    #[test]
    fn micro_usd_rounds_and_never_truncates() {
        // The case that matters: a 6-dp USD value a truncating conversion would
        // shave by one micro-unit. quarry's integration doc names `int()` as the
        // defect.
        assert_eq!(
            usd_to_micro(0.0000015),
            2,
            "must round up, not truncate to 1"
        );
        assert_eq!(usd_to_micro(0.15), 150_000);
        assert_eq!(usd_to_micro(1.0), 1_000_000);
        assert_eq!(usd_to_micro(0.0), 0);
    }

    #[test]
    fn micro_usd_survives_float_representation_error() {
        // 0.07 is not representable in binary; naive truncation of 0.07 * 1e6
        // yields 69_999. This is exactly how a receipt would drift.
        assert_eq!(usd_to_micro(0.07), 70_000);
        assert_eq!(usd_to_micro(0.29), 290_000);
    }

    // ── The open union ────────────────────────────────────────────────────────

    #[test]
    fn unknown_event_type_is_forwarded_not_rejected() {
        // quarry's union is open by design — "adding an event kind here cannot
        // break an existing consumer". If this ever returns Err, that promise is
        // false on our side and the next quarry release breaks this host.
        let line = r#"{"type":"future_kind","payload":42}"#;
        match parse_line(line).expect("an unknown kind must parse") {
            RunEvent::Unknown { event_type, raw } => {
                assert_eq!(event_type, "future_kind");
                // The raw JSON survives, so a consumer can still show it.
                assert_eq!(raw["payload"], 42);
            }
            other => panic!("expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn known_event_types_parse_into_typed_variants() {
        let model =
            parse_line(r#"{"type":"model","tier":"m-1","label":"m-1","state":"done","cost":0.25}"#)
                .unwrap();
        assert_eq!(model.event_type(), "model");
        if let RunEvent::Model(m) = model {
            assert_eq!(m.label, "m-1");
            assert_eq!(m.cost_micro_usd(), 250_000);
        } else {
            panic!("expected Model");
        }

        let answer = parse_line(r#"{"type":"answer","text":"forty-two"}"#).unwrap();
        if let RunEvent::Answer(a) = answer {
            assert_eq!(a.text, "forty-two");
            assert_eq!(a.title, "", "an omitted title defaults, it does not error");
        } else {
            panic!("expected Answer");
        }
    }

    #[test]
    fn artifact_provenance_is_optional() {
        // quarry omits the whole object when its stability estimate is
        // unpublishable — that omission is the only in-band way it can say "not
        // measured", so it must not be an error here.
        let bare = parse_line(r#"{"type":"artifact","run_id":"abc","url":""}"#).unwrap();
        if let RunEvent::Artifact(a) = bare {
            assert_eq!(a.run_id, "abc");
            assert!(a.provenance.is_none());
        } else {
            panic!("expected Artifact");
        }

        let with = parse_line(
            r#"{"type":"artifact","run_id":"abc","url":"file:///r.json","provenance":{"record_hash":"abc","verified":3,"unverified":1,"stability":0.5,"adversarial_findings":0}}"#,
        )
        .unwrap();
        if let RunEvent::Artifact(a) = with {
            let p = a.provenance.expect("provenance present");
            assert_eq!(p.record_hash, "abc");
            assert_eq!(p.verified, 3);
            assert_eq!(p.unverified, 1);
            assert_eq!(p.stability, 0.5);
        } else {
            panic!("expected Artifact");
        }
    }

    #[test]
    fn artifact_tolerates_an_extra_provenance_key() {
        // quarry's Python twin is extra="forbid", but we are a reader: a key we do
        // not know must not fail us, or a coordinated upstream field addition
        // would break this host before it could be upgraded.
        let parsed = parse_line(
            r#"{"type":"artifact","run_id":"a","provenance":{"record_hash":"a","verified":1,"unverified":0,"stability":1.0,"adversarial_findings":0,"some_new_field":7}}"#,
        );
        assert!(
            parsed.is_ok(),
            "an unknown provenance key must not be fatal"
        );
    }

    // ── Line-level failures ───────────────────────────────────────────────────

    #[test]
    fn line_errors_are_classified_by_cause() {
        assert!(matches!(
            parse_line("this is not json"),
            Err(LineError::NotJson(_))
        ));
        assert_eq!(parse_line("[1,2,3]"), Err(LineError::NotObject));
        assert_eq!(parse_line(r#"{"label":1}"#), Err(LineError::MissingType));
        // A non-string `type` cannot be forwarded as Unknown either.
        assert_eq!(parse_line(r#"{"type":99}"#), Err(LineError::MissingType));
        // Known kind, wrong shape: `answer` requires `text`.
        assert!(matches!(
            parse_line(r#"{"type":"answer"}"#),
            Err(LineError::BadShape { .. })
        ));
    }

    // ── Receipt reconciliation ────────────────────────────────────────────────

    #[test]
    fn receipt_rows_reconcile_against_total() {
        let ok = ReceiptEvent {
            rows: vec![
                ReceiptRow {
                    label: "n0".into(),
                    kind: "llm".into(),
                    cost: 0.07,
                },
                ReceiptRow {
                    label: "n1".into(),
                    kind: "llm".into(),
                    cost: 0.29,
                },
            ],
            total: 0.36,
        };
        assert!(ok.rows_reconcile(), "0.07 + 0.29 must reconcile with 0.36");
        assert_eq!(ok.total_micro_usd(), 360_000);
        assert_eq!(ok.rows_micro_usd(), 360_000);

        // A receipt that does not add up is worse than no receipt — a host that
        // renders one should be able to notice.
        let short = ReceiptEvent {
            rows: vec![ReceiptRow {
                label: "n0".into(),
                kind: "llm".into(),
                cost: 0.07,
            }],
            total: 0.36,
        };
        assert!(!short.rows_reconcile());
    }

    // ── Record summary: what quarry keeps and the stream drops ────────────────

    fn record(bound_by: &str, outcomes: serde_json::Value) -> RunRecordSummary {
        serde_json::from_value(serde_json::json!({
            "RunID": "deadbeef",
            "BoundBy": bound_by,
            "Outcomes": outcomes,
        }))
        .expect("record summary parses")
    }

    #[test]
    fn record_summary_reads_gos_exported_field_names() {
        // RunRecord has no serde tags upstream, so keys are Go identifiers. If
        // this ever fails, snake_case crept in and every run would report as
        // untruncated with no gaps — silently.
        let r = record(
            "spend",
            serde_json::json!([{"NodeID":"n0","Content":"hi","Cost":150,"Gap":false}]),
        );
        assert_eq!(r.run_id, "deadbeef");
        assert_eq!(r.bound_by, "spend");
        assert_eq!(r.outcomes.len(), 1);
        assert_eq!(r.outcomes[0].node_id, "n0");
        assert_eq!(r.outcomes[0].cost, 150);
    }

    #[test]
    fn snake_case_keys_do_not_populate_the_summary() {
        // The vacuity guard for the test above: prove the rename is load-bearing.
        // A summary built from snake_case keys still parses (every field defaults)
        // but is empty — which is exactly how a naming drift would hide.
        let r: RunRecordSummary = serde_json::from_value(serde_json::json!({
            "run_id": "deadbeef",
            "bound_by": "spend",
            "outcomes": [{"node_id":"n0","gap":true}],
        }))
        .unwrap();
        assert_eq!(r.run_id, "", "snake_case keys must not match");
        assert!(!r.truncated(), "and the verdict would look falsely clean");
    }

    #[test]
    fn only_time_is_a_gap_but_spend_still_truncates() {
        // The distinction quarry keeps two separate sentinels to protect. A node
        // priced out by the cap has empty content and NO Gap flag, so `gaps()` is
        // empty — while the run is genuinely truncated.
        let priced_out = record(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"","Cost":0,"Gap":false,"Model":"","Verified":null},
            ]),
        );
        assert!(
            priced_out.gaps().is_empty(),
            "spend degradation is not a gap"
        );
        assert_eq!(priced_out.unfunded().len(), 1);
        assert!(
            priced_out.truncated(),
            "Truncated() must be broader than Gaps() or spend truncation is invisible"
        );

        // Time truncation is the other case: a gap, and truncated.
        let timed_out = record(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"partial","Cost":100,"Gap":true,"Model":"m-1"},
            ]),
        );
        assert_eq!(timed_out.gaps().len(), 1);
        assert!(timed_out.truncated());
    }

    #[test]
    fn a_solved_but_empty_node_is_not_unfunded() {
        // An empty answer is a RESULT, not a shortfall. The verdict is what tells
        // them apart: this node reached a model and was checked.
        let r = record(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"","Cost":50,"Gap":false,"Model":"m-1","Verified":true},
            ]),
        );
        assert!(r.unfunded().is_empty(), "solved-then-empty is not unfunded");
        assert!(!r.truncated(), "and it did not truncate the run");
    }

    #[test]
    fn a_cache_hit_and_a_reduce_node_are_not_unfunded() {
        // Both legitimately carry no model. Counting them as unfunded would report
        // every healthy run as truncated.
        let r = record(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"merged","Cost":0,"Children":["n0.1"],"Model":""},
                {"NodeID":"n0.1","Content":"cached","Cost":0,"CacheHit":true,"Model":""},
            ]),
        );
        assert!(r.unfunded().is_empty());
        assert!(!r.truncated());
    }

    #[test]
    fn bound_by_alone_truncates() {
        let r = record("latency", serde_json::json!([]));
        assert!(r.truncated(), "a cap that bit is truncation on its own");
        assert!(r.gaps().is_empty());
    }

    #[test]
    fn unlimited_cost_is_excluded_from_the_total() {
        // Unlimited is -1, not zero. Summing it raw understates the total by a
        // micro-unit per unlimited node — quarry's own `spend < cap` trap in
        // another guise.
        let r = record(
            "",
            serde_json::json!([
                {"NodeID":"n0","Cost":-1,"Model":"m-1","Content":"a"},
                {"NodeID":"n1","Cost":250,"Model":"m-1","Content":"b"},
            ]),
        );
        assert_eq!(
            r.total_cost_micro_usd(),
            250,
            "an unlimited node contributes nothing, it does not subtract"
        );
    }

    // ── Stream stats ──────────────────────────────────────────────────────────

    #[test]
    fn stream_stats_clean_only_when_nothing_was_skipped() {
        let mut s = StreamStats::default();
        assert!(s.clean());
        s.bad_lines.push((3, LineError::NotObject));
        assert!(!s.clean());
    }
}
