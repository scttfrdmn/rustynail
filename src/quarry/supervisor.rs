//! Spawns and supervises `quarry` subprocesses.
//!
//! # Subprocess, not library
//!
//! The gateway spawns the `quarry` binary per run and reads its `RunEvent` stream
//! from stdout. The Go twin does the same even though importing quarry as a package
//! would be trivial there, so that both hosts test the *same* integration boundary
//! and the Go-vs-Rust comparison is not confounded at its most interesting seam. It
//! is also quarry's no-resident-orchestrator principle applied to its hosts: a run
//! exists only while it is running.
//!
//! # Structural precedent
//!
//! agenkit's `McpStdioClient` already solves piped-stdio-plus-NDJSON — `Command` +
//! `Stdio::piped()` + `BufReader` over the child's stdout. This follows its shape
//! rather than inheriting from it. What it deliberately does *not* follow is
//! `src/tools/shell.rs`, which uses `cmd.output()`: that collects everything and
//! returns at exit, so a live tree could not be rendered from it.
//!
//! # The distinctions this module exists to keep
//!
//! quarry's degradation semantics are precise and very easy to flatten by accident.
//! [`Termination`] is the type that keeps them apart; its variant docs carry the
//! reasoning. In short:
//!
//! - **Only time is a gap.** A run that ran out of *money* is planned degradation,
//!   disclosed before spend — not a gap. quarry keeps two separate error sentinels
//!   for this because reusing one "would relabel spend degradation as time
//!   truncation", and raising the wrong cap in response buys nothing.
//! - **Crash is not completion.** A non-zero exit with a partial stream is a fault;
//!   a zero exit with a truncated run is a legitimate degraded result. Both produce
//!   fewer events than a full run, so the exit status is the only thing separating
//!   them.
//! - **Our own timeout is time truncation**, and is reported as such — never as
//!   budget degradation.
//!
//! Budget degradation is read from quarry's own record, never inferred from how
//! many events arrived. See [`Termination`] and [`RunOutcome::record`].

use crate::audit::{AuditEvent, AuditLogger};
use crate::config::QuarryConfig;
use crate::quarry::event::{
    self as event, parse_line, OutcomeEvent, RunEvent, RunRecordSummary, StreamStats,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ── Run request ───────────────────────────────────────────────────────────────

/// Everything needed to spawn one run.
///
/// Caps and scope arrive already resolved. This module does not decide what a
/// sender is allowed — that is operator policy, and minting caps from a sender's
/// own request would make the request the policy.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// The problem statement, passed as quarry's positional argument.
    pub statement: String,
    /// Sender identity, for audit and for the reply address.
    pub user_id: String,
    /// Originating channel.
    pub channel_id: String,
    /// Spend cap in micro-dollars. `None` omits `--cap`.
    ///
    /// quarry refuses a run with no cap at all (`Caps.Validate()` →
    /// "at least one cap is required"): planning is budget-conditioned, so a run
    /// with no cap has nothing to plan against. Passing neither this nor
    /// [`Self::deadline`] is therefore refused *here*, with quarry's reason, rather
    /// than spawning a child that will exit non-zero.
    pub spend_micro_usd: Option<i64>,
    /// Latency cap, rendered as quarry's `--deadline`.
    pub deadline: Option<Duration>,
    /// Recursion backstop (`--depth`). `None` leaves quarry's default.
    ///
    /// A backstop, not a design dial: quarry's primary terminator is running out of
    /// verifiers, and a run bounded by depth is under-verified rather than complete.
    pub max_depth: Option<u32>,
    /// Explicit versioned model ID (`--model`).
    ///
    /// Never an alias. Alias routing is observable but non-deterministic, which
    /// breaks replay — the same reason `/v1/chat/completions` reports the model that
    /// actually ran instead of echoing the request.
    pub model: Option<String>,
    /// Scope tags folded into every cache key (`--scope`).
    ///
    /// A `BTreeMap` so rendering is canonical: quarry sorts tags before hashing, and
    /// two hosts that ordered them differently would compute different cache keys
    /// for the same scope. Ordering here is not cosmetic.
    pub scope_tags: BTreeMap<String, String>,
    /// Use quarry's built-in fake provider (`--fake`): no credentials, no money,
    /// synthetic answers. Real shape, real cost accounting, meaningless content.
    pub fake: bool,
    /// Environment for the child, constructed explicitly.
    ///
    /// See [`Supervisor::spawn`] — this is the *entire* environment the child gets.
    pub env: BTreeMap<String, String>,
    /// Withhold `--events-json`, to prove a test would notice its absence.
    ///
    /// Exists only so the mutation check for the flag can be a test rather than a
    /// hand edit someone has to remember to make. `#[cfg(test)]` so it cannot be set
    /// in a release build: this is the one flag whose omission silently disables the
    /// entire event stream, and nothing outside a test may ask for that.
    #[cfg(test)]
    pub suppress_events_json_for_test: bool,
}

impl RunRequest {
    /// A minimal request for `user_id` asking `statement` under a spend cap.
    pub fn new(user_id: &str, channel_id: &str, statement: &str, spend_micro_usd: i64) -> Self {
        Self {
            statement: statement.to_string(),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            spend_micro_usd: Some(spend_micro_usd),
            deadline: None,
            max_depth: None,
            model: None,
            scope_tags: BTreeMap::new(),
            fake: false,
            env: BTreeMap::new(),
            #[cfg(test)]
            suppress_events_json_for_test: false,
        }
    }

    /// Render quarry's CLI arguments.
    ///
    /// `--cap` is a decimal string because that is quarry's flag format; it parses
    /// back with `FromFloat`, which is the same `round(usd × 1e6)` conversion in the
    /// other direction. Formatted at 6 decimal places so a micro-unit survives the
    /// round trip — `{}` on an `f64` would render `0.0000015` in exponent form and
    /// quarry's `Sscanf("%g")` would be reading a different number than we meant.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = vec!["run".to_string()];
        if let Some(micro) = self.spend_micro_usd {
            args.push("--cap".to_string());
            args.push(format!("{:.6}", micro as f64 / 1_000_000.0));
        }
        if let Some(d) = self.deadline {
            args.push("--deadline".to_string());
            // Milliseconds: Go's ParseDuration accepts "ms", and seconds alone
            // would silently floor a sub-second deadline to zero — which quarry
            // reads as "no latency cap" rather than "an immediate one".
            args.push(format!("{}ms", d.as_millis()));
        }
        if let Some(depth) = self.max_depth {
            args.push("--depth".to_string());
            args.push(depth.to_string());
        }
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if !self.scope_tags.is_empty() {
            args.push("--scope".to_string());
            args.push(
                self.scope_tags
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        if self.fake {
            args.push("--fake".to_string());
        }
        // `--events-json` is what makes this integration exist at all. Without it
        // quarry writes its human summary to stdout and emits **no events**:
        // `cmd/quarry/run.go` gates the entire `WriteRunEvents` call on the flag, and
        // moves human output to stderr only when it is set. Omitting it does not
        // degrade the stream, it removes it, and every run then classifies as
        // `StreamMalformed`.
        #[cfg(test)]
        let emit_events = !self.suppress_events_json_for_test;
        #[cfg(not(test))]
        let emit_events = true;
        if emit_events {
            args.push("--events-json".to_string());
        }
        // `--quiet` suppresses quarry's interactive tree. Belt and braces alongside
        // the flag above: `--events-json` already redirects the tree to stderr, but
        // an ANSI-repainting tree on the stderr we also drain is noise in the logs,
        // and the host renders its own view from the events.
        args.push("--quiet".to_string());
        args.push(self.statement.clone());
        args
    }
}

// ── Termination ───────────────────────────────────────────────────────────────

/// How a supervised run ended.
///
/// The variants exist to stop three collapses that all corrupt a receipt:
/// time-truncation reported as budget degradation, a crash reported as a
/// completion, and a run that produced nothing reported as one that answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// Zero exit, an answer, and the record shows nothing was cut short.
    Completed,
    /// Zero exit with an answer, but quarry's record shows the run stopped short
    /// of what it set out to do.
    ///
    /// **A legitimate result, not a failure.** Partial tolerance is quarry's
    /// design: a partial answer with its gaps named is what the system promises to
    /// return, so failing on truncation would make the promise unusable. quarry
    /// itself exits zero here.
    ///
    /// `bound_by` is quarry's own verdict on which cap bit — `"spend"`,
    /// `"latency"`, or `"due"` — read from the record, never inferred from the
    /// event count. `None` means the record showed truncation (an unfunded node or
    /// a gap) without naming a denomination.
    Truncated { bound_by: Option<String> },
    /// quarry exited non-zero because the run produced **no answer at all**.
    ///
    /// A distinct exit code upstream, and distinct here, because the record is
    /// still written and still citable: it faithfully records that nothing was
    /// affordable, which is a useful artifact. Reporting this as a crash would
    /// discard a valid record; reporting it as completion would present an empty
    /// answer as an answer.
    NoAnswer,
    /// **We** killed the child after [`QuarryConfig::run_timeout_seconds`].
    ///
    /// Time truncation, and it must be surfaced as such — never as budget
    /// degradation. A caller told "priced out" would raise the spend cap and buy
    /// nothing, because what actually ran out was time.
    TimedOut { after: Duration },
    /// A caller cancelled the run mid-flight.
    ///
    /// Also time truncation: the tree held a returnable partial answer and we
    /// stopped it. quarry makes the same choice for `^C` — a signal cancels the
    /// run's context so the record still lands with its gaps named, rather than
    /// killing the process outright.
    Cancelled,
    /// Non-zero exit that is not [`Self::NoAnswer`]. A fault, not degradation.
    Crashed { exit_code: i32 },
    /// Terminated by a signal. Distinct from a non-zero exit: nothing in the child
    /// chose this, so its own error reporting never ran.
    KilledBySignal { signal: Option<i32> },
    /// The child ran and exited, but produced **no parseable event at all**.
    ///
    /// A contract mismatch — the wrong binary, or a quarry that no longer emits
    /// this stream — as distinct from the individual bad lines in
    /// [`StreamStats::bad_lines`], which are skipped and recorded while the run
    /// continues. Kept separate so a caller retries a malformed *line* forever
    /// without retrying a malformed *stream* at all.
    StreamMalformed,
    /// The stream declared a contract version this build does not implement.
    ///
    /// **Refused rather than folded**, which is the entire reason quarry puts the
    /// version on the first line. Its compatibility rule is that adding an event
    /// *kind* is a minor change a host must tolerate, while changing or removing a
    /// *field* — or changing what an existing kind means — is major and bumps the
    /// version. So an unknown version is by definition a change that skipping cannot
    /// absorb: the events we still recognise may no longer mean what we read them to
    /// mean, and folding them would produce a confident wrong receipt.
    StreamVersionUnsupported { declared: u32 },
    /// The stream ended with no terminal `quarry_outcome` event.
    ///
    /// **The run was killed.** NDJSON yields complete lines whether or not the
    /// producer finished, so a stream cut off after the artifact event is
    /// byte-indistinguishable from a clean one except for this absence — which is
    /// why quarry added the terminal event and why it must never be defaulted to
    /// "complete". Events read before the cut are kept: they were paid for.
    ///
    /// Distinct from [`Self::Crashed`] because the child may well have exited zero.
    /// A stream truncated mid-line is this, not a bad line.
    StreamIncomplete { events_read: usize },
    /// quarry refused the invocation: bad flags, or caps it would not accept.
    ///
    /// **A host defect, not a run outcome.** Nothing was attempted, so there is
    /// nothing to retry and nothing to cite — the args we built are wrong and need a
    /// code change. Kept out of [`Self::Crashed`] because a crash invites a retry and
    /// this one would fail identically forever.
    ///
    /// The line between this and a fault is whether anything was *attempted*:
    /// upstream notes that `quarry show nonexistent.json` is a fault, not a usage
    /// error, because the invocation was well-formed and the read failed.
    UsageError,
}

