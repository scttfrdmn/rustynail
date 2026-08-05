//! Natural-language caps parsing into quarry's `Caps`.
//!
//! A sender writes *"research X, spend up to $5, done by tonight"*. Something has
//! to turn that prose into quarry's `Caps` before a run can even be planned,
//! because **planning is budget-conditioned (P9)**: quarry's `Caps.Validate()`
//! refuses an uncapped run with `at least one cap is required (P9)`, and
//! `cmd/quarry/run.go` surfaces it as *"planning is budget-conditioned (P9): set
//! --cap or --deadline"*. That is a design refusal, not a missing default — so
//! this module must not quietly pick a cap on the sender's behalf either. When
//! the text carries no cap, the answer is a question, not a guess.
//!
//! # This is a pure function, deliberately
//!
//! `parse_caps(text, now, tz)` does no I/O, holds no gateway state, and makes no
//! model call. Everything it needs is an argument, including `now` — which is why
//! "by tonight" is testable at all. An LLM-based parser was considered and
//! rejected on two grounds: a model call to read "$5" spends money to determine a
//! money budget, and it makes the component untestable offline. Deterministic
//! parsing, and ask when unsure.
//!
//! # Only three denominations exist
//!
//! quarry's `Caps` is exactly `{Spend Units, Latency time.Duration, Due
//! time.Time}` and `Denomination` is `spend | latency | due`. Nothing else is a
//! cap, which rules out the most natural-sounding request in the room:
//!
//! **"at most 30 agents" is not a cap.** The nearest thing in quarry is
//! `Executor.MaxDepth`, and quarry is emphatic that depth is "a BACKSTOP, not the
//! design (P2)" — a run bounded by it is *under-verified rather than complete*.
//! Fanout is the planner's decision under the budget, not a dial a sender turns.
//! Parsing an agent count into a cap would invent a denomination quarry cannot
//! report as `BoundBy`, so the run would come back bounded by something the
//! sender never asked about. It is recognised here and asked about, never
//! silently dropped and never faked.
//!
//! # `Due` has nowhere to go yet, and that costs real money
//!
//! `cmd/quarry/run.go` sets `Caps{Spend: spend, Latency: *deadline}`. There is
//! **no flag that populates `Due`** — verified against the flag set in
//! `runCmd`: `--cap`, `--floor`, `--deadline`, `--depth`, `--fake`, `--model`,
//! `--region`, `--out`, `--quiet`, `--fake-latency`, `--scope`, `--retries`.
//!
//! This matters more than a missing flag usually would. `Caps.Deferrable()` is
//! `!Due.IsZero() && Latency == 0`: a due date *without* a latency cap is what
//! makes batch inference and off-peak execution available. The deadline is a
//! price control, not a scheduling field. So substituting `Latency` for `Due`
//! does not merely lose precision — it forfeits the cheap path, because a run
//! with a `Latency` set is by definition not deferrable.
//!
//! This module therefore parses `Due` into its own field and reports the
//! substitution as a [`Disclosure`] rather than performing it silently. The
//! caller decides whether to substitute or refuse; either way the sender sees it
//! before spend. When upstream `--due` lands, the disclosure goes away and
//! nothing else here changes.
//!
//! # What this module does not do
//!
//! It reports what the sender *said*. Clamping that against operator policy is a
//! separate concern (`quarry: caps and Scope minted from operator policy`), which
//! is why `"spend up to $999999"` parses cleanly here: an absurd request is a
//! policy refusal, and pre-empting it here would put the limit in two places and
//! let them disagree.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use std::time::Duration;

// ── Units ─────────────────────────────────────────────────────────────────────

/// quarry's `Unlimited` sentinel: a cap that is not set.
///
/// `-1`, **not zero**. quarry's own comment on `Limited()` explains why callers
/// must ask before comparing: *"Unlimited is -1, so `spend < cap` silently treats
/// an uncapped run as a zero budget."* A zero cap and an absent cap are opposite
/// instructions — one funds nothing, one funds everything — and the type does not
/// distinguish them for you.
pub const UNLIMITED_MICRO_USD: i64 = -1;

/// Whether a micro-unit quantity is a real cap rather than [`UNLIMITED_MICRO_USD`].
///
/// The mirror of quarry's `Units.Limited()`. Consult this before *any* comparison
/// against a spend quantity.
pub fn is_limited(micro_usd: i64) -> bool {
    micro_usd >= 0
}

/// Convert a human-facing USD quantity into int64 micro-dollars.
///
/// One definition, shared with the receipt reader in [`super::event`] — a sender's
/// cap and the cost reconciled against it have to be rounded identically or the
/// comparison is against a number that was never charged.
///
/// `round`, **never** `int()`. This is not hypothetical for the amounts a sender
/// actually types: `$2.01 × 1e6` is `2009999.9999999998` in binary floating point,
/// so truncation yields `2_009_999` and rounding `2_010_000`. Note quarry's own
/// `FromFloat` is `Units(f * 1e6)` — a Go truncating conversion — so **quarry's
/// helper has the bug its own integration doc warns about**. We hand quarry
/// decimal text (`--cap 2.010000`) and it re-parses via `Sscanf("%g")` and
/// `FromFloat`, so the last micro-unit can still be lost on its side; that is an
/// upstream matter. What this guarantees is that the number *we* record, audit and
/// reconcile against is the correctly rounded one, so the gateway's ledger does
/// not compound the error.
pub use super::event::usd_to_micro;

// ── Caps ──────────────────────────────────────────────────────────────────────

/// The caps a sender asked for — the mirror of quarry's `Caps`.
///
/// Any subset may be present. All three absent is not a runnable state, which
/// [`RequestedCaps::validate`] enforces the way quarry does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestedCaps {
    /// Spend cap in int64 micro-dollars. `None` is *unset*; `Some(-1)` is an
    /// explicit request for `Unlimited`, which is a different thing — the first
    /// says nothing, the second says "no limit", and policy treats them
    /// differently.
    pub spend_micro_usd: Option<i64>,
    /// Latency cap as a duration.
    pub latency: Option<Duration>,
    /// Deadline as an instant, in UTC. Carried separately from `latency` because
    /// `Deferrable()` is precisely the case where one is set and the other is not.
    pub due: Option<DateTime<Utc>>,
}

impl RequestedCaps {
    /// Whether at least one real cap is present — quarry's P9 precondition.
    pub fn any(&self) -> bool {
        self.spend_micro_usd.is_some() || self.latency.is_some() || self.due.is_some()
    }

    /// Whether slack is convertible into money — quarry's `Caps.Deferrable()`.
    ///
    /// A due date with no latency cap means the run is not needed soon, so batch
    /// and off-peak inference are available. Giving up fast mechanically buys
    /// cheap.
    pub fn deferrable(&self) -> bool {
        self.due.is_some() && self.latency.is_none()
    }

    /// Validate the way quarry's `Caps.Validate()` does, with its messages.
    ///
    /// Kept in lockstep on purpose: a caps set this accepts must be one quarry
    /// accepts, or the gateway refuses a run at the wrong layer and reports the
    /// wrong reason.
    pub fn validate(&self) -> Result<(), CapsRefusal> {
        if let Some(spend) = self.spend_micro_usd {
            if is_limited(spend) && spend <= 0 {
                return Err(CapsRefusal::SpendNotPositive);
            }
        }
        if !self.any() {
            return Err(CapsRefusal::Uncapped);
        }
        Ok(())
    }
}

/// A reason a caps set cannot be run, phrased so a sender can act on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsRefusal {
    /// No cap in any denomination. quarry refuses this by design (P9).
    Uncapped,
    /// A spend cap of zero or less. quarry's `Validate()` rejects it, and it is
    /// also what "don't spend anything" means — a request to run for free, which
    /// is not a budget.
    SpendNotPositive,
}

impl CapsRefusal {
    /// A stable machine-readable code, for audit records and tests.
    pub fn code(&self) -> &'static str {
        match self {
            CapsRefusal::Uncapped => "uncapped",
            CapsRefusal::SpendNotPositive => "spend_not_positive",
        }
    }

    /// The sender-facing explanation, quoting quarry's own reason.
    pub fn message(&self) -> String {
        match self {
            CapsRefusal::Uncapped => "planning is budget-conditioned (P9): a run needs at least \
                 one cap. Tell me a spend limit (\"up to $5\"), a time limit \
                 (\"within 20 minutes\"), or a deadline (\"by 5pm\")."
                .to_string(),
            CapsRefusal::SpendNotPositive => {
                "a spend cap has to be positive — quarry plans against the budget, so a \
                 cap of zero funds nothing and there is no run to plan."
                    .to_string()
            }
        }
    }
}

// ── Disclosures and questions ─────────────────────────────────────────────────

/// Something the sender must be told before spend, even though parsing succeeded.
///
/// These exist because P9's disclosure requirement is about *quiet* degradation.
/// A substitution the sender agreed to is fine; the same substitution unannounced
/// is what the principle prohibits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disclosure {
    /// A `Due` deadline was parsed, but quarry's CLI has no `--due` flag, so it
    /// can only be honoured as an equivalent `Latency` — which forfeits
    /// `Deferrable()` and with it batch/off-peak pricing. The caller chooses
    /// substitution or refusal; the sender is told either way.
    DueHasNoUpstreamFlag {
        /// The resolved deadline, echoed so a wrong timezone guess is visible
        /// *before* spend rather than discovered after it.
        resolved: DateTime<Utc>,
        /// The equivalent latency, if substitution is taken.
        equivalent_latency: Duration,
    },
    /// The deadline was resolved in this timezone, and where that zone came from.
    /// Echoed for the same reason: a deadline resolved in the wrong zone is a
    /// silently wrong budget.
    DeadlineResolvedIn {
        /// IANA zone name.
        timezone: String,
        /// How the zone was determined — see [`TimezoneSource`].
        source: TimezoneSource,
        /// The instant, as local wall-clock text for a human to check.
        local: String,
    },
}

/// Where a sender's timezone came from, in precedence order.
///
/// Surfaced rather than hidden: a deadline resolved from `Utc` because nothing
/// was configured is a materially different claim from one resolved in the
/// sender's own zone, and only the sender can tell whether it is right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimezoneSource {
    /// The sender's stored preference.
    SenderPreference,
    /// The operator's configured default.
    ConfigDefault,
    /// Neither was set. UTC is the last resort, and saying so is the point.
    UtcFallback,
}

impl TimezoneSource {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            TimezoneSource::SenderPreference => "sender_preference",
            TimezoneSource::ConfigDefault => "config_default",
            TimezoneSource::UtcFallback => "utc_fallback",
        }
    }
}

/// A resolved timezone together with where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderTimezone {
    /// The zone deadlines resolve in.
    pub tz: Tz,
    /// Which step of the fallback chain supplied it.
    pub source: TimezoneSource,
}

impl SenderTimezone {
    /// Resolve the fallback chain: sender preference, then operator default,
    /// then UTC.
    ///
    /// An unparseable zone name at either level falls through to the next rather
    /// than failing the run — but the returned `source` then reports what was
    /// *actually* used, so the disclosure does not claim a zone that was
    /// rejected.
    pub fn resolve(sender_pref: Option<&str>, config_default: Option<&str>) -> Self {
        if let Some(tz) = sender_pref.and_then(|s| s.parse::<Tz>().ok()) {
            return Self {
                tz,
                source: TimezoneSource::SenderPreference,
            };
        }
        if let Some(tz) = config_default.and_then(|s| s.parse::<Tz>().ok()) {
            return Self {
                tz,
                source: TimezoneSource::ConfigDefault,
            };
        }
        Self {
            tz: chrono_tz::UTC,
            source: TimezoneSource::UtcFallback,
        }
    }

    /// UTC with the fallback provenance — the zero-configuration case.
    pub fn utc_fallback() -> Self {
        Self {
            tz: chrono_tz::UTC,
            source: TimezoneSource::UtcFallback,
        }
    }
}

/// A question the sender has to answer before a run can start.
///
/// Every variant carries the fragment that triggered it, because "I didn't
/// understand" is not actionable and "I didn't understand *by 5*" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    /// No cap was found in any denomination.
    NoCapFound,
    /// A cap-shaped phrase was found but is ambiguous between readings.
    Ambiguous {
        /// The literal text that could not be resolved.
        fragment: String,
        /// The readings it could take, so the sender picks rather than retypes.
        readings: Vec<String>,
    },
    /// The sender asked to limit something that is not a quarry denomination —
    /// agent count, node count, depth. Recognised rather than dropped: a request
    /// this module silently ignored would come back bounded by a cap the sender
    /// never set.
    NotADenomination {
        /// The literal text.
        fragment: String,
        /// Why it cannot be a cap.
        reason: String,
    },
    /// A spend amount was recognised but is not usable as a cap (zero, negative).
    UnusableSpend {
        /// The literal text.
        fragment: String,
        /// Why it cannot fund a run.
        reason: String,
    },
}

impl Question {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Question::NoCapFound => "no_cap_found",
            Question::Ambiguous { .. } => "ambiguous",
            Question::NotADenomination { .. } => "not_a_denomination",
            Question::UnusableSpend { .. } => "unusable_spend",
        }
    }

    /// The sender-facing question.
    pub fn message(&self) -> String {
        match self {
            Question::NoCapFound => CapsRefusal::Uncapped.message(),
            Question::Ambiguous { fragment, readings } => format!(
                "\"{}\" could mean {} — which did you mean?",
                fragment,
                readings.join(" or ")
            ),
            Question::NotADenomination { fragment, reason } => {
                format!("\"{fragment}\" is not something I can cap: {reason}")
            }
            Question::UnusableSpend { fragment, reason } => {
                format!("\"{fragment}\" cannot fund a run: {reason}")
            }
        }
    }
}

/// The outcome of parsing a sender's message for caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsParse {
    /// At least one real cap was found. `disclosures` may be non-empty and must
    /// reach the sender before spend.
    Caps {
        /// What the sender asked for.
        caps: RequestedCaps,
        /// What they need to be told about it.
        disclosures: Vec<Disclosure>,
    },
    /// Nothing runnable was found. Ask; never default.
    Ask {
        /// Everything that needs answering, so a sender fixes it in one reply
        /// rather than being asked serially.
        questions: Vec<Question>,
    },
}

impl CapsParse {
    /// The caps, if any were found.
    pub fn caps(&self) -> Option<&RequestedCaps> {
        match self {
            CapsParse::Caps { caps, .. } => Some(caps),
            CapsParse::Ask { .. } => None,
        }
    }

    /// The questions, if the parse could not produce a runnable cap.
    pub fn questions(&self) -> &[Question] {
        match self {
            CapsParse::Ask { questions } => questions,
            CapsParse::Caps { .. } => &[],
        }
    }