impl Termination {
    /// A stable machine-readable slug. Callers classify on this, never on
    /// [`Display`] output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Truncated { .. } => "truncated",
            Self::NoAnswer => "no_answer",
            Self::TimedOut { .. } => "timed_out",
            Self::Cancelled => "cancelled",
            Self::Crashed { .. } => "crashed",
            Self::KilledBySignal { .. } => "killed_by_signal",
            Self::StreamMalformed => "stream_malformed",
            Self::StreamVersionUnsupported { .. } => "stream_version_unsupported",
            Self::StreamIncomplete { .. } => "stream_incomplete",
            Self::UsageError => "usage_error",
        }
    }

    /// Whether the run produced a usable record.
    ///
    /// True for the three *degraded but honest* outcomes as well as a clean one:
    /// a truncated run and a no-answer run both write citable records, and a
    /// cancelled one holds a partial tree. False only for faults, where the child
    /// stopped for a reason unrelated to what it was asked to do.
    pub fn produced_record(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Truncated { .. } | Self::NoAnswer | Self::Cancelled
        )
    }

    /// Whether the run was cut short by **time** — a deadline, our timeout, or a
    /// cancellation.
    ///
    /// Deliberately does **not** include a spend-truncated run. That is the whole
    /// distinction: `Truncated { bound_by: Some("spend") }` is planned degradation
    /// disclosed before the run, and offering it more time would buy nothing.
    pub fn time_truncated(&self) -> bool {
        match self {
            Self::TimedOut { .. } | Self::Cancelled => true,
            Self::Truncated { bound_by } => {
                matches!(bound_by.as_deref(), Some("latency") | Some("due"))
            }
            _ => false,
        }
    }

    /// Whether the run was cut short by **money**.
    ///
    /// Reported only from quarry's own `BoundBy`. Never inferred from a short event
    /// stream, which is equally consistent with a small tree, a crash, or a
    /// deadline.
    pub fn spend_truncated(&self) -> bool {
        matches!(self, Self::Truncated { bound_by } if bound_by.as_deref() == Some("spend"))
    }
}

impl std::fmt::Display for Termination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => write!(f, "completed"),
            Self::Truncated { bound_by: Some(d) } => {
                write!(f, "truncated: the {d} cap bit before the work was done")
            }
            Self::Truncated { bound_by: None } => {
                write!(
                    f,
                    "truncated: the run stopped short of what it set out to do"
                )
            }
            Self::NoAnswer => write!(f, "no answer: nothing was affordable"),
            Self::TimedOut { after } => {
                write!(f, "timed out after {:?} — cut short by time", after)
            }
            Self::Cancelled => write!(f, "cancelled — cut short by time"),
            Self::Crashed { exit_code } => write!(f, "crashed (exit {exit_code})"),
            Self::KilledBySignal { signal: Some(s) } => write!(f, "killed by signal {s}"),
            Self::KilledBySignal { signal: None } => write!(f, "killed by a signal"),
            Self::StreamMalformed => write!(f, "no parseable events — wrong binary?"),
            Self::StreamVersionUnsupported { declared } => write!(
                f,
                "refused a stream declaring contract version {declared}; this build implements {}",
                crate::quarry::event::SUPPORTED_STREAM_VERSION
            ),
            Self::StreamIncomplete { events_read } => write!(
                f,
                "the stream ended without its terminal outcome event after {events_read} events — the run was killed"
            ),
            Self::UsageError => write!(
                f,
                "quarry rejected the invocation — the arguments we built are wrong"
            ),
        }
    }
}

// ── Outcome ───────────────────────────────────────────────────────────────────

/// The result of one supervised run.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// Gateway-assigned run identifier. Distinct from quarry's content-hash
    /// `RunID`, which only exists once the record is written — this one names the
    /// run from the moment it is requested, including when it never starts.
    pub run_id: String,
    /// How it ended.
    pub termination: Termination,
    /// Every event parsed from stdout, in arrival order.
    pub events: Vec<RunEvent>,
    /// Parse statistics, including any skipped lines.
    pub stats: StreamStats,
    /// The child's stderr, captured separately.
    ///
    /// **Never interleaved into the event stream.** quarry writes human-readable
    /// diagnostics here, and merging them into stdout would produce unparseable
    /// lines indistinguishable from a genuine contract break.
    pub stderr: String,
    /// The run's own directory.
    pub run_dir: PathBuf,
    /// quarry's record summary, when a record was written and could be read.
    ///
    /// The authoritative source for gaps, truncation and `BoundBy` — the event
    /// stream carries none of them.
    pub record: Option<RunRecordSummary>,
    /// Wall-clock duration.
    pub duration: Duration,
}

impl RunOutcome {
    /// The root answer text, if the run produced one.
    pub fn answer(&self) -> Option<&str> {
        self.events.iter().find_map(|e| match e {
            RunEvent::Answer(a) => Some(a.text.as_str()),
            _ => None,
        })
    }

    /// Total spend in micro-dollars, from the receipt event.
    pub fn cost_micro_usd(&self) -> Option<i64> {
        self.events.iter().find_map(|e| match e {
            RunEvent::Receipt(r) => Some(r.total_micro_usd()),
            _ => None,
        })
    }
}

// ── Spawn errors ──────────────────────────────────────────────────────────────

/// Why a run could not be started.
///
/// Separate from [`Termination`], which describes a run that *did* start. A caller
/// needs to tell "never ran" from "ran and failed" — the first leaves no record to
/// cite, and only one of them is worth retrying unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// `quarry.enabled` is false.
    Disabled,
    /// [`QuarryConfig::max_concurrent_runs`] is already in use.
    ///
    /// **Refused, not queued.** A queue would silently convert a concurrency limit
    /// into unbounded latency: the sender waits with no disclosure, and a
    /// `--deadline` they set could expire before the run even starts, which would
    /// surface as time truncation of a run that never ran. Refusing is visible
    /// immediately and leaves retry timing to the caller.
    AtCapacity { limit: usize },
    /// No cap of any kind was requested.
    ///
    /// quarry refuses this itself, so we could let the child do it — but spawning a
    /// process in order to have it reject the arguments we chose wastes the spawn
    /// and buries quarry's reasoning in a subprocess's stderr.
    NoCap,
    /// The binary could not be executed.
    BinaryUnavailable { path: String, detail: String },
    /// The run directory could not be created.
    RunDirUnavailable { path: String, detail: String },
    /// The binary could not be verified, so it was not run.
    ///
    /// Carries the specific check that failed rather than collapsing to "refused":
    /// the operator needs to know whether the signature was absent, from the wrong
    /// identity, or simply unverifiable because no mechanism is installed. The
    /// sender-facing text comes from
    /// [`crate::quarry::verify::VerificationRefusal::sender_message`] and names none
    /// of it.
    Unverified {
        refusal: crate::quarry::verify::VerificationRefusal,
    },
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(f, "quarry runs are not enabled (quarry.enabled = false)"),
            Self::AtCapacity { limit } => write!(
                f,
                "already running {limit} quarry runs (quarry.max_concurrent_runs); \
                 refused rather than queued so the wait is not silent"
            ),
            Self::NoCap => write!(
                f,
                "no cap requested: planning is budget-conditioned, so a run needs \
                 at least a spend cap or a deadline"
            ),
            Self::BinaryUnavailable { path, detail } => {
                write!(f, "cannot execute quarry binary at {path}: {detail}")
            }
            Self::RunDirUnavailable { path, detail } => {
                write!(f, "cannot create run directory {path}: {detail}")
            }
            Self::Unverified { refusal } => write!(f, "{refusal}"),
        }
    }
}

impl std::error::Error for SpawnError {}

impl SpawnError {
    /// A stable machine-readable slug.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "quarry_disabled",
            Self::AtCapacity { .. } => "at_capacity",
            Self::NoCap => "no_cap",
            Self::BinaryUnavailable { .. } => "binary_unavailable",
            Self::RunDirUnavailable { .. } => "run_dir_unavailable",
            // The specific failing check, not a generic `unverified`: a code that
            // could not distinguish "no signature" from "no mechanism installed"
            // would be the operator-hunting problem this issue exists to avoid.
            Self::Unverified { refusal } => refusal.code(),
        }
    }

    /// What the sender is told.
    ///
    /// Every variant but one is an operator configuration problem the sender can do
    /// nothing about, and verification refusals must additionally not leak a path,
    /// digest, or identity regex. [`Self::AtCapacity`] is the exception: a transient
    /// condition the sender can act on by waiting.
    pub fn sender_message(&self) -> &str {
        match self {
            Self::Unverified { refusal } => refusal.sender_message(),
            Self::AtCapacity { .. } => {
                "I am already running as many of those as I can at once. Try again shortly."
            }
            Self::Disabled
            | Self::NoCap
            | Self::BinaryUnavailable { .. }
            | Self::RunDirUnavailable { .. } => {
                "That capability is unavailable right now. This is a configuration problem on \
                 my side, not something wrong with your request."
            }
        }
    }
}

// ── Credential hygiene ────────────────────────────────────────────────────────

/// Environment variables that must never reach a quarry child.
///
/// The child is given an explicitly constructed environment rather than an
/// inherited one, so this list is a **backstop against a caller that builds the
/// wrong map**, not the mechanism. Its only network need is the localhost `/v1`
/// endpoint and the bearer token for it; a provider key would let it bypass the
/// gateway entirely, and a channel token would let it post as the bot.
const FORBIDDEN_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "TAVILY_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "DISCORD_BOT_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "SLACK_SIGNING_SECRET",
    "TWILIO_AUTH_TOKEN",
    "TWILIO_ACCOUNT_SID",
    "WHATSAPP_ACCESS_TOKEN",
    "WHATSAPP_VERIFY_TOKEN",
    "TEAMS_APP_PASSWORD",
    "TEAMS_HMAC_SECRET",
    "EMAIL_PASSWORD",
    "DATABASE_URL",
    "REDIS_URL",
    "DASHBOARD_AUTH_PASSWORD",
];

/// Drop forbidden keys from a child environment, returning what was removed.
///
/// Returns the rejected keys so the caller can log that it happened. Silently
/// dropping a credential would hide a real misconfiguration: something upstream
/// tried to hand a secret to a subprocess, and that is worth a warning even though
/// the removal made it harmless.
fn sanitize_env(env: &BTreeMap<String, String>) -> (BTreeMap<String, String>, Vec<String>) {
    let mut clean = BTreeMap::new();
    let mut rejected = Vec::new();
    for (k, v) in env {
        let upper = k.to_ascii_uppercase();
        if FORBIDDEN_ENV_KEYS.contains(&upper.as_str()) {
            rejected.push(k.clone());
        } else {
            clean.insert(k.clone(), v.clone());
        }
    }
    (clean, rejected)
}

// ── Supervisor ────────────────────────────────────────────────────────────────

/// Spawns quarry runs, bounded by a concurrency limit, and reaps their output.
pub struct Supervisor {
    config: QuarryConfig,
    /// Signed-binary verification, checked before every spawn.
    ///
    /// Not an [`Option`]. A supervisor with no gate would be a supervisor that spawns
    /// unverified binaries, and the way that ships accidentally is a field somebody
    /// forgot to set — so [`Supervisor::new`] always builds one, and the only way to
    /// skip the signature check is the config setting that says so and warns.
    gate: crate::quarry::verify::SpawnGate,
    /// Runs currently executing. An [`AtomicUsize`] rather than a semaphore
    /// because the limit is enforced by *refusal*: a permit-based limiter would
    /// make a caller wait, which is the queueing behaviour
    /// [`SpawnError::AtCapacity`] exists to avoid.
    active: Arc<AtomicUsize>,
    audit: Option<Arc<AuditLogger>>,
}

/// Decrements the active-run counter however the run ends.
///
/// A guard rather than a decrement at each return site: a run that panics, is
/// cancelled, or returns early through `?` must still release its slot, or the
/// concurrency limit leaks downward until no run can start.
#[derive(Debug)]
struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Supervisor {
    /// Build a supervisor from config.
    ///
    /// The manifest's declared egress port cannot be checked without knowing this
    /// gateway's own HTTP port, and a check that cannot run must refuse — so a
    /// supervisor built this way refuses every manifest declaring egress. Callers
    /// that will actually spawn must use [`Self::with_gateway_port`]; the gateway
    /// does. Leaving it out is a refusal rather than a silently unchecked port.
    pub fn new(config: QuarryConfig) -> Self {
        let gate = crate::quarry::verify::SpawnGate::new(
            config.verification.clone(),
            config.run_record_dir.clone(),
            None,
        );
        Self {
            config,
            gate,
            active: Arc::new(AtomicUsize::new(0)),
            audit: None,
        }
    }

    /// Tell the verification gate which localhost port the manifest may declare.
    pub fn with_gateway_port(mut self, port: u16) -> Self {
        self.gate.set_gateway_port(Some(port));
        self
    }

    /// Install the signature verifier the gate calls into (#103's mechanism).
    pub fn with_verifier(
        mut self,
        verifier: Arc<dyn crate::quarry::verify::SignatureVerifier>,
    ) -> Self {
        self.gate.set_verifier(Some(verifier));
        self
    }

    /// Attach an audit logger.
    ///
    /// The gate gets it too: a verification refusal is exactly the kind of thing an
    /// operator reads the audit log to find.
    pub fn with_audit(mut self, audit: Option<Arc<AuditLogger>>) -> Self {
        self.gate.set_audit(audit.clone());
        self.audit = audit;
        self
    }

    /// Runs currently executing.
    pub fn active_runs(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Whether quarry supervision is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Claim a concurrency slot, or report that the limit is reached.
    ///
    /// A compare-exchange loop rather than `fetch_add` then check: incrementing
    /// first would let two callers both observe a value over the limit and both
    /// back out, and — worse — a third caller arriving between the increment and
    /// the rollback would be refused a slot that was never really taken.
    fn try_claim_slot(&self) -> Result<ActiveGuard, SpawnError> {
        let limit = self.config.max_concurrent_runs;
        loop {
            let current = self.active.load(Ordering::SeqCst);
            if current >= limit {
                return Err(SpawnError::AtCapacity { limit });
            }
            if self
                .active
                .compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(ActiveGuard(Arc::clone(&self.active)));
            }
        }
    }

    /// Spawn a run and supervise it to completion.
    ///
    /// `events` receives each event **as it arrives**, not after exit, so a caller
    /// can render a live tree. The channel is dropped when the stream ends; a
    /// caller that stops listening does not stall the run.
    ///
    /// `cancel` cancels the run mid-flight. Cancellation reports as
    /// [`Termination::Cancelled`] — time truncation, never budget degradation.
    ///
    /// The child's environment is exactly [`RunRequest::env`], minus anything in
    /// [`FORBIDDEN_ENV_KEYS`]. It is built with `env_clear()`, so the gateway's own
    /// environment — every provider key and channel token in it — is not inherited.
    pub async fn run(
        &self,
        request: RunRequest,
        events: Option<mpsc::UnboundedSender<RunEvent>>,
        cancel: Option<tokio::sync::oneshot::Receiver<()>>,
    ) -> Result<RunOutcome, SpawnError> {
        if !self.config.enabled {
            self.audit_failed(None, &request.user_id, SpawnError::Disabled.to_string());
            return Err(SpawnError::Disabled);
        }
        if request.spend_micro_usd.is_none() && request.deadline.is_none() {
            self.audit_failed(None, &request.user_id, SpawnError::NoCap.to_string());
            return Err(SpawnError::NoCap);
        }

        let _guard = self.try_claim_slot().inspect_err(|e| {
            self.audit_failed(None, &request.user_id, e.to_string());
        })?;

        // Verification runs **before the run directory exists**, so that a refusal
        // leaves no artifact behind — no child, and no empty run directory that a
        // later reader would mistake for a run that happened. The negative tests
        // assert that absence rather than merely asserting an error was returned,
        // because an error return with the side effect already performed is the #90
        // failure mode.
        let verified = match self.gate.check(&self.config.binary_path).await {
            Ok(v) => v,
            Err(refusal) => {
                let err = SpawnError::Unverified { refusal };
                self.audit_failed(None, &request.user_id, err.to_string());
                return Err(err);
            }
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let run_dir = Path::new(&self.config.run_record_dir).join(&run_id);
        if let Err(e) = tokio::fs::create_dir_all(&run_dir).await {
            let err = SpawnError::RunDirUnavailable {
                path: run_dir.display().to_string(),
                detail: e.to_string(),
            };
            self.audit_failed(Some(&run_id), &request.user_id, err.to_string());
            return Err(err);
        }

        let record_path = run_dir.join("record.json");
        let (env, rejected) = sanitize_env(&request.env);
        if !rejected.is_empty() {
            // The removal already made this harmless; the warning is because
            // something upstream tried, which is a real misconfiguration.
            warn!(
                run_id = %run_id,
                "refused to pass credentials to quarry child: {}",
                rejected.join(", ")
            );
        }

        let mut args = request.to_args();
        // `--out` goes before the positional statement. quarry's flag parser stops
        // at the first non-flag argument, so a flag appended after the statement
        // would be swallowed into it and the record would land at quarry's default
        // path instead of ours.
        let statement = args.pop().expect("to_args always ends with the statement");
        args.push("--out".to_string());
        args.push(record_path.display().to_string());
        args.push(statement);

        let mut cmd = Command::new(&self.config.binary_path);
        cmd.args(&args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let started = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let err = SpawnError::BinaryUnavailable {
                    path: self.config.binary_path.clone(),
                    detail: e.to_string(),
                };
                self.audit_failed(Some(&run_id), &request.user_id, err.to_string());
                return Err(err);
            }
        };

        if let Some(audit) = &self.audit {
            audit.log(AuditEvent::QuarryRunStarted {
                run_id: run_id.clone(),
                user_id: request.user_id.clone(),
                channel_id: request.channel_id.clone(),
                binary_path: self.config.binary_path.clone(),
                env_keys: env.keys().cloned().collect(),
                binary_digest: verified.digest.clone(),
                signature_checked: verified.signature_checked,
            });
        }
        info!(run_id = %run_id, user_id = %request.user_id, "quarry run started");

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        // Both pipes are drained concurrently with the wait. Reading stdout to EOF
        // before waiting would deadlock on a child that fills the stderr pipe
        // buffer, and vice versa.
        //
        // The readers accumulate into shared state rather than returning a value at
        // EOF, because EOF is not guaranteed to arrive — see `drain_grace`.
        let stdout_acc: SharedStdout = Arc::new(std::sync::Mutex::new(Default::default()));
        let stderr_acc: SharedStderr = Arc::new(std::sync::Mutex::new(String::new()));
        let stdout_task = tokio::spawn(read_event_stream(stdout, events, Arc::clone(&stdout_acc)));
        let stderr_task = tokio::spawn(read_to_string(stderr, Arc::clone(&stderr_acc)));

        let timeout = if self.config.run_timeout_seconds == 0 {
            None
        } else {
            Some(Duration::from_secs(self.config.run_timeout_seconds))
        };

        let (status, forced) = wait_with_limits(&mut child, timeout, cancel).await;

        // Wait for the readers to finish draining — but only for a bounded grace
        // period. A killed child's *descendants* can still hold the write end of the
        // pipe (a `sleep`, a spawned verifier), so EOF may never arrive; joining
        // unconditionally would hang the supervisor long past the timeout it just
        // enforced, which defeats having a timeout at all.
        //
        // The accumulators are shared, so abandoning the read keeps everything
        // already parsed. Events emitted before a kill are real and must be
        // reported: a killed run is a truncated run, not a discarded one.
        let abandoned = drain_grace(stdout_task, stderr_task, &run_id).await;
        let drained = std::mem::take(&mut *stdout_acc.lock().expect("stdout accumulator"));
        let events_read = drained.events_seen;
        let mut stats = drained.stats;
        stats.read_abandoned = abandoned;
        let stderr_text = std::mem::take(&mut *stderr_acc.lock().expect("stderr accumulator"));
        let duration = started.elapsed();
        if abandoned {
            warn!(
                run_id = %run_id,
                "stopped reading quarry stdout before EOF: a descendant still holds \
                 the pipe. Event counts are a lower bound."
            );
        }

        let record = read_record(&record_path).await;
        let termination = classify(
            forced,
            status,
            &events_read,
            &stats,
            record.as_ref(),
            duration,
        );

        if !stats.clean() {
            // Individual bad lines are skipped, not fatal — but they are recorded
            // and surfaced, because a stream that needs recovery is a signal even
            // when the run succeeded.
            warn!(
                run_id = %run_id,
                "skipped {} unparseable line(s) in quarry event stream",
                stats.bad_lines.len()
            );
            for (n, e) in &stats.bad_lines {
                debug!(run_id = %run_id, "line {n}: {e}");
            }
        }
        for (kind, count) in &stats.unknown_kinds {
            // Not a warning: the union is open by design and an unknown kind is
            // expected when quarry is ahead of this build.
            debug!(run_id = %run_id, "forwarded {count} event(s) of unknown kind `{kind}`");
        }

        if !termination.produced_record() && !stderr_text.trim().is_empty() {
            // stderr is surfaced on failure and only on failure, and never merged
            // into the event stream.
            error!(run_id = %run_id, "quarry stderr: {}", stderr_text.trim());
        }

        let cost = events_read
            .iter()
            .find_map(|e| match e {
                RunEvent::Receipt(r) => Some(r.total_micro_usd()),
                _ => None,
            })
            .unwrap_or(0);

        if let Some(audit) = &self.audit {
            audit.log(AuditEvent::QuarryRunEnded {
                run_id: run_id.clone(),
                user_id: request.user_id.clone(),
                termination: termination.code().to_string(),
                truncated_by: match &termination {
                    Termination::Truncated { bound_by } => bound_by.clone(),
                    _ => None,
                },
                events: stats.events,
                bad_lines: stats.bad_lines.len(),
                cost_micro_usd: cost,
                duration_ms: duration.as_millis() as u64,
            });
        }
        info!(run_id = %run_id, "quarry run ended: {termination}");

        Ok(RunOutcome {
            run_id,
            termination,
            events: events_read,
            stats,
            stderr: stderr_text,
            run_dir,
            record,
            duration,
        })
    }

    /// Delete run directories beyond the configured retention limits.
    ///
    /// Returns how many were removed. Both limits are independent and either can
    /// be disabled with `0`; with both disabled nothing is deleted, which is a
    /// legitimate choice for an operator archiving records elsewhere.
    pub async fn reap_run_dirs(&self) -> std::io::Result<usize> {
        let root = Path::new(&self.config.run_record_dir);
        if !root.exists() {
            return Ok(0);
        }
        let max_runs = self.config.retention_max_runs;
        let max_age = self.config.retention_max_age_seconds;
        if max_runs == 0 && max_age == 0 {
            return Ok(0);
        }

        // (modified, path), oldest first.
        let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        let mut entries = tokio::fs::read_dir(root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let meta = match entry.metadata().await {
                Ok(m) if m.is_dir() => m,
                _ => continue,
            };
            let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            dirs.push((modified, entry.path()));
        }
        dirs.sort_by_key(|(t, _)| *t);

        let mut doomed: Vec<PathBuf> = Vec::new();
        if max_age > 0 {
            let cutoff = Duration::from_secs(max_age);
            let now = std::time::SystemTime::now();
            for (modified, path) in &dirs {
                if now
                    .duration_since(*modified)
                    .map(|age| age > cutoff)
                    .unwrap_or(false)
                {
                    doomed.push(path.clone());
                }
            }
        }
        if max_runs > 0 && dirs.len() > max_runs {
            // Oldest first, so the surviving set is the most recent `max_runs`.
            for (_, path) in &dirs[..dirs.len() - max_runs] {
                if !doomed.contains(path) {
                    doomed.push(path.clone());
                }
            }
        }

        let mut removed = 0;
        for path in doomed {
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => removed += 1,
                // A reaper that aborts on one undeletable directory would stop
                // reclaiming space entirely; log and continue.
                Err(e) => warn!("could not remove run directory {}: {e}", path.display()),
            }
        }
        Ok(removed)
    }

    fn audit_failed(&self, run_id: Option<&str>, user_id: &str, reason: String) {
        if let Some(audit) = &self.audit {
            audit.log(AuditEvent::QuarryRunFailed {
                run_id: run_id.map(str::to_string),
                user_id: user_id.to_string(),
                reason,
            });
        }
    }
}