    /// Disclosures that must be shown before spend.
    pub fn disclosures(&self) -> &[Disclosure] {
        match self {
            CapsParse::Caps { disclosures, .. } => disclosures,
            CapsParse::Ask { .. } => &[],
        }
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse caps out of a sender's message.
///
/// Pure: `now` and `tz` are arguments precisely so relative deadlines are
/// testable. Returns [`CapsParse::Ask`] whenever it cannot produce a cap it is
/// confident in — including when it found nothing at all, which is the common
/// case and must never become a default.
pub fn parse_caps(text: &str, now: DateTime<Utc>, tz: SenderTimezone) -> CapsParse {
    let lower = text.to_lowercase();
    let mut caps = RequestedCaps::default();
    let mut disclosures = Vec::new();
    let mut questions = Vec::new();

    // Order matters only in that an ambiguity found anywhere blocks the run: a
    // parse that took the spend cap and shrugged at an ambiguous deadline would
    // start a run under half the constraints the sender wrote.
    match parse_spend(&lower) {
        SpendFind::None => {}
        SpendFind::Micro(micro) => caps.spend_micro_usd = Some(micro),
        SpendFind::Unlimited => caps.spend_micro_usd = Some(UNLIMITED_MICRO_USD),
        SpendFind::Question(q) => questions.push(q),
    }

    match parse_latency(&lower) {
        Some(Ok(d)) => caps.latency = Some(d),
        Some(Err(q)) => questions.push(q),
        None => {}
    }

    match parse_due(&lower, now, tz.tz) {
        Some(Ok(instant)) => {
            caps.due = Some(instant);
            disclosures.push(Disclosure::DeadlineResolvedIn {
                timezone: tz.tz.name().to_string(),
                source: tz.source,
                local: instant
                    .with_timezone(&tz.tz)
                    .format("%Y-%m-%d %H:%M %Z")
                    .to_string(),
            });
        }
        Some(Err(q)) => questions.push(q),
        None => {}
    }

    // Not a cap in quarry's model, and worth saying so out loud.
    if let Some(q) = detect_non_denomination(&lower) {
        questions.push(q);
    }

    // The Due substitution disclosure comes last so it reads after the resolved
    // instant it refers to.
    if let Some(due) = caps.due {
        let equivalent_latency = (due - now)
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(0));
        disclosures.push(Disclosure::DueHasNoUpstreamFlag {
            resolved: due,
            equivalent_latency,
        });
    }

    if !questions.is_empty() {
        return CapsParse::Ask { questions };
    }

    match caps.validate() {
        Ok(()) => CapsParse::Caps { caps, disclosures },
        Err(CapsRefusal::Uncapped) => CapsParse::Ask {
            questions: vec![Question::NoCapFound],
        },
        Err(refusal) => CapsParse::Ask {
            questions: vec![Question::UnusableSpend {
                fragment: text.trim().to_string(),
                reason: refusal.message(),
            }],
        },
    }
}

// ── Spend ─────────────────────────────────────────────────────────────────────

enum SpendFind {
    None,
    Micro(i64),
    Unlimited,
    Question(Question),
}

/// Phrases that ask for no spend at all. "Don't spend anything" is not a $0 cap —
/// a $0 cap is a run that cannot fund its first call, which quarry rejects. It is
/// a request the sender has to restate.
const NO_SPEND_PHRASES: &[&str] = &[
    "don't spend anything",
    "dont spend anything",
    "do not spend anything",
    "without spending anything",
    "spend nothing",
    "for free",
    "free of charge",
    "no money",
];

/// Phrases requesting an explicitly unlimited spend cap. Distinct from *absent*:
/// this is the sender saying "no limit", which policy may well refuse — and
/// should by default — but which is a statement, not a silence.
const UNLIMITED_PHRASES: &[&str] = &[
    "unlimited",
    "no spend limit",
    "no budget limit",
    "whatever it costs",
    "whatever it takes",
    "money is no object",
    "spare no expense",
];

fn parse_spend(lower: &str) -> SpendFind {
    for phrase in NO_SPEND_PHRASES {
        if lower.contains(phrase) {
            return SpendFind::Question(Question::UnusableSpend {
                fragment: (*phrase).to_string(),
                reason: "quarry plans against a budget, so a run with no money to spend has \
                         nothing to plan. If you want the cheapest useful answer, give me a \
                         small cap like \"up to $0.25\" instead."
                    .to_string(),
            });
        }
    }
    for phrase in UNLIMITED_PHRASES {
        if lower.contains(phrase) {
            return SpendFind::Unlimited;
        }
    }

    let amounts = find_spend_amounts(lower);
    match amounts.len() {
        0 => SpendFind::None,
        1 => {
            let (fragment, value) = amounts.into_iter().next().unwrap();
            if value <= 0.0 {
                // Covers both "$0" and "-$5". A negative cap is not a cap and a
                // zero cap funds nothing; neither is a typo we should silently
                // correct into the other.
                return SpendFind::Question(Question::UnusableSpend {
                    fragment,
                    reason: "a spend cap has to be a positive amount — quarry divides the cap \
                             across the tree, and there is nothing to divide."
                        .to_string(),
                });
            }
            SpendFind::Micro(usd_to_micro(value))
        }
        _ => {
            // "$5 or maybe $50". Picking either would be picking the sender's
            // budget for them; picking the smaller looks conservative but is
            // still a guess, and the larger is a guess that costs money.
            let readings: Vec<String> = amounts.iter().map(|(f, _)| f.clone()).collect();
            SpendFind::Question(Question::Ambiguous {
                fragment: readings.join(" / "),
                readings,
            })
        }
    }
}

/// Find every spend-shaped amount in the text, as (fragment, usd).
///
/// Recognises `$5`, `$5.50`, `-$5`, `5 dollars`, `5.50 usd`, `usd 5`. Written as
/// a scanner rather than a regex because the crate has no regex dependency of its
/// own and one denomination's parser is not worth adding one for.
fn find_spend_amounts(lower: &str) -> Vec<(String, f64)> {
    let bytes = lower.as_bytes();
    let mut found: Vec<(String, f64)> = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        // `$` form, with an optional leading minus so "-$5" is *seen* and
        // refused rather than parsed as a bare "$5".
        if bytes[i] == b'$' {
            let negative = i > 0 && preceding_minus(bytes, i);
            let start = if negative { i - 1 } else { i };
            let after = skip_spaces(bytes, i + 1);
            if let Some((value, end)) = read_number(bytes, after) {
                let fragment = lower[start..end].trim().to_string();
                found.push((fragment, if negative { -value } else { value }));
                i = end;
                continue;
            }
            i += 1;
            continue;
        }

        // `usd 5` prefix form.
        if starts_word(lower, bytes, i, "usd") {
            let after = skip_spaces(bytes, i + 3);
            if let Some((value, end)) = read_number(bytes, after) {
                found.push((lower[i..end].trim().to_string(), value));
                i = end;
                continue;
            }
        }

        // Number followed by a currency word: `5 dollars`, `5.50 usd`.
        if is_number_start(bytes, i) && !mid_token(bytes, i) {
            if let Some((value, end)) = read_number(bytes, i) {
                let after = skip_spaces(bytes, end);
                for unit in ["dollars", "dollar", "usd", "bucks"] {
                    if starts_word(lower, bytes, after, unit) {
                        let unit_end = after + unit.len();
                        let negative = i > 0 && preceding_minus(bytes, i);
                        let start = if negative { i - 1 } else { i };
                        found.push((
                            lower[start..unit_end].trim().to_string(),
                            if negative { -value } else { value },
                        ));
                        i = unit_end;
                        break;
                    }
                }
                if found.last().map(|(_, _)| ()).is_some() && i >= end {
                    continue;
                }
                i = end;
                continue;
            }
        }

        i += 1;
    }

    found
}

fn preceding_minus(bytes: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && bytes[j - 1] == b' ' {
        j -= 1;
    }
    j > 0 && bytes[j - 1] == b'-'
}

fn skip_spaces(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    i
}

fn is_number_start(bytes: &[u8], i: usize) -> bool {
    i < bytes.len() && bytes[i].is_ascii_digit()
}