// ── Waiting ───────────────────────────────────────────────────────────────────

/// Why the supervisor stopped the child, if it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Forced {
    /// The child ended on its own.
    No,
    Timeout,
    Cancelled,
}

/// Wait for the child, killing it on timeout or cancellation.
///
/// Returns the exit status where one is available. After a kill the status is the
/// signal-terminated one, which would classify as `KilledBySignal` — so the
/// `Forced` reason is returned alongside and takes precedence in [`classify`].
/// Reporting our own kill as a signal death would hide *why* the run ended, and
/// specifically would lose that it was time and not money.
async fn wait_with_limits(
    child: &mut tokio::process::Child,
    timeout: Option<Duration>,
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
) -> (Option<std::process::ExitStatus>, Forced) {
    // A cancel channel that was never provided must never resolve. `pending()`
    // rather than a dummy receiver, whose sender would drop immediately and fire
    // the branch at once.
    let cancelled = async {
        match cancel {
            Some(rx) => {
                let _ = rx.await;
            }
            None => std::future::pending::<()>().await,
        }
    };
    let timed_out = async {
        match timeout {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending::<()>().await,
        }
    };

    tokio::select! {
        status = child.wait() => (status.ok(), Forced::No),
        _ = timed_out => {
            let _ = child.kill().await;
            (child.wait().await.ok(), Forced::Timeout)
        }
        _ = cancelled => {
            let _ = child.kill().await;
            (child.wait().await.ok(), Forced::Cancelled)
        }
    }
}

// ── Stream reading ────────────────────────────────────────────────────────────

/// How long to keep draining a pipe after the child has gone.
///
/// Non-zero because a child that exits normally may still have bytes in flight, and
/// dropping them would lose the receipt. Bounded because a killed child's
/// descendants can hold the pipe open indefinitely, and waiting forever there would
/// make the run timeout meaningless.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Everything a stdout reader accumulates, so an abandoned read keeps its progress.
#[derive(Debug, Default)]
struct StdoutAcc {
    stats: StreamStats,
    events_seen: Vec<RunEvent>,
    line_no: usize,
}

type SharedStdout = Arc<std::sync::Mutex<StdoutAcc>>;
type SharedStderr = Arc<std::sync::Mutex<String>>;

/// Wait for both readers, giving up after [`DRAIN_GRACE`].
///
/// Returns whether the stdout drain was abandoned. Only stdout's abandonment is
/// reported: it carries the events, so an incomplete read there changes what a
/// caller may conclude from the counts. Truncated stderr is diagnostic text, and
/// losing its tail costs nothing a caller reasons about.
async fn drain_grace(
    mut stdout_task: tokio::task::JoinHandle<()>,
    mut stderr_task: tokio::task::JoinHandle<()>,
    run_id: &str,
) -> bool {
    // `abort()` rather than dropping the handle: a dropped `JoinHandle` *detaches*
    // its task, which would leave the reader alive holding the read end of the pipe
    // for as long as the grandchild lives. Aborting closes it.
    let stdout_done = tokio::select! {
        joined = &mut stdout_task => {
            if let Err(e) = joined {
                // The reader task itself panicked — a bug here, not a fault in the
                // child. It must not discard the run: whatever was parsed before
                // the panic is still in the shared accumulator.
                error!(run_id = %run_id, "stdout reader task failed: {e}");
            }
            true
        }
        _ = tokio::time::sleep(DRAIN_GRACE) => {
            stdout_task.abort();
            false
        }
    };
    tokio::select! {
        _ = &mut stderr_task => {}
        _ = tokio::time::sleep(DRAIN_GRACE) => {
            stderr_task.abort();
            debug!(run_id = %run_id, "stopped reading quarry stderr before EOF");
        }
    }
    !stdout_done
}

/// Read NDJSON events from the child's stdout, forwarding each as it arrives.
///
/// Incremental by construction: [`AsyncBufReadExt::read_line`] yields on every
/// newline, so a subscriber sees an event while the run is still going. Collecting
/// to EOF first would make a live tree impossible — and, since EOF is not
/// guaranteed to arrive at all, would risk losing the whole stream.
///
/// Accumulates into `acc` rather than returning, so a caller that abandons this
/// task still has everything it parsed.
async fn read_event_stream(
    stdout: tokio::process::ChildStdout,
    sink: Option<mpsc::UnboundedSender<RunEvent>>,
    acc: SharedStdout,
) {
    let mut reader = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = reader.next_line().await {
        // quarry's encoder emits one object per line and nothing else, but a blank
        // line is not a contract break and must not be counted as a bad one.
        let blank = line.trim().is_empty();
        let parsed = if blank { None } else { Some(parse_line(&line)) };

        // The lock is taken per line and released before the next await, so a
        // caller reading the accumulator mid-run is never blocked behind the pipe.
        let mut acc = match acc.lock() {
            Ok(a) => a,
            // A poisoned mutex means a previous holder panicked. Dropping the rest
            // of the stream is worse than continuing without recording it.
            Err(e) => e.into_inner(),
        };
        acc.line_no += 1;
        if blank {
            continue;
        }
        acc.stats.lines += 1;
        match parsed.expect("non-blank lines are parsed") {
            Ok(event) => {
                acc.stats.events += 1;
                if let RunEvent::Unknown { event_type, .. } = &event {
                    *acc.stats
                        .unknown_kinds
                        .entry(event_type.clone())
                        .or_insert(0) += 1;
                }
                if let Some(tx) = &sink {
                    // A closed receiver is not an error: the caller stopped
                    // watching. The run continues and the event is still recorded.
                    let _ = tx.send(event.clone());
                }
                acc.events_seen.push(event);
            }
            // One bad line is skipped and recorded; the run continues. Only a
            // stream with no events at all is treated as a contract break.
            Err(e) => {
                let n = acc.line_no;
                acc.stats.bad_lines.push((n, e));
            }
        }
    }
}

/// Drain a pipe into a shared string.
async fn read_to_string(stderr: tokio::process::ChildStderr, acc: SharedStderr) {
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        let mut out = match acc.lock() {
            Ok(o) => o,
            Err(e) => e.into_inner(),
        };
        out.push_str(&line);
        out.push('\n');
    }
}

/// Read and parse quarry's record file, if it wrote one.
///
/// A missing or unreadable record is `None`, not an error: a crashed run may not
/// have written one, and that absence is itself part of the outcome.
async fn read_record(path: &Path) -> Option<RunRecordSummary> {
    let bytes = tokio::fs::read(path).await.ok()?;
    match serde_json::from_slice::<RunRecordSummary>(&bytes) {
        Ok(r) => Some(r),
        Err(e) => {
            warn!("quarry record at {} did not parse: {e}", path.display());
            None
        }
    }
}

// ── Classification ────────────────────────────────────────────────────────────

/// quarry's exit-code vocabulary.
///
/// Five codes, not two. An earlier reading of this integration assumed `main` mapped
/// every error to 1 and separated the outcomes by inspecting the events instead —
/// which classified a time-truncated run holding a perfectly good partial answer as a
/// crash. The codes are the contract; the events corroborate them.
///
/// The split that matters is between the two *degraded but honest* codes and the two
/// faults. 3 and 4 both write a citable record. 1 and 2 do not.
mod exit {
    /// Complete — **and cap-bound degradation**, which is a success: quarry planned
    /// to fit the cap it was given and did, so nothing went wrong.
    pub const COMPLETE: i32 = 0;
    /// A fault. Something broke that has nothing to do with what was asked.
    pub const FAULT: i32 = 1;
    /// A usage error: quarry refused the invocation. Our bug, not the run's.
    pub const USAGE: i32 = 2;
    /// Time-truncated: a deadline or latency cap bit. **There may still be an
    /// answer**, and if there is, it is the answer the system promised to return.
    pub const TIME_TRUNCATED: i32 = 3;
    /// No answer at all. The record still lands and is still citable.
    pub const NO_ANSWER: i32 = 4;
}

/// What to report when `wait` itself failed and no code exists.
const EXIT_UNKNOWN: i32 = -1;

/// Decide how a run ended.
///
/// The order is the argument. Each step is a distinction that collapses if the one
/// above it is allowed to answer first:
///
/// 1. **Our own kill wins.** A child we killed looks signal-terminated, which would
///    report the mechanism instead of the reason — and would lose that the reason
///    was time.
/// 2. **A signal death is not a non-zero exit.** Nothing in the child chose it, so
///    its own error handling never ran and no code exists to read.
/// 3. **Version before content.** If the stream declares a contract we do not
///    implement, every field below is suspect and reading them would produce a
///    confident wrong receipt. Refuse, do not fold.
/// 4. **No events at all is a contract break**, distinct from the individual bad
///    lines that are skipped while the run continues.
/// 5. **The terminal outcome event is the authority.** It carries quarry's own
///    verdict — `outcome`, `bound_by`, `gaps`, `unfunded`, and the money as
///    integers — so where it exists we take it over anything we could infer.
/// 6. **Its absence means the run was killed.** NDJSON yields complete lines
///    whether or not the producer finished, so a stream cut off after the artifact
///    is byte-indistinguishable from a clean one *except* for this absence. It must
///    never default to complete.
/// 7. **The record is the fallback**, for a stream that predates the frame.
///
/// Exit codes are read as the documented five, and 3 is the one that used to be a
/// bug: a time-truncated run holding a usable partial answer exits 3, and reporting
/// that as a crash discards exactly the result quarry promises to return.
fn classify(
    forced: Forced,
    status: Option<std::process::ExitStatus>,
    events: &[RunEvent],
    stats: &StreamStats,
    record: Option<&RunRecordSummary>,
    duration: Duration,
) -> Termination {
    match forced {
        Forced::Timeout => return Termination::TimedOut { after: duration },
        Forced::Cancelled => return Termination::Cancelled,
        Forced::No => {}
    }

    let Some(status) = status else {
        // The wait itself failed, so nothing is known about how the child ended.
        // Reporting this as a clean completion would be a guess in the dangerous
        // direction.
        return Termination::Crashed {
            exit_code: EXIT_UNKNOWN,
        };
    };

    let Some(code) = status.code() else {
        return Termination::KilledBySignal {
            signal: signal_of(&status),
        };
    };

    // Before anything is read out of the stream: does it claim a contract we
    // implement? quarry's rule is that a new event *kind* is minor and must be
    // tolerated, while a changed field or a changed meaning is major and bumps this
    // number. So an unknown version is by definition a change that skipping cannot
    // absorb — the events we still recognise may no longer mean what we read them to
    // mean. Checked ahead of the exit code because a zero exit says nothing about
    // whether we understood the bytes.
    if let Some(declared) = event::stream_version(events) {
        if declared != event::SUPPORTED_STREAM_VERSION {
            return Termination::StreamVersionUnsupported { declared };
        }
    }

    // A usage error is ours, and it is not a run: quarry refused the invocation
    // before attempting anything, so there is no stream to interpret and no record
    // to cite. Kept above the event checks because the empty stream here is a
    // consequence, not a contract break.
    if code == exit::USAGE {
        return Termination::UsageError;
    }

    // No parseable event at all: the wrong binary, a quarry that no longer emits
    // this stream — or, as this integration shipped once, `--events-json` never
    // passed, in which case quarry writes a human summary and emits nothing.
    // Distinct from `stats.bad_lines`, which are individually recoverable: a caller
    // must not retry a contract break the way it retries a transient fault.
    if stats.events == 0 {
        return Termination::StreamMalformed;
    }

    let has_answer = events
        .iter()
        .any(|e| matches!(e, RunEvent::Answer(a) if !a.text.trim().is_empty()));

    // The frame's own verdict, when the frame is closed. Scanned backwards, because
    // a future event kind is permitted to follow the outcome.
    if let Some(outcome) = event::terminal_outcome(events) {
        return classify_from_outcome(outcome, code, has_answer);
    }

    // No terminal event. Whether that is a killed run turns on whether the frame was
    // ever *opened*: a `quarry_stream` header with no `quarry_outcome` closer is a
    // stream cut off mid-run, while a stream with neither is not framed at all — an
    // older quarry, or agate's four events read directly — and there was never a
    // closer to miss.
    if event::stream_version(events).is_some() {
        // The frame opened and never closed. A fault still reports as a fault: the
        // exit code is real information and more specific than "cut off". Every other
        // code, **including zero**, is a killed run, because a clean stream and a
        // severed one are byte-identical except for the absence we just found.
        if code == exit::FAULT {
            return Termination::Crashed { exit_code: code };
        }
        return Termination::StreamIncomplete {
            events_read: stats.events,
        };
    }

    // Unframed. This is the pre-frame path: the record is the authority, which is
    // what this integration read before the terminal event existed.
    if code == exit::FAULT {
        return Termination::Crashed { exit_code: code };
    }
    if let Some(record) = record {
        if record.truncated() {
            return Termination::Truncated {
                bound_by: non_empty(&record.bound_by),
            };
        }
    }
    match code {
        exit::TIME_TRUNCATED => Termination::Truncated {
            // No frame and no record to name the denomination. Exit 3 is time by
            // definition, so `latency` is the honest floor — never `spend`, which
            // would send a caller to raise a cap that was not what ran out.
            bound_by: Some("latency".to_string()),
        },
        exit::NO_ANSWER => Termination::NoAnswer,
        // Zero exit with no answer event. Reporting it as `Completed` would present
        // an empty result as an answer.
        exit::COMPLETE if !has_answer => Termination::NoAnswer,
        exit::COMPLETE => Termination::Completed,
        other => Termination::Crashed { exit_code: other },
    }
}

/// Read the terminal event's verdict, with the exit code as a cross-check.
///
/// quarry's `outcome` field and its exit code are two encodings of the same
/// decision, and they agree in every fixture. Where the field is missing or a value
/// we do not know, the code decides — a host that trusted only the string would
/// report a future outcome name as complete.
///
/// The money is **not** touched here. `total_micros` and `cap_micros` are already
/// integers on the wire and are read straight off it; re-deriving them from the
/// float rows is the mistake the corpus exists to catch, and `cap_micros: -1` means
/// no cap rather than a cap of nothing.
fn classify_from_outcome(outcome: &OutcomeEvent, code: i32, has_answer: bool) -> Termination {
    match outcome.outcome.as_str() {
        // Both are successes, and deliberately indistinguishable here: quarry plans
        // to fit the cap it was given, so being priced out of a branch is the plan
        // working, disclosed before the run and inside the authority granted. It is
        // reported in the receipt as `unfunded`, never as a gap and never as a
        // failure.
        "complete" | "cap-bound-degradation" => Termination::Completed,
        // Time. There may be an answer, and if there is, it is the promise being
        // kept, not broken.
        "time-truncated" => Termination::Truncated {
            bound_by: Some(non_empty(&outcome.bound_by).unwrap_or_else(|| "latency".to_string())),
        },
        "no-answer" => Termination::NoAnswer,
        // A value this build has never seen. Deciding from the code is the safe
        // direction: it cannot invent an answer that is not there.
        _ => match code {
            exit::COMPLETE if has_answer => Termination::Completed,
            exit::COMPLETE => Termination::NoAnswer,
            exit::TIME_TRUNCATED => Termination::Truncated {
                bound_by: Some(
                    non_empty(&outcome.bound_by).unwrap_or_else(|| "latency".to_string()),
                ),
            },
            exit::NO_ANSWER => Termination::NoAnswer,
            other => Termination::Crashed { exit_code: other },
        },
    }
}