/// Whether byte `i` sits inside a larger token, so `claude-3-5` does not read as
/// the number 3.
fn mid_token(bytes: &[u8], i: usize) -> bool {
    i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'-' || bytes[i - 1] == b'.')
}

/// Read a decimal number at `i`, returning (value, end).
///
/// Rejects thousands separators rather than guessing: `1,500` is `1500` in the US
/// and `1.5` in much of Europe, and localised formats are out of scope, so a
/// comma ends the number.
fn read_number(bytes: &[u8], start: usize) -> Option<(f64, usize)> {
    let mut i = start;
    let mut seen_digit = false;
    let mut seen_dot = false;
    while i < bytes.len() {
        match bytes[i] {
            b'0'..=b'9' => {
                seen_digit = true;
                i += 1;
            }
            b'.' if !seen_dot => {
                // Only a decimal point if a digit follows, so "$5." ends at 5.
                if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
                    seen_dot = true;
                    i += 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    if !seen_digit {
        return None;
    }
    std::str::from_utf8(&bytes[start..i])
        .ok()?
        .parse::<f64>()
        .ok()
        .map(|v| (v, i))
}

/// Whether `word` starts at byte `i` on a token boundary.
///
/// A *digit* immediately before the word is a boundary, but a letter is not.
/// Senders write units joined to their number — `90s`, `20m`, `5pm`, `5usd` — and
/// requiring a space there silently dropped every one of them, which is worse than
/// a parse error: the run started under a cap the sender did not write. A letter
/// before still blocks the match, so `bugs` does not read as a `s` unit.
fn starts_word(s: &str, bytes: &[u8], i: usize, word: &str) -> bool {
    if i + word.len() > bytes.len() {
        return false;
    }
    if &s[i..i + word.len()] != word {
        return false;
    }
    if i > 0 && bytes[i - 1].is_ascii_alphabetic() {
        return false;
    }
    let after = i + word.len();
    if after < bytes.len() && bytes[after].is_ascii_alphanumeric() {
        return false;
    }
    true
}

// ── Latency ───────────────────────────────────────────────────────────────────

/// Time-unit words and their duration in seconds.
const TIME_UNITS: &[(&str, u64)] = &[
    ("milliseconds", 0),
    ("millisecond", 0),
    ("seconds", 1),
    ("second", 1),
    ("secs", 1),
    ("sec", 1),
    ("s", 1),
    ("minutes", 60),
    ("minute", 60),
    ("mins", 60),
    ("min", 60),
    ("m", 60),
    ("hours", 3600),
    ("hour", 3600),
    ("hrs", 3600),
    ("hr", 3600),
    ("h", 3600),
    ("days", 86400),
    ("day", 86400),
    ("d", 86400),
];

/// Words that introduce a *duration* rather than a deadline. "within 20 minutes"
/// is a `Latency`; "by 5pm" is a `Due`, and the two are different denominations
/// with different prices, so the trigger word decides which.
const LATENCY_TRIGGERS: &[&str] = &[
    "within",
    "in under",
    "under",
    "in less than",
    "in",
    "inside",
];

fn parse_latency(lower: &str) -> Option<Result<Duration, Question>> {
    for trigger in LATENCY_TRIGGERS {
        let mut search = 0;
        while let Some(rel) = lower[search..].find(trigger) {
            let at = search + rel;
            let bytes = lower.as_bytes();
            let boundary_before = at == 0 || !bytes[at - 1].is_ascii_alphanumeric();
            let after_trigger = at + trigger.len();
            let boundary_after =
                after_trigger >= bytes.len() || !bytes[after_trigger].is_ascii_alphanumeric();
            if boundary_before && boundary_after {
                if let Some(d) = read_duration(lower, after_trigger) {
                    return Some(Ok(d));
                }
            }
            search = at + trigger.len();
        }
    }
    None
}

/// Read `<number> <unit>` starting at `from`, tolerating a joined form (`90s`).
fn read_duration(lower: &str, from: usize) -> Option<Duration> {
    let bytes = lower.as_bytes();
    let mut i = skip_spaces(bytes, from);
    // Skip filler like "the next".
    for filler in ["the next ", "next ", "the "] {
        if lower[i..].starts_with(filler) {
            i += filler.len();
            i = skip_spaces(bytes, i);
        }
    }
    let (value, end) = read_number(bytes, i)?;
    let unit_start = skip_spaces(bytes, end);
    // Longest match first, so "min" does not win over "minutes".
    let mut best: Option<(&str, u64)> = None;
    for (word, secs) in TIME_UNITS {
        if starts_word(lower, bytes, unit_start, word)
            && best.map(|(b, _)| word.len() > b.len()).unwrap_or(true)
        {
            best = Some((word, *secs));
        }
    }
    let (word, secs) = best?;
    if word.starts_with("millisecond") {
        return Some(Duration::from_millis(
            (value * 1000.0).round() as u64 / 1000,
        ));
    }
    let total_secs = value * secs as f64;
    if total_secs <= 0.0 {
        return None;
    }
    Some(Duration::from_millis((total_secs * 1000.0).round() as u64))
}

// ── Due ───────────────────────────────────────────────────────────────────────

/// Named parts of the day, as the local hour they resolve to.
///
/// "Tonight" is 23:59 local, not 20:00: a sender saying "by tonight" is naming
/// the end of their day, and picking an earlier hour would buy less compute than
/// they asked for.
const DAY_PARTS: &[(&str, u32, u32)] = &[
    ("tonight", 23, 59),
    ("this evening", 23, 59),
    ("end of day", 23, 59),
    ("eod", 23, 59),
    ("midnight", 23, 59),
    ("morning", 9, 0),
    ("noon", 12, 0),
    ("midday", 12, 0),
    ("afternoon", 17, 0),
    ("evening", 23, 59),
];

const WEEKDAYS: &[(&str, u32)] = &[
    ("monday", 0),
    ("tuesday", 1),
    ("wednesday", 2),
    ("thursday", 3),
    ("friday", 4),
    ("saturday", 5),
    ("sunday", 6),
];

fn parse_due(lower: &str, now: DateTime<Utc>, tz: Tz) -> Option<Result<DateTime<Utc>, Question>> {
    let at = find_by_trigger(lower)?;
    let rest = lower[at..].trim_start();
    let local_now = now.with_timezone(&tz);

    // "by tomorrow morning" / "by tomorrow"
    if let Some(after) = strip_prefix_word(rest, "tomorrow") {
        let date = local_now.date_naive() + ChronoDuration::days(1);
        let (h, m) = day_part_in(after).unwrap_or((23, 59));
        return Some(resolve_local(tz, date, h, m));
    }
    if let Some(after) = strip_prefix_word(rest, "today") {
        let (h, m) = day_part_in(after).unwrap_or((23, 59));
        return Some(resolve_local(tz, local_now.date_naive(), h, m));
    }

    // "by friday" / "by friday morning"
    for (name, target) in WEEKDAYS {
        if let Some(after) = strip_prefix_word(rest, name) {
            let current = local_now.weekday().num_days_from_monday();
            // Strictly forward: "by Friday" said on a Friday means *next* Friday,
            // because the sender is naming a future boundary, not one that may
            // already have passed today. Note `% 7` is 0 for today, and the
            // correction is 7 rather than 1 — clamping to 1 would silently move a
            // week-long deadline to tomorrow.
            let ahead = match (*target + 7 - current) % 7 {
                0 => 7,
                n => n,
            } as i64;
            let date = local_now.date_naive() + ChronoDuration::days(ahead);
            let (h, m) = day_part_in(after).unwrap_or((23, 59));
            return Some(resolve_local(tz, date, h, m));
        }
    }

    // "by tonight" / "by noon" / "by end of day"
    for (name, h, m) in DAY_PARTS {
        if let Some(_after) = strip_prefix_word(rest, name) {
            let mut date = local_now.date_naive();
            // A part of today that has already passed means tomorrow's.
            if (local_now.hour(), local_now.minute()) >= (*h, *m) {
                date += ChronoDuration::days(1);
            }
            return Some(resolve_local(tz, date, *h, *m));
        }
    }

    // "by 5pm" / "by 17:00" / "by 5" — the last of which is the ambiguity.
    if let Some(result) = parse_clock_time(rest, local_now, tz) {
        return Some(result);
    }

    None
}

/// Find the byte offset just past a deadline trigger word.
fn find_by_trigger(lower: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    for trigger in ["by ", "before ", "due ", "deadline ", "done by "] {
        let mut search = 0;
        while let Some(rel) = lower[search..].find(trigger) {
            let at = search + rel;
            if at == 0 || !bytes[at - 1].is_ascii_alphanumeric() {
                return Some(at + trigger.len());
            }
            search = at + trigger.len();
        }
    }
    None
}

/// Strip `word` from the front of `s` if present on a token boundary.
fn strip_prefix_word<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(word)?;
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(rest.trim_start())
}

/// Find a named part of the day anywhere in `s`.
fn day_part_in(s: &str) -> Option<(u32, u32)> {
    let mut best: Option<(usize, u32, u32)> = None;
    for (name, h, m) in DAY_PARTS {
        if s.contains(name) && best.map(|(l, _, _)| name.len() > l).unwrap_or(true) {
            best = Some((name.len(), *h, *m));
        }
    }
    best.map(|(_, h, m)| (h, m))
}

/// Parse a clock time, or return the ambiguity question for a bare number.
fn parse_clock_time(
    rest: &str,
    local_now: DateTime<Tz>,
    tz: Tz,
) -> Option<Result<DateTime<Utc>, Question>> {
    let bytes = rest.as_bytes();
    let (hour_f, end) = read_number(bytes, 0)?;
    if hour_f.fract() != 0.0 {
        return None;
    }
    let mut hour = hour_f as u32;
    let mut minute = 0u32;
    let mut i = end;

    // ":30"
    if i < bytes.len() && bytes[i] == b':' {
        if let Some((m, mend)) = read_number(bytes, i + 1) {
            minute = m as u32;
            i = mend;
        }
    }
    let after = skip_spaces(bytes, i);
    let meridiem_pm =
        starts_word(rest, bytes, after, "pm") || starts_word(rest, bytes, after, "p.m");
    let meridiem_am =
        starts_word(rest, bytes, after, "am") || starts_word(rest, bytes, after, "a.m");

    if !meridiem_pm && !meridiem_am && hour < 13 && i == end {
        // "by 5". A bare hour under 13 is genuinely two times, twelve hours and a
        // lot of money apart. Resolving it — even "sensibly" — is the guess this
        // module exists to avoid.
        return Some(Err(Question::Ambiguous {
            fragment: format!("by {hour}"),
            readings: vec![format!("{hour}am"), format!("{hour}pm")],
        }));
    }
    if meridiem_pm && hour < 12 {
        hour += 12;
    }
    if meridiem_am && hour == 12 {
        hour = 0;
    }
    if hour > 23 || minute > 59 {
        return None;
    }

    let mut date = local_now.date_naive();
    if (hour, minute) <= (local_now.hour(), local_now.minute()) {
        date += ChronoDuration::days(1);
    }
    Some(resolve_local(tz, date, hour, minute))
}

/// Turn a local wall-clock date and time into a UTC instant.
///
/// DST makes this a real conversion rather than an offset add: a local time can
/// be skipped (spring forward) or doubled (autumn back). A skipped time takes the
/// next valid instant — moving the deadline *later*, never earlier, because a
/// deadline pulled earlier silently shrinks the budget the sender asked for. An
/// ambiguous time takes the earlier instant, which is the conservative direction
/// for the same reason inverted: it never grants more time than was asked.
fn resolve_local(
    tz: Tz,
    date: NaiveDate,
    hour: u32,
    minute: u32,
) -> Result<DateTime<Utc>, Question> {
    let naive = date
        .and_hms_opt(hour, minute, 0)
        .ok_or(Question::Ambiguous {
            fragment: format!("{hour}:{minute:02}"),
            readings: vec!["a valid time of day".to_string()],
        })?;
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
        chrono::LocalResult::Ambiguous(earlier, _later) => Ok(earlier.with_timezone(&Utc)),
        chrono::LocalResult::None => {
            // Spring-forward gap: walk forward to the first instant that exists.
            for add in 1..=180i64 {
                let candidate = naive + ChronoDuration::minutes(add);
                if let chrono::LocalResult::Single(dt) = tz.from_local_datetime(&candidate) {
                    return Ok(dt.with_timezone(&Utc));
                }
            }
            Err(Question::Ambiguous {
                fragment: naive.format("%Y-%m-%d %H:%M").to_string(),
                readings: vec!["a time that exists in your timezone (this one falls in a \
                     daylight-saving gap)"
                    .to_string()],
            })
        }
    }
}

// ── Non-denominations ─────────────────────────────────────────────────────────

/// Nouns a sender might try to cap that quarry has no denomination for.
const NON_DENOMINATION_NOUNS: &[&str] = &[
    "agents",
    "agent",
    "workers",
    "worker",
    "subagents",
    "subagent",
    "nodes",
    "node",
    "tasks",
    "task",
    "steps",
    "step",
    "levels",
    "level",
    "calls",
    "call",
    "tokens",
    "token",
];

/// Trigger phrases for a limit request.
const LIMIT_TRIGGERS: &[&str] = &[
    "at most",
    "no more than",
    "max",
    "maximum",
    "up to",
    "limit of",
    "no deeper than",
];

fn detect_non_denomination(lower: &str) -> Option<Question> {
    for trigger in LIMIT_TRIGGERS {
        let mut search = 0;
        while let Some(rel) = lower[search..].find(trigger) {
            let at = search + rel;
            let tail = &lower[at + trigger.len()..];
            // Only look as far as the end of the phrase, so an unrelated noun
            // later in a long message is not attributed to this limit.
            let window_end = tail.len().min(40);
            let window = &tail[..window_end];
            for noun in NON_DENOMINATION_NOUNS {
                if window.contains(noun) {
                    let fragment = format!("{}{}", trigger, window.trim_end())
                        .trim()
                        .to_string();
                    return Some(Question::NotADenomination {
                        fragment,
                        reason: non_denomination_reason(noun),
                    });
                }
            }
            search = at + trigger.len();
        }
    }
    // "depth 5" / "3 levels deep" without a limit trigger.
    if lower.contains("depth") || lower.contains("levels deep") {
        return Some(Question::NotADenomination {
            fragment: "depth".to_string(),
            reason: non_denomination_reason("levels"),
        });
    }
    None
}

fn non_denomination_reason(noun: &str) -> String {
    match noun {
        "tokens" | "token" => "quarry budgets in money, time, or a deadline — not tokens. A \
             spend cap covers token cost across every model it uses, which is \
             the number you probably care about. Try \"up to $2\"."
            .to_string(),
        "levels" | "level" => "recursion depth is a backstop, not a budget — quarry stops \
             recursing when it runs out of verifiers (P2), and a run bounded by \
             depth is under-verified rather than complete. Cap the money or the \
             time instead."
            .to_string(),
        _ => format!(
            "quarry has no {noun} budget. How wide to go is the planner's decision under the \
             budget you set, so capping it directly would bound the run by something quarry \
             cannot report back. Give me money (\"up to $5\"), time (\"within 20 minutes\"), or \
             a deadline (\"by 5pm\") and it will spend that on as many {noun} as it can \
             verify."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant so every deadline assertion is exact. 2026-08-04 is a
    /// Tuesday; 14:30 UTC.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 4, 14, 30, 0).unwrap()
    }

    fn ny() -> SenderTimezone {
        SenderTimezone {
            tz: chrono_tz::America::New_York,
            source: TimezoneSource::SenderPreference,
        }
    }

    fn parse(text: &str) -> CapsParse {
        parse_caps(text, now(), ny())
    }

    /// Parse and take the caps, panicking with the parse for a readable failure.
    fn caps_of(text: &str) -> RequestedCaps {
        let parsed = parse(text);
        parsed
            .caps()
            .unwrap_or_else(|| panic!("{text:?} should parse, got {parsed:?}"))
            .clone()
    }

    // ── Units ────────────────────────────────────────────────────────────────

    #[test]
    fn dollars_convert_to_micro_units_by_rounding_not_truncation() {
        // $2.01 × 1e6 is 2009999.9999999998 in binary floating point. Truncation
        // yields 2_009_999 — the exact micro-unit loss quarry's integration doc
        // says would desync the debit.
        assert_eq!(usd_to_micro(2.01), 2_010_000);
        assert_eq!((2.01_f64 * 1_000_000.0) as i64, 2_009_999);
        assert_eq!(usd_to_micro(5.0), 5_000_000);
        assert_eq!(usd_to_micro(5.50), 5_500_000);
        assert_eq!(usd_to_micro(0.0002), 200);
    }

    #[test]
    fn unlimited_is_negative_one_and_not_a_zero_budget() {
        assert_eq!(UNLIMITED_MICRO_USD, -1);
        assert!(!is_limited(UNLIMITED_MICRO_USD));
        assert!(is_limited(0));
        assert!(is_limited(5_000_000));
    }

    #[test]
    fn the_unlimited_comparison_trap_is_guarded() {
        // quarry's warning, made concrete: a naive `spend < cap` treats an
        // uncapped run as a zero budget, so an unlimited run would look
        // exhausted before it started.
        let cap = UNLIMITED_MICRO_USD;
        let spent = 1_000_000;
        assert!(
            !(spent < cap),
            "the naive comparison is what must not be used"
        );
        // The guarded form: ask Limited() first, and an unlimited cap never
        // exhausts.
        let exhausted = is_limited(cap) && spent >= cap;
        assert!(!exhausted);
    }

    // ── Spend ────────────────────────────────────────────────────────────────

    #[test]
    fn spend_parses_in_every_documented_form() {
        for (text, expect) in [
            ("research x, $5", 5_000_000),
            ("research x, 5 dollars", 5_000_000),
            ("research x, up to $5.50", 5_500_000),
            ("research x, usd 5", 5_000_000),
            ("research x, 5.50 usd", 5_500_000),
            ("research x, spend up to $0.25", 250_000),
            ("research x for 3 bucks", 3_000_000),
        ] {
            let parsed = parse(text);
            let caps = parsed
                .caps()
                .unwrap_or_else(|| panic!("{text:?} should parse, got {parsed:?}"));
            assert_eq!(
                caps.spend_micro_usd,
                Some(expect),
                "{text:?} produced {caps:?}"
            );
        }
    }

    #[test]
    fn a_large_request_parses_here_because_clamping_belongs_to_policy() {
        // Refusing an absurd amount here would put the limit in two places. This
        // component reports what the sender said; policy decides what they get.
        let caps = caps_of("research x, spend up to $999999");
        assert_eq!(caps.spend_micro_usd, Some(999_999_000_000));
    }

    #[test]
    fn zero_and_negative_amounts_are_asked_about_not_coerced() {
        for text in ["research x, $0", "research x, -$5", "research x, 0 dollars"] {
            let parsed = parse(text);
            let qs = parsed.questions();
            assert!(
                qs.iter().any(|q| q.code() == "unusable_spend"),
                "{text:?} should ask about the amount, got {parsed:?}"
            );
            assert!(parsed.caps().is_none(), "{text:?} must not produce a cap");
        }
    }

    #[test]
    fn a_negative_amount_is_not_silently_read_as_its_positive() {
        // The failure this guards: scanning for `$` and ignoring the sign turns
        // "-$5" into a $5 cap — a budget the sender never authorised.
        let parsed = parse("research x, -$5");
        assert!(parsed.caps().is_none());
        assert!(parsed.questions()[0].message().contains("positive"));
    }

    #[test]
    fn two_amounts_are_an_ambiguity_not_a_choice_we_make() {
        let parsed = parse("research x, $5 or maybe $50");
        let q = &parsed.questions()[0];
        assert_eq!(q.code(), "ambiguous");
        assert!(parsed.caps().is_none());
        // Both readings offered, so the sender picks instead of retyping.
        assert!(q.message().contains("$5"));
        assert!(q.message().contains("$50"));
    }

    #[test]
    fn asking_to_spend_nothing_is_a_question_not_a_zero_cap() {
        for text in [
            "research x, don't spend anything",
            "research x for free",
            "research x, spend nothing",
        ] {
            let parsed = parse(text);
            assert!(parsed.caps().is_none(), "{text:?} produced {parsed:?}");
            assert_eq!(parsed.questions()[0].code(), "unusable_spend");
        }
    }

    #[test]
    fn an_explicit_unlimited_request_is_recorded_as_unlimited_not_as_absent() {
        // Policy should refuse this by default, but it has to be able to *see*
        // it: "no limit" and silence are different requests and get different
        // answers.
        let caps = caps_of("research x, whatever it costs");
        assert_eq!(caps.spend_micro_usd, Some(UNLIMITED_MICRO_USD));
        assert!(!is_limited(caps.spend_micro_usd.unwrap()));
    }

    #[test]
    fn a_model_name_with_digits_is_not_read_as_an_amount() {
        // "claude-3-5-sonnet" must not contribute a 3 or a 5 to the budget.
        let parsed = parse("summarise this with claude-3-5-sonnet within 10 minutes");
        let caps = parsed.caps().unwrap();
        assert_eq!(caps.spend_micro_usd, None);
        assert_eq!(caps.latency, Some(Duration::from_secs(600)));
    }

    // ── Latency ──────────────────────────────────────────────────────────────

    #[test]
    fn latency_parses_in_every_documented_form() {
        for (text, secs) in [
            ("research x within 20 minutes", 1200),
            ("research x in 2 hours", 7200),
            ("research x under 90s", 90),
            ("research x within 45 min", 2700),
            ("research x in 1 day", 86400),
            ("research x in under 30 seconds", 30),
        ] {
            let parsed = parse(text);
            let caps = parsed
                .caps()
                .unwrap_or_else(|| panic!("{text:?} should parse, got {parsed:?}"));
            assert_eq!(
                caps.latency,
                Some(Duration::from_secs(secs)),
                "{text:?} produced {caps:?}"
            );
        }
    }

    #[test]
    fn a_latency_only_run_is_runnable_because_quarry_accepts_time_alone() {
        let caps = caps_of("research x within 20 minutes");
        assert_eq!(caps.spend_micro_usd, None);
        assert!(caps.validate().is_ok());
    }

    // ── Due ──────────────────────────────────────────────────────────────────

    #[test]
    fn by_tonight_resolves_to_the_end_of_the_senders_day_not_the_gateways() {
        // 14:30 UTC is 10:30 in New York, so "tonight" is 23:59 EDT = 03:59 UTC
        // the next day. Resolved in UTC it would have been 23:59 the same day —
        // four hours less compute than the sender asked for.
        let parsed = parse("research x by tonight");
        let due = parsed.caps().unwrap().due.unwrap();
        assert_eq!(due, Utc.with_ymd_and_hms(2026, 8, 5, 3, 59, 0).unwrap());
        let local = due.with_timezone(&chrono_tz::America::New_York);
        assert_eq!((local.hour(), local.minute()), (23, 59));
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()
        );
    }

    #[test]
    fn the_same_phrase_in_a_different_zone_resolves_to_a_different_instant() {
        let tokyo = SenderTimezone {
            tz: chrono_tz::Asia::Tokyo,
            source: TimezoneSource::SenderPreference,
        };
        let ny_due = parse_caps("research x by tonight", now(), ny())
            .caps()
            .unwrap()
            .due
            .unwrap();
        let tokyo_due = parse_caps("research x by tonight", now(), tokyo)
            .caps()
            .unwrap()
            .due
            .unwrap();
        assert_ne!(
            ny_due, tokyo_due,
            "a deadline is a price control; the sender's zone has to change the instant"
        );
    }

    #[test]
    fn clock_deadlines_resolve_forward_in_the_senders_day() {
        // Local now is 10:30 EDT. 5pm is later today; 9am has passed, so it means
        // tomorrow — a deadline in the past would be an instantly-truncated run.
        let five_pm = parse("research x by 5pm").caps().unwrap().due.unwrap();
        let local = five_pm.with_timezone(&chrono_tz::America::New_York);
        assert_eq!((local.hour(), local.minute()), (17, 0));
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 4).unwrap()
        );

        let nine_am = parse("research x by 9am").caps().unwrap().due.unwrap();
        let local = nine_am.with_timezone(&chrono_tz::America::New_York);
        assert_eq!((local.hour(), local.minute()), (9, 0));
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 5).unwrap(),
            "a time already past today means tomorrow"
        );
        assert!(nine_am > now(), "a deadline in the past is not a deadline");
    }

    #[test]
    fn by_five_is_ambiguous_and_is_asked_about() {
        // Twelve hours apart, and the sender's whole budget rides on which.
        let parsed = parse("research x by 5");
        let q = &parsed.questions()[0];
        assert_eq!(q.code(), "ambiguous");
        assert!(parsed.caps().is_none());
        assert!(q.message().contains("5am"));
        assert!(q.message().contains("5pm"));
    }

    #[test]
    fn by_tomorrow_morning_and_by_friday_resolve_to_local_instants() {
        let tomorrow_am = parse("research x by tomorrow morning")
            .caps()
            .unwrap()
            .due
            .unwrap();
        let local = tomorrow_am.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 5).unwrap()
        );
        assert_eq!((local.hour(), local.minute()), (9, 0));

        // 2026-08-04 is a Tuesday, so Friday is the 7th.
        let friday = parse("research x by friday").caps().unwrap().due.unwrap();
        let local = friday.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 7).unwrap()
        );
        assert_eq!((local.hour(), local.minute()), (23, 59));
    }

    #[test]
    fn a_weekday_naming_today_means_next_week_not_a_deadline_already_gone() {
        // Said on a Tuesday, "by Tuesday" means the next one. Reading it as today
        // would resolve to 23:59 tonight, which is a much smaller budget than the
        // sender asked for — and if said after 23:59 would be in the past.
        let due = parse("research x by tuesday").caps().unwrap().due.unwrap();
        let local = due.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(
            local.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 11).unwrap()
        );
    }

    #[test]
    fn a_deadline_in_a_daylight_saving_gap_moves_later_never_earlier() {
        // 2026-03-08 02:30 does not exist in New York — the clocks jump 02:00 →
        // 03:00. Moving the deadline earlier would silently shrink the budget, so
        // the resolution walks forward to the first instant that exists.
        let march = Utc.with_ymd_and_hms(2026, 3, 8, 5, 0, 0).unwrap(); // 00:00 EST
        let parsed = parse_caps("research x by 2:30am", march, ny());
        let due = parsed
            .caps()
            .unwrap_or_else(|| panic!("should resolve, got {parsed:?}"))
            .due
            .unwrap();
        assert!(
            due > march,
            "the resolved deadline must still be in the future"
        );
        let local = due.with_timezone(&chrono_tz::America::New_York);
        assert_eq!(
            (local.hour(), local.minute()),
            (3, 0),
            "02:30 does not exist; the next valid local instant is 03:00"
        );
    }

    // ── Disclosures ──────────────────────────────────────────────────────────

    #[test]
    fn a_due_deadline_discloses_that_upstream_has_no_flag_for_it() {
        let parsed = parse("research x by tonight");
        let d = parsed
            .disclosures()
            .iter()
            .find_map(|d| match d {
                Disclosure::DueHasNoUpstreamFlag {
                    equivalent_latency, ..
                } => Some(*equivalent_latency),
                _ => None,
            })
            .expect("the Latency substitution must be disclosed, never silent");
        // 14:30 UTC → 03:59 UTC next day is 13h29m.
        assert_eq!(d, Duration::from_secs(13 * 3600 + 29 * 60));
    }

    #[test]
    fn a_due_only_run_is_deferrable_and_a_substituted_one_would_not_be() {
        // This is why the substitution is disclosed rather than performed: with a
        // Latency set, Deferrable() is false, and the batch/off-peak price is
        // gone.
        let due_only = caps_of("research x by tonight");
        assert!(due_only.deferrable());

        let substituted = RequestedCaps {
            latency: Some(Duration::from_secs(3600)),
            ..due_only
        };
        assert!(
            !substituted.deferrable(),
            "substituting Latency for Due forfeits the cheap path"
        );
    }

    #[test]
    fn the_resolved_deadline_and_its_timezone_are_echoed_for_checking() {
        let parsed = parse("research x by 5pm");
        let (zone, source, local) = parsed
            .disclosures()
            .iter()
            .find_map(|d| match d {
                Disclosure::DeadlineResolvedIn {
                    timezone,
                    source,
                    local,
                } => Some((timezone.clone(), *source, local.clone())),
                _ => None,
            })
            .expect("a resolved deadline must say which zone resolved it");
        assert_eq!(zone, "America/New_York");
        assert_eq!(source, TimezoneSource::SenderPreference);
        // Wall-clock text a human can check against what they meant.
        assert!(local.contains("17:00"), "got {local:?}");
    }

    #[test]
    fn a_utc_fallback_deadline_says_it_fell_back() {
        // The disclosure has to distinguish "resolved in your zone" from
        // "resolved in UTC because nothing was configured" — only the sender can
        // tell whether the second is right.
        let parsed = parse_caps(
            "research x by tonight",
            now(),
            SenderTimezone::utc_fallback(),
        );
        let source = parsed
            .disclosures()
            .iter()
            .find_map(|d| match d {
                Disclosure::DeadlineResolvedIn { source, .. } => Some(*source),
                _ => None,
            })
            .unwrap();
        assert_eq!(source, TimezoneSource::UtcFallback);
        assert_eq!(source.code(), "utc_fallback");
    }

    #[test]
    fn no_deadline_means_no_deadline_disclosures() {
        let parsed = parse("research x, up to $5");
        assert!(parsed.disclosures().is_empty());
    }

    // ── Timezone fallback chain ──────────────────────────────────────────────

    #[test]
    fn the_timezone_fallback_chain_runs_sender_then_config_then_utc() {
        let s = SenderTimezone::resolve(Some("Europe/Berlin"), Some("America/Denver"));
        assert_eq!(s.tz, chrono_tz::Europe::Berlin);
        assert_eq!(s.source, TimezoneSource::SenderPreference);

        let s = SenderTimezone::resolve(None, Some("America/Denver"));
        assert_eq!(s.tz, chrono_tz::America::Denver);
        assert_eq!(s.source, TimezoneSource::ConfigDefault);

        let s = SenderTimezone::resolve(None, None);
        assert_eq!(s.tz, chrono_tz::UTC);
        assert_eq!(s.source, TimezoneSource::UtcFallback);
    }

    #[test]
    fn an_unparseable_zone_falls_through_and_reports_what_was_actually_used() {
        // The trap: reporting SenderPreference for a zone that failed to parse
        // would attribute a UTC-resolved deadline to the sender's own zone.
        let s = SenderTimezone::resolve(Some("Mars/Olympus_Mons"), Some("America/Denver"));
        assert_eq!(s.tz, chrono_tz::America::Denver);
        assert_eq!(s.source, TimezoneSource::ConfigDefault);

        let s = SenderTimezone::resolve(Some("nonsense"), Some("also nonsense"));
        assert_eq!(s.tz, chrono_tz::UTC);
        assert_eq!(s.source, TimezoneSource::UtcFallback);
    }

    // ── Non-denominations ────────────────────────────────────────────────────

    #[test]
    fn an_agent_count_is_recognised_as_not_a_cap() {
        // Neither invented as a cap nor silently dropped: a dropped limit means
        // the run comes back bounded by something the sender never set.
        for text in [
            "research x with at most 30 agents",
            "research x, no more than 10 workers",
            "research x with a limit of 5 subagents",
        ] {
            let parsed = parse(text);
            assert!(parsed.caps().is_none(), "{text:?} produced {parsed:?}");
            let q = parsed
                .questions()
                .iter()
                .find(|q| q.code() == "not_a_denomination")
                .unwrap_or_else(|| panic!("{text:?} gave {parsed:?}"));
            assert!(q.message().contains("no "), "{}", q.message());
        }
    }

    #[test]
    fn a_depth_limit_is_refused_with_quarrys_own_reason() {
        let parsed = parse("research x, no deeper than 3 levels");
        let q = &parsed.questions()[0];
        assert_eq!(q.code(), "not_a_denomination");
        // Depth is a backstop, and a run bounded by it is under-verified rather
        // than complete — the sender should know that before choosing it.
        assert!(q.message().contains("backstop"), "{}", q.message());
        assert!(q.message().contains("verifiers"), "{}", q.message());
    }

    #[test]
    fn a_token_budget_is_redirected_to_the_spend_cap_that_actually_covers_it() {
        let parsed = parse("research x with at most 100000 tokens");
        let q = &parsed.questions()[0];
        assert_eq!(q.code(), "not_a_denomination");
        assert!(q.message().contains("token"), "{}", q.message());
        assert!(q.message().contains('$'), "{}", q.message());
    }

    #[test]
    fn a_non_denomination_blocks_the_run_even_when_a_real_cap_is_present() {
        // The sender wrote two constraints. Honouring one and ignoring the other
        // runs under half of what they asked for, so this asks rather than
        // proceeding.
        let parsed = parse("research x, up to $5, with at most 30 agents");
        assert!(parsed.caps().is_none(), "got {parsed:?}");
        assert_eq!(parsed.questions()[0].code(), "not_a_denomination");
    }

    // ── Absent and combined ──────────────────────────────────────────────────

    #[test]
    fn no_cap_at_all_asks_and_quotes_quarrys_refusal() {
        let parsed = parse("research the history of the transistor");
        assert!(parsed.caps().is_none());
        let q = &parsed.questions()[0];
        assert_eq!(q.code(), "no_cap_found");
        // quarry's own reason, so the sender learns the constraint rather than
        // hitting a gateway quirk.
        assert!(
            q.message().contains("budget-conditioned"),
            "{}",
            q.message()
        );
        assert!(q.message().contains("P9"), "{}", q.message());
    }

    #[test]
    fn an_empty_message_asks_rather_than_defaulting() {
        assert!(parse("").caps().is_none());
        assert!(parse("   ").caps().is_none());
    }

    #[test]
    fn several_caps_in_one_message_all_parse() {
        let caps = caps_of("research x, $5 and by tonight");
        assert_eq!(caps.spend_micro_usd, Some(5_000_000));
        assert!(caps.due.is_some());
        assert_eq!(caps.latency, None);
        assert!(
            caps.deferrable(),
            "spend + due with no latency is deferrable"
        );
    }

    #[test]
    fn all_three_denominations_can_be_set_at_once() {
        let caps = caps_of("research x, up to $5, within 20 minutes, by 5pm");
        assert_eq!(caps.spend_micro_usd, Some(5_000_000));
        assert_eq!(caps.latency, Some(Duration::from_secs(1200)));
        assert!(caps.due.is_some());
        assert!(
            !caps.deferrable(),
            "a latency cap means the run is needed soon, so the cheap path is closed"
        );
    }

    // ── Validation mirrors quarry ────────────────────────────────────────────

    #[test]
    fn validate_refuses_an_uncapped_set_the_way_quarry_does() {
        let empty = RequestedCaps::default();
        assert_eq!(empty.validate(), Err(CapsRefusal::Uncapped));
        assert!(empty.validate().unwrap_err().message().contains("P9"));
    }

    #[test]
    fn validate_refuses_a_non_positive_spend_but_accepts_unlimited() {
        let zero = RequestedCaps {
            spend_micro_usd: Some(0),
            ..Default::default()
        };
        assert_eq!(zero.validate(), Err(CapsRefusal::SpendNotPositive));

        // Unlimited is -1 and must not trip the `<= 0` check — the reason
        // Limited() is consulted first.
        let unlimited = RequestedCaps {
            spend_micro_usd: Some(UNLIMITED_MICRO_USD),
            ..Default::default()
        };
        assert!(unlimited.validate().is_ok());
    }

    #[test]
    fn refusal_and_question_codes_are_unique() {
        // These codes land in audit records; two causes sharing one code is how a
        // reader ends up string-matching a message to tell them apart.
        let codes = [
            CapsRefusal::Uncapped.code(),
            CapsRefusal::SpendNotPositive.code(),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in codes {
            assert!(seen.insert(c), "duplicate refusal code {c}");
        }
        let qcodes = [
            Question::NoCapFound.code(),
            Question::Ambiguous {
                fragment: String::new(),
                readings: vec![],
            }
            .code(),
            Question::NotADenomination {
                fragment: String::new(),
                reason: String::new(),
            }
            .code(),
            Question::UnusableSpend {
                fragment: String::new(),
                reason: String::new(),
            }
            .code(),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in qcodes {
            assert!(seen.insert(c), "duplicate question code {c}");
        }
    }

    #[test]
    fn parsing_is_deterministic_and_offline() {
        // Same inputs, same output — including `now`, which is why it is a
        // parameter. A parser that read the clock could not be asserted against
        // an exact instant.
        let a = parse("research x, up to $5.50 by tomorrow morning");
        let b = parse("research x, up to $5.50 by tomorrow morning");
        assert_eq!(a, b);
    }

    #[test]
    fn adversarial_inputs_never_produce_a_surprise_cap() {
        // Each of these must either parse to exactly what it says or ask. None
        // may quietly become a budget.
        for text in [
            "research x, $0",
            "research x, -$5",
            "research x, $5 or maybe $50",
            "research x, don't spend anything",
            "research x by 5",
            "research x with at most 30 agents",
            "research $ x",
            "research x, $",
            "research x, usd",
            "research x, spend up to",
        ] {
            let parsed = parse(text);
            if let Some(caps) = parsed.caps() {
                panic!("{text:?} should not have produced a cap, got {caps:?}");
            }
            assert!(
                !parsed.questions().is_empty(),
                "{text:?} produced neither caps nor a question"
            );
        }
    }
}