/// `""` is quarry's "no cap bound this run", and it is emitted because it is a
/// measurement rather than an omission. `None` carries that distinction; an empty
/// `Some` would read as a denomination named the empty string.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// The signal that killed a process, on platforms that report one.
#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quarry::event::{
        AnswerEvent, ArtifactEvent, ReceiptEvent, StreamEvent, SUPPORTED_STREAM_VERSION,
    };

    // ── Argument rendering ────────────────────────────────────────────────────

    #[test]
    fn cap_is_rendered_at_six_decimal_places() {
        // quarry parses --cap with Sscanf("%g") and converts with FromFloat. A
        // bare `{}` on 1.5e-6 renders "0.0000015" in Rust but exponent form for
        // some values, and either way a coarser format would floor a micro-unit
        // cap to zero — which quarry reads as "no cap" rather than "a tiny one".
        let mut r = RunRequest::new("u", "c", "q", 1);
        let args = r.to_args();
        let cap = &args[args.iter().position(|a| a == "--cap").unwrap() + 1];
        assert_eq!(cap, "0.000001", "one micro-dollar must survive rendering");

        r.spend_micro_usd = Some(5_500_000);
        let args = r.to_args();
        let cap = &args[args.iter().position(|a| a == "--cap").unwrap() + 1];
        assert_eq!(cap, "5.500000");
    }

    #[test]
    fn deadline_is_rendered_in_milliseconds() {
        // Go's ParseDuration needs a unit. Seconds alone would floor a 500ms
        // deadline to "0s", which quarry treats as no latency cap at all.
        let mut r = RunRequest::new("u", "c", "q", 1_000_000);
        r.deadline = Some(Duration::from_millis(500));
        let args = r.to_args();
        let d = &args[args.iter().position(|a| a == "--deadline").unwrap() + 1];
        assert_eq!(d, "500ms");
    }

    #[test]
    fn scope_tags_render_canonically_sorted() {
        // Scope tags are folded into every cache key. Two hosts that ordered them
        // differently would compute different keys for the same scope, so the
        // ordering is a correctness property rather than cosmetics.
        let mut r = RunRequest::new("u", "c", "q", 1_000_000);
        r.scope_tags.insert("tenant".into(), "acme".into());
        r.scope_tags.insert("channel".into(), "discord".into());
        r.scope_tags.insert("sender".into(), "u1".into());
        let args = r.to_args();
        let scope = &args[args.iter().position(|a| a == "--scope").unwrap() + 1];
        assert_eq!(scope, "channel=discord,sender=u1,tenant=acme");
    }

    #[test]
    fn statement_is_last_so_flags_are_not_swallowed() {
        // quarry's flag parser stops at the first non-flag argument. A flag placed
        // after the statement would be parsed as part of it.
        let r = RunRequest::new("u", "c", "explain X", 1_000_000);
        let args = r.to_args();
        assert_eq!(args[0], "run");
        assert_eq!(args.last().unwrap(), "explain X");
        assert!(args.contains(&"--quiet".to_string()));
    }

    #[test]
    fn a_statement_that_looks_like_a_flag_still_lands_last() {
        // Not a shell: args are passed as a vector, so no quoting is involved and
        // a leading dash cannot split into new arguments.
        let r = RunRequest::new("u", "c", "--cap 99999", 1_000_000);
        let args = r.to_args();
        assert_eq!(args.last().unwrap(), "--cap 99999");
        assert_eq!(
            args.iter().filter(|a| *a == "--cap").count(),
            1,
            "the statement must not become a second --cap flag"
        );
    }

    // ── Credential hygiene ────────────────────────────────────────────────────

    #[test]
    fn provider_and_channel_credentials_are_stripped_from_the_child_env() {
        let mut env = BTreeMap::new();
        env.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-secret".to_string());
        env.insert("DISCORD_BOT_TOKEN".to_string(), "discord".to_string());
        env.insert("AWS_SECRET_ACCESS_KEY".to_string(), "aws".to_string());
        env.insert("GATEWAY_API_TOKEN".to_string(), "gw".to_string());
        env.insert(
            "QUARRY_PROVIDER_URL".to_string(),
            "http://127.0.0.1:8080".into(),
        );

        let (clean, rejected) = sanitize_env(&env);
        assert_eq!(rejected.len(), 3);
        assert!(!clean.contains_key("ANTHROPIC_API_KEY"));
        assert!(!clean.contains_key("DISCORD_BOT_TOKEN"));
        assert!(!clean.contains_key("AWS_SECRET_ACCESS_KEY"));
        // The two the child legitimately needs: the localhost endpoint and its
        // bearer token. That token is the only credential quarry receives.
        assert_eq!(clean.get("GATEWAY_API_TOKEN").unwrap(), "gw");
        assert!(clean.contains_key("QUARRY_PROVIDER_URL"));
    }

    #[test]
    fn forbidden_keys_are_matched_case_insensitively() {
        // Env lookup is case-sensitive on unix, but a caller building the map from
        // a config file could easily produce a different casing, and the value
        // would be just as much a secret.
        let mut env = BTreeMap::new();
        env.insert("anthropic_api_key".to_string(), "sk".to_string());
        let (clean, rejected) = sanitize_env(&env);
        assert!(clean.is_empty());
        assert_eq!(rejected, vec!["anthropic_api_key"]);
    }

    // ── Termination semantics ─────────────────────────────────────────────────

    fn answer(text: &str) -> RunEvent {
        RunEvent::Answer(AnswerEvent {
            title: String::new(),
            text: text.to_string(),
        })
    }

    fn artifact() -> RunEvent {
        RunEvent::Artifact(ArtifactEvent {
            run_id: "abc".into(),
            url: String::new(),
            provenance: None,
        })
    }

    /// The frame's opening line.
    fn stream_header(version: u32) -> RunEvent {
        RunEvent::Stream(StreamEvent {
            version,
            producer: "quarry-go".to_string(),
        })
    }

    /// The frame's closing line — quarry's own verdict on the run.
    fn outcome(outcome: &str, bound_by: &str, gaps: u64, unfunded: u64) -> RunEvent {
        RunEvent::Outcome(OutcomeEvent {
            outcome: outcome.to_string(),
            bound_by: bound_by.to_string(),
            gaps,
            unfunded,
            ..Default::default()
        })
    }

    fn stats_with(events: usize) -> StreamStats {
        StreamStats {
            lines: events,
            events,
            ..Default::default()
        }
    }

    fn record_json(bound_by: &str, outcomes: serde_json::Value) -> RunRecordSummary {
        serde_json::from_value(serde_json::json!({
            "RunID": "abc", "BoundBy": bound_by, "Outcomes": outcomes,
        }))
        .unwrap()
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    fn signal_status(sig: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(sig)
    }

    #[test]
    #[cfg(unix)]
    fn our_own_timeout_is_time_truncation_never_budget_degradation() {
        // The load-bearing distinction. We killed the child, so it looks
        // signal-terminated — but reporting that would name the mechanism instead
        // of the reason, and would lose that the reason was TIME. A caller told
        // "priced out" would raise the spend cap and buy nothing.
        let t = classify(
            Forced::Timeout,
            Some(signal_status(9)),
            &[],
            &StreamStats::default(),
            None,
            Duration::from_secs(30),
        );
        assert_eq!(
            t,
            Termination::TimedOut {
                after: Duration::from_secs(30)
            }
        );
        assert!(t.time_truncated(), "our timeout is time truncation");
        assert!(
            !t.spend_truncated(),
            "and must never read as budget degradation"
        );
        assert_eq!(t.code(), "timed_out");
    }

    #[test]
    #[cfg(unix)]
    fn cancellation_is_time_truncation_too() {
        // The tree held a returnable partial answer and we stopped it. quarry
        // makes the same choice for ^C.
        let t = classify(
            Forced::Cancelled,
            Some(signal_status(9)),
            &[answer("partial")],
            &stats_with(1),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Cancelled);
        assert!(t.time_truncated());
        assert!(!t.spend_truncated());
        assert!(
            t.produced_record(),
            "a cancelled run still holds a partial tree"
        );
    }

    #[test]
    #[cfg(unix)]
    fn spend_truncation_is_read_from_the_record_and_is_not_a_gap() {
        // quarry exits ZERO here: a partial answer with its gaps named is what the
        // system promises to return. The record — not the event count — says the
        // spend cap bit.
        let record = record_json("spend", serde_json::json!([]));
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("partial"), artifact()],
            &stats_with(2),
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(
            t,
            Termination::Truncated {
                bound_by: Some("spend".into())
            }
        );
        assert!(t.spend_truncated());
        assert!(
            !t.time_truncated(),
            "money is not time: offering this run more time buys nothing"
        );
        assert!(
            t.produced_record(),
            "a truncated run is a legitimate result"
        );
    }

    #[test]
    #[cfg(unix)]
    fn latency_truncation_from_the_record_is_time_not_money() {
        let record = record_json("latency", serde_json::json!([]));
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("partial"), artifact()],
            &stats_with(2),
            Some(&record),
            Duration::from_secs(1),
        );
        assert!(t.time_truncated());
        assert!(!t.spend_truncated());
    }

    #[test]
    #[cfg(unix)]
    fn an_unfunded_node_truncates_even_with_no_bound_by() {
        // quarry's Truncated() is broader than Gaps() precisely for this: a run
        // that dropped children it could not afford has no gaps at all, and may
        // have no BoundBy either, while being the clearest case of a run that did
        // not finish.
        let record = record_json(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"","Cost":0,"Gap":false,"Model":"","Verified":null},
            ]),
        );
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("partial"), artifact()],
            &stats_with(2),
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Truncated { bound_by: None });
        assert!(
            !t.time_truncated() && !t.spend_truncated(),
            "an unnamed denomination must not be guessed in either direction"
        );
    }

    #[test]
    #[cfg(unix)]
    fn crash_is_not_completion_and_not_no_answer() {
        // Exit 1 is the fault code, and with no record and no closed frame there is
        // nothing to say the child got as far as deciding anything about the work.
        let t = classify(
            Forced::No,
            Some(exit_status(1)),
            &[RunEvent::Receipt(ReceiptEvent::default())],
            &stats_with(1),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Crashed { exit_code: 1 });
        assert!(!t.produced_record(), "a crash leaves nothing citable");
        assert!(!t.time_truncated() && !t.spend_truncated());
    }

    #[test]
    #[cfg(unix)]
    fn a_usage_error_is_our_bug_and_not_a_crash() {
        // Exit 2 means quarry refused the invocation: the args we built are wrong.
        // Kept out of `Crashed` because a crash invites a retry and this one would
        // fail identically forever. Nothing was attempted, so the empty stream here
        // is a consequence and must not surface as a contract break either.
        let t = classify(
            Forced::No,
            Some(exit_status(2)),
            &[],
            &StreamStats::default(),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::UsageError);
        assert_ne!(t, Termination::StreamMalformed);
        assert!(!t.produced_record());
    }

    #[test]
    #[cfg(unix)]
    fn exit_four_with_a_record_is_no_answer_not_a_crash() {
        // No-answer has its own exit code, 4, and it still WRITES a record — one
        // that faithfully says nothing was affordable, which is a useful artifact.
        // Classifying it as a crash would discard a valid record.
        let record = record_json("", serde_json::json!([]));
        let t = classify(
            Forced::No,
            Some(exit_status(4)),
            &[artifact()],
            &stats_with(1),
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::NoAnswer);
        assert!(t.produced_record(), "the record is still citable");
    }

    #[test]
    #[cfg(unix)]
    fn exit_three_with_a_partial_answer_is_time_truncation_not_a_crash() {
        // The bug this whole classifier rewrite exists for. quarry exits 3 when a
        // deadline or latency cap bit, and that run may hold a perfectly good
        // partial answer — which is the result the system promises to return, not a
        // failure. Reporting it as a crash threw the answer away.
        //
        // With no frame and no record to name the denomination, `latency` is the
        // honest floor. What must never happen is `spend`: a caller told "priced
        // out" would raise a cap and buy nothing, because what ran out was time.
        let t = classify(
            Forced::No,
            Some(exit_status(3)),
            &[answer("as far as we got"), artifact()],
            &stats_with(2),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(
            t,
            Termination::Truncated {
                bound_by: Some("latency".to_string())
            }
        );
        assert!(t.time_truncated(), "exit 3 is time, by definition");
        assert!(!t.spend_truncated());
        assert!(t.produced_record());
    }

    #[test]
    #[cfg(unix)]
    fn the_terminal_outcome_event_outranks_the_exit_code() {
        // The frame carries quarry's own verdict, so where it exists we take it. A
        // cap-bound-degradation run exits 0 and IS a success: quarry planned to fit
        // the cap it was given and did. Being priced out of a branch is the plan
        // working, disclosed before the run and inside the authority granted.
        //
        // `bound_by` is EMPTY here, matching the `live-partition` fixture. quarry does
        // not name spend as having "bound" a run it planned to fit — which is why
        // `Truncated { bound_by: Some("spend") }` is reachable only on the unframed
        // record path below, never from a frame.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[
                stream_header(1),
                answer("the affordable part"),
                outcome("cap-bound-degradation", "", 0, 5),
            ],
            &stats_with(3),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Completed);
        assert!(
            !t.spend_truncated(),
            "planned degradation inside authority is not a truncation"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_frame_that_opens_and_never_closes_is_a_killed_run_even_on_exit_zero() {
        // NDJSON yields complete lines whether or not the producer finished, so a
        // stream cut off after the artifact is byte-identical to a clean one EXCEPT
        // for the missing terminal event. That absence is the only in-band kill
        // signal there is, and defaulting it to `Completed` would publish a receipt
        // for a run that never finished.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[stream_header(1), answer("looks complete"), artifact()],
            &stats_with(3),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::StreamIncomplete { events_read: 3 });
        assert_ne!(t, Termination::Completed);
        assert!(!t.produced_record(), "nothing here is citable");
    }

    #[test]
    #[cfg(unix)]
    fn an_unframed_stream_is_not_reported_as_cut_off() {
        // The vacuity guard for the test above. Without a `quarry_stream` header
        // there was never a closer to miss — an older quarry, or agate's four events
        // read directly — so the record path still decides. If this asserted
        // `StreamIncomplete` too, the test above would prove nothing about the frame.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("looks complete"), artifact()],
            &stats_with(2),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Completed);
    }

    #[test]
    #[cfg(unix)]
    fn a_stream_declaring_an_unknown_version_is_refused_not_folded() {
        // The version is on the first line precisely so a host can refuse. quarry's
        // rule: a new event KIND is minor and must be tolerated, but a changed field
        // or a changed meaning is major and bumps this number. So the events we still
        // recognise here may no longer mean what we read them to mean, and folding
        // them would produce a confident wrong receipt.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[
                stream_header(SUPPORTED_STREAM_VERSION + 1),
                answer("do not trust me"),
                outcome("complete", "", 0, 0),
            ],
            &stats_with(3),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(
            t,
            Termination::StreamVersionUnsupported {
                declared: SUPPORTED_STREAM_VERSION + 1
            }
        );
        assert!(
            !t.produced_record(),
            "a stream we cannot read yields nothing citable, even with an outcome saying complete"
        );
    }

    #[test]
    #[cfg(unix)]
    fn the_terminal_event_is_found_by_scanning_backwards() {
        // Tolerating a new event kind means tolerating one AFTER the outcome. A
        // forward scan that stopped at the last event, or one that required the
        // outcome to be last, would report a complete run as cut off the first time
        // quarry appends anything.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[
                stream_header(1),
                answer("done"),
                outcome("complete", "", 0, 0),
                RunEvent::Unknown {
                    event_type: "quarry_future_kind".to_string(),
                    raw: serde_json::json!({"type":"quarry_future_kind"}),
                },
            ],
            &stats_with(4),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Completed);
    }

    #[test]
    #[cfg(unix)]
    fn a_time_truncated_outcome_names_its_own_denomination() {
        // `bound_by` comes off the wire rather than being inferred. A `due` bound is
        // still time, and must not be flattened into `latency`: they are different
        // things to tell a caller — one missed a deadline, the other ran out of
        // allotted wall-clock.
        let t = classify(
            Forced::No,
            Some(exit_status(3)),
            &[
                stream_header(1),
                answer("partial"),
                outcome("time-truncated", "due", 2, 0),
            ],
            &stats_with(3),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(
            t,
            Termination::Truncated {
                bound_by: Some("due".to_string())
            }
        );
        assert!(t.time_truncated() && !t.spend_truncated());
    }

    #[test]
    #[cfg(unix)]
    fn an_unknown_outcome_name_falls_back_to_the_exit_code() {
        // A future outcome string must not read as complete. The code is the safe
        // direction to decide from: it cannot invent an answer that is not there.
        let t = classify(
            Forced::No,
            Some(exit_status(4)),
            &[stream_header(1), outcome("some-future-outcome", "", 0, 0)],
            &stats_with(2),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::NoAnswer);
        assert_ne!(t, Termination::Completed);
    }

    #[test]
    #[cfg(unix)]
    fn an_unlimited_cap_is_negative_one_and_not_a_cap_of_nothing() {
        // The `deadline-only` case. `cap_micros: -1` means no spend cap; reading it
        // as zero would report a run that spent anything as having blown its budget.
        let unlimited = OutcomeEvent {
            cap_micros: -1,
            total_micros: 360_000,
            ..OutcomeEvent::default()
        };
        assert!(!unlimited.has_spend_cap());
        let capped = OutcomeEvent {
            cap_micros: 0,
            ..OutcomeEvent::default()
        };
        assert!(
            capped.has_spend_cap(),
            "a cap of zero is a real cap that funds nothing, not the absence of one"
        );
    }

    #[test]
    #[cfg(unix)]
    fn an_empty_answer_event_does_not_count_as_an_answer() {
        // quarry omits the answer event when the root produced nothing, so its
        // absence is meaningful — but a whitespace-only one must not be treated as
        // a result either.
        let record = record_json("", serde_json::json!([]));
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("   "), artifact()],
            &stats_with(2),
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::NoAnswer);
    }

    #[test]
    #[cfg(unix)]
    fn signal_death_is_distinct_from_a_non_zero_exit() {
        // Nothing in the child chose this, so its own error reporting never ran —
        // which is exactly why it should not read as a crash it diagnosed itself.
        let t = classify(
            Forced::No,
            Some(signal_status(9)),
            &[answer("partial")],
            &stats_with(1),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::KilledBySignal { signal: Some(9) });
        assert!(!t.produced_record());
    }

    #[test]
    #[cfg(unix)]
    fn no_events_at_all_is_a_contract_break_even_on_a_clean_exit() {
        // Wrong binary, or a quarry that no longer emits this stream. Kept
        // separate from the bad lines that are skipped mid-stream, so a caller
        // does not retry a contract break the way it retries a transient fault.
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[],
            &StreamStats::default(),
            None,
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::StreamMalformed);
        assert!(!t.produced_record());
    }

    #[test]
    #[cfg(unix)]
    fn bad_lines_alone_do_not_fail_a_run() {
        // The counterpart to the test above: an individually unparseable line is
        // skipped and recorded, and the run still completes.
        let record = record_json("", serde_json::json!([]));
        let mut stats = stats_with(2);
        stats.lines = 3;
        stats
            .bad_lines
            .push((2, crate::quarry::event::LineError::NotObject));
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("done"), artifact()],
            &stats,
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Completed);
        assert!(
            !stats.clean(),
            "but the skip is still visible to the caller"
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_clean_run_completes() {
        let record = record_json(
            "",
            serde_json::json!([
                {"NodeID":"n0","Content":"done","Cost":150,"Model":"m-1","Verified":true},
            ]),
        );
        let t = classify(
            Forced::No,
            Some(exit_status(0)),
            &[answer("done"), artifact()],
            &stats_with(2),
            Some(&record),
            Duration::from_secs(1),
        );
        assert_eq!(t, Termination::Completed);
        assert!(t.produced_record());
        assert!(!t.time_truncated() && !t.spend_truncated());
    }

    #[test]
    fn termination_codes_are_unique() {
        // Callers classify on `code()`. Two outcomes sharing a slug would be the
        // overloaded-status defect this whole type exists to avoid.
        let all = [
            Termination::Completed,
            Termination::Truncated { bound_by: None },
            Termination::NoAnswer,
            Termination::TimedOut {
                after: Duration::ZERO,
            },
            Termination::Cancelled,
            Termination::Crashed { exit_code: 1 },
            Termination::KilledBySignal { signal: None },
            Termination::StreamMalformed,
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|t| t.code()).collect();
        assert_eq!(codes.len(), all.len());
    }

    // ── Concurrency ───────────────────────────────────────────────────────────

    fn config(max: usize) -> QuarryConfig {
        QuarryConfig {
            enabled: true,
            max_concurrent_runs: max,
            run_timeout_seconds: 0,
            ..Default::default()
        }
    }

    #[test]
    fn the_concurrency_limit_actually_bounds_concurrency() {
        let s = Supervisor::new(config(2));
        let a = s.try_claim_slot().expect("first slot");
        let b = s.try_claim_slot().expect("second slot");
        assert_eq!(s.active_runs(), 2);
        // Refused, not queued: a queue turns a concurrency limit into unbounded
        // latency with no disclosure, and a deadline could expire before the run
        // even starts.
        assert_eq!(
            s.try_claim_slot().unwrap_err(),
            SpawnError::AtCapacity { limit: 2 }
        );
        drop(a);
        assert_eq!(s.active_runs(), 1);
        let _c = s.try_claim_slot().expect("a freed slot is reusable");
        drop(b);
        assert_eq!(s.active_runs(), 1);
    }

    #[test]
    fn a_slot_is_released_even_on_an_early_return() {
        // The guard exists because a run that returns through `?` — a missing
        // binary, an uncreatable directory — must still release its slot, or the
        // limit leaks downward until nothing can start.
        let s = Supervisor::new(config(1));
        {
            let _g = s.try_claim_slot().unwrap();
            assert_eq!(s.active_runs(), 1);
        }
        assert_eq!(s.active_runs(), 0);
        assert!(s.try_claim_slot().is_ok());
    }

    #[tokio::test]
    async fn a_disabled_supervisor_refuses_before_spawning() {
        let s = Supervisor::new(QuarryConfig::default());
        assert!(!s.enabled());
        let err = s
            .run(RunRequest::new("u", "c", "q", 1_000_000), None, None)
            .await
            .unwrap_err();
        assert_eq!(err, SpawnError::Disabled);
        assert_eq!(s.active_runs(), 0, "a refused run takes no slot");
    }

    #[tokio::test]
    async fn an_uncapped_run_is_refused_with_quarrys_reason() {
        // quarry refuses this itself, but spawning a process to have it reject our
        // own arguments wastes the spawn and buries the reasoning in stderr.
        let s = Supervisor::new(config(1));
        let mut r = RunRequest::new("u", "c", "q", 1_000_000);
        r.spend_micro_usd = None;
        r.deadline = None;
        let err = s.run(r, None, None).await.unwrap_err();
        assert_eq!(err, SpawnError::NoCap);
        assert!(err.to_string().contains("budget-conditioned"));
        assert_eq!(s.active_runs(), 0);
    }

    #[tokio::test]
    async fn a_deadline_alone_is_a_sufficient_cap() {
        // A run may be bound by time instead of money, so requiring a spend cap
        // would refuse a legitimate request.
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            binary_path: "/nonexistent/quarry".into(),
            run_record_dir: std::env::temp_dir()
                .join("rustynail-quarry-deadline-test")
                .display()
                .to_string(),
            ..config(1)
        });
        let mut r = RunRequest::new("u", "c", "q", 1_000_000);
        r.spend_micro_usd = None;
        r.deadline = Some(Duration::from_secs(60));
        // It gets past the cap check, which is the point: NoCap did not reject it. It
        // then fails at the verification gate, because the named binary does not
        // exist — a *later* refusal, and the distinction the test is asserting.
        let err = s.run(r, None, None).await.unwrap_err();
        assert_eq!(err.code(), "binary_missing", "got {err:?}");
        assert_eq!(s.active_runs(), 0, "the failed spawn released its slot");
    }

    #[tokio::test]
    async fn a_missing_binary_is_refused_at_the_gate_before_any_spawn() {
        // The gate resolves and hashes the binary, so a missing one is caught there
        // rather than by `cmd.spawn()` — one refusal path, not two that could
        // disagree about whether verification ran.
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            binary_path: "/nonexistent/definitely-not-quarry".into(),
            run_record_dir: std::env::temp_dir()
                .join("rustynail-quarry-missing-bin-test")
                .display()
                .to_string(),
            ..config(1)
        });
        let err = s
            .run(RunRequest::new("u", "c", "q", 1_000_000), None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "binary_missing");
        assert_eq!(s.active_runs(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_verified_but_unexecutable_binary_still_reports_binary_unavailable() {
        // `BinaryUnavailable` survives the gate's arrival because the gate checks that
        // a file exists and hashes, not that the kernel will execute it: a
        // non-executable regular file passes verification and fails at `execve`. The
        // two errors are not redundant.
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("quarry");
        std::fs::write(&bin, b"not executable").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::write(
            tmp.path().join("quarry.manifest.json"),
            crate::quarry::verify::development_manifest_json(8080, "quarry-runs"),
        )
        .unwrap();

        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            binary_path: bin.display().to_string(),
            run_record_dir: "quarry-runs".to_string(),
            verification: crate::quarry::verify::development_config(),
            ..config(1)
        })
        .with_gateway_port(8080);
        let err = s
            .run(RunRequest::new("u", "c", "q", 1_000_000), None, None)
            .await
            .unwrap_err();
        assert_eq!(err.code(), "binary_unavailable", "got {err:?}");
        assert_eq!(s.active_runs(), 0);
        let _ = std::fs::remove_dir_all("quarry-runs");
    }

    #[test]
    fn spawn_error_codes_are_unique() {
        let all = [
            SpawnError::Disabled,
            SpawnError::AtCapacity { limit: 1 },
            SpawnError::NoCap,
            SpawnError::BinaryUnavailable {
                path: String::new(),
                detail: String::new(),
            },
            SpawnError::RunDirUnavailable {
                path: String::new(),
                detail: String::new(),
            },
            // `Unverified` delegates to the refusal's own code, so it contributes one
            // of the fifteen verification codes rather than a code of its own — which
            // is why the set below has one representative rather than all fifteen.
            SpawnError::Unverified {
                refusal: crate::quarry::verify::VerificationRefusal::Unsigned {
                    digest: String::new(),
                },
            },
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|e| e.code()).collect();
        assert_eq!(codes.len(), all.len());
    }

    #[test]
    fn a_spawn_error_tells_the_sender_nothing_about_the_hosts_configuration() {
        // Every refusal but AtCapacity is an operator problem, and a verification
        // refusal must additionally not leak a path or an identity regex.
        let all = [
            SpawnError::Disabled,
            SpawnError::NoCap,
            SpawnError::BinaryUnavailable {
                path: "/opt/secret/quarry".into(),
                detail: "no".into(),
            },
            SpawnError::RunDirUnavailable {
                path: "/srv/private/runs".into(),
                detail: "no".into(),
            },
            SpawnError::Unverified {
                refusal: crate::quarry::verify::VerificationRefusal::WrongIdentity {
                    digest: "deadbeef".into(),
                    expected: "quarry-release".into(),
                    found: "attacker".into(),
                },
            },
        ];
        for e in &all {
            let msg = e.sender_message();
            for leak in [
                "/opt/secret",
                "/srv/private",
                "deadbeef",
                "quarry-release",
                "attacker",
            ] {
                assert!(!msg.contains(leak), "{} leaks '{leak}': {msg}", e.code());
            }
        }
        assert!(
            SpawnError::AtCapacity { limit: 2 }
                .sender_message()
                .contains("again"),
            "capacity is transient and the sender can act on it"
        );
    }

    // ── Retention ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn retention_by_count_keeps_the_newest_runs() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["r1", "r2", "r3", "r4", "r5"] {
            let d = tmp.path().join(name);
            tokio::fs::create_dir_all(&d).await.unwrap();
            tokio::fs::write(d.join("record.json"), "{}").await.unwrap();
            // Distinct mtimes, so "oldest" is well defined rather than dependent
            // on directory iteration order.
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            run_record_dir: tmp.path().display().to_string(),
            retention_max_runs: 2,
            retention_max_age_seconds: 0,
            ..Default::default()
        });
        assert_eq!(s.reap_run_dirs().await.unwrap(), 3);
        assert!(!tmp.path().join("r1").exists());
        assert!(!tmp.path().join("r3").exists());
        assert!(tmp.path().join("r4").exists(), "the newest survive");
        assert!(tmp.path().join("r5").exists());
    }

    #[tokio::test]
    async fn retention_disabled_deletes_nothing() {
        // Both limits off is a legitimate operator choice — records archived
        // elsewhere. A reaper that deleted anyway would destroy citable artifacts.
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(tmp.path().join("r1"))
            .await
            .unwrap();
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            run_record_dir: tmp.path().display().to_string(),
            retention_max_runs: 0,
            retention_max_age_seconds: 0,
            ..Default::default()
        });
        assert_eq!(s.reap_run_dirs().await.unwrap(), 0);
        assert!(tmp.path().join("r1").exists());
    }

    #[tokio::test]
    async fn retention_by_age_removes_only_old_runs() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(tmp.path().join("fresh"))
            .await
            .unwrap();
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            run_record_dir: tmp.path().display().to_string(),
            retention_max_runs: 0,
            retention_max_age_seconds: 3600,
            ..Default::default()
        });
        assert_eq!(
            s.reap_run_dirs().await.unwrap(),
            0,
            "a run created seconds ago is not an hour old"
        );
        assert!(tmp.path().join("fresh").exists());
    }

    #[tokio::test]
    async fn reaping_a_missing_directory_is_not_an_error() {
        let s = Supervisor::new(QuarryConfig {
            enabled: true,
            run_record_dir: "/nonexistent/quarry-runs".into(),
            retention_max_runs: 1,
            ..Default::default()
        });
        assert_eq!(s.reap_run_dirs().await.unwrap(), 0);
    }
}
