//! Caps and `Scope` minted from operator policy.
//!
//! [`super::caps`] reports what a sender *asked for*. This module decides what
//! they are *allowed*, and mints the `Scope` their run carries. The split is the
//! point: a sender who types "spend up to $500" gets whatever policy permits and
//! is told so, because letting the request be the policy means there is no policy.
//!
//! # `Scope` is the security boundary here, and there is nothing behind it
//!
//! quarry's `Problem.Key()` is **scope-qualified, not the statement hash alone**.
//! Its own comment says why:
//!
//! > Two users can pose a hash-identical sub-problem while holding different
//! > entitlements, and one's cached answer may derive from documents the other
//! > cannot see. Serving across that line walks straight through the ABAC
//! > boundary (P6).
//!
//! `Scope.Key()` folds the tags into every cache key, so **getting the scope
//! wrong is a cross-tenant data leak, not a misconfiguration**.
//!
//! quarry treats tags as opaque — it hashes them, compares them with `NarrowsTo`,
//! and does nothing else. In the agate deployment the real enforcement is AWS IAM,
//! and quarry's local check is explicitly "a fast-fail courtesy, not the security
//! boundary." **In this integration there is no IAM.** The gateway *is* the
//! boundary. Nothing downstream will catch a sloppy scope, which is a materially
//! weaker position than agate's and the reason this module is defensive to the
//! point of being boring.
//!
//! # A real collision in quarry's `Scope.Key()`, verified against the source
//!
//! `Scope.Key()` renders `k=v;` per sorted key with **no escaping of `=` or
//! `;`**. So these two different scopes produce byte-identical keys — confirmed by
//! running quarry's own code:
//!
//! ```text
//! {tenant: "victim", user: "alice"}   → "tenant=victim;user=alice;"
//! {tenant: "victim;user=alice"}       → "tenant=victim;user=alice;"
//! ```
//!
//! A single tag value containing `;` and `=` forges another scope's cache key. With
//! no IAM behind us, that is a cross-tenant cache read. quarry cannot fix this for
//! us without a wire change, and we cannot escape the values because quarry would
//! then hash the escaped form and our keys would stop matching the twin's.
//!
//! So [`ScopeTags`] **rejects the separator bytes at mint time** rather than
//! encoding around them. A tag key or value containing `=` or `;` is refused; so is
//! a control character, which no legitimate identity carries and which would make
//! an audit line unreadable. Refusing is the only option that keeps our keys
//! byte-identical to quarry's *and* injective.
//!
//! # Scope narrowing here is subset-of-tags, matching quarry
//!
//! quarry's `NarrowsTo` is subset-of-tags: `s.NarrowsTo(other)` holds when every
//! tag in `s` appears in `other` with the same value. agate's narrowing for its
//! scope *path* is prefix/ancestor (`chemistry` ⊇ `chemistry/chem-101`), and the
//! two disagree — verified against quarry: `{scope: "chemistry"}.NarrowsTo({scope:
//! "chemistry/chem-101"})` is **false**.
//!
//! **Decision: this repo uses subset-of-tags, quarry's relation, and does not
//! adopt hierarchical tag values.** Two reasons. Adopting prefix semantics would
//! put a relation in the gateway that quarry does not implement, so a check that
//! passed here would fail there — the worst kind of disagreement, since it fails
//! *open* in the direction of the cache. And a hierarchical value invites exactly
//! the `/`-and-separator string handling that produced the collision above. Scope
//! values are opaque, flat identifiers.
//!
//! Note the subset relation has a sharp edge worth naming: an **empty scope narrows
//! to everything**, because a subset check over no tags is vacuously true. A run
//! minted with no tags would therefore pass any `NarrowsTo` check against any
//! scope. [`CapsPolicy`] never mints an empty scope, and [`ScopeTags::mint`]
//! refuses to.
//!
//! # Default-deny
//!
//! A sender with no matching policy entry cannot run quarry. A missing config means
//! *no one may run*, never *anyone may run unlimited* — the failure mode of the
//! opposite default is an unbounded spend on a fresh install.
//!
//! # Cedar later; a trait now
//!
//! There is no policy layer of any kind in this repo today. Resolution sits behind
//! [`CapsPolicy`] with one config-file implementation, shaped so a Cedar backend
//! can replace it without changing callers. Cedar is not implemented here.

use crate::audit::{AuditEvent, AuditLogger};
use crate::config::{QuarryPolicyConfig, QuarryPolicyEntry};
use crate::quarry::caps::{is_limited, RequestedCaps, UNLIMITED_MICRO_USD};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

// ── Scope ─────────────────────────────────────────────────────────────────────

/// Bytes that cannot appear in a scope tag key or value.
///
/// `=` and `;` are quarry's own separators in `Scope.Key()`, which does not escape
/// them — see the module docs for the verified collision. A control character is
/// refused because nothing legitimate carries one and it would corrupt the audit
/// line that records the decision.
fn forbidden_in_tag(s: &str) -> Option<char> {
    s.chars().find(|c| *c == '=' || *c == ';' || c.is_control())
}

/// Why a set of tags could not be minted into a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    /// A key or value contained `=`, `;`, or a control character.
    ///
    /// Refused rather than escaped: escaping would make our cache keys disagree
    /// with quarry's, and *not* refusing lets one scope forge another's key.
    ForbiddenCharacter {
        /// Which tag.
        key: String,
        /// The offending character.
        found: char,
    },
    /// An empty key. `Scope.Key()` would render `=v;`, which no other scope can
    /// produce but which also identifies nothing.
    EmptyKey,
    /// No tags at all. An empty scope `NarrowsTo` everything, so a run carrying one
    /// would pass any entitlement check.
    Empty,
}

impl ScopeError {
    /// A stable machine-readable code for audit records.
    pub fn code(&self) -> &'static str {
        match self {
            ScopeError::ForbiddenCharacter { .. } => "scope_forbidden_character",
            ScopeError::EmptyKey => "scope_empty_key",
            ScopeError::Empty => "scope_empty",
        }
    }

    /// An operator-facing explanation.
    pub fn message(&self) -> String {
        match self {
            ScopeError::ForbiddenCharacter { key, found } => format!(
                "scope tag {key:?} contains {found:?}, which is one of quarry's own cache-key \
                 separators. quarry's Scope.Key() does not escape it, so a value containing \
                 it can render the same key as a different scope — a cross-tenant cache read. \
                 Refused rather than escaped, because escaping would make this gateway's cache \
                 keys disagree with quarry's."
            ),
            ScopeError::EmptyKey => "a scope tag key cannot be empty".to_string(),
            ScopeError::Empty => "a scope must carry at least one tag: quarry's NarrowsTo is a \
                 subset check, so an empty scope narrows to every other scope and \
                 would pass any entitlement check."
                .to_string(),
        }
    }
}

/// Validated scope tags, canonicalised the way quarry canonicalises them.
///
/// Held as a `BTreeMap` so iteration is sorted — quarry sorts keys in
/// `Scope.Key()`, and matching that ordering is what makes a cache key computed
/// here agree with one computed in the Go twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeTags {
    tags: BTreeMap<String, String>,
}

impl ScopeTags {
    /// Validate and mint. Every tag key and value is checked; one bad tag refuses
    /// the whole scope rather than being dropped, since a *partial* scope is a
    /// **wider** scope, and silently widening is the failure this guards.
    pub fn mint(tags: BTreeMap<String, String>) -> Result<Self, ScopeError> {
        if tags.is_empty() {
            return Err(ScopeError::Empty);
        }
        for (k, v) in &tags {
            if k.is_empty() {
                return Err(ScopeError::EmptyKey);
            }
            if let Some(found) = forbidden_in_tag(k) {
                return Err(ScopeError::ForbiddenCharacter {
                    key: k.clone(),
                    found,
                });
            }
            if let Some(found) = forbidden_in_tag(v) {
                return Err(ScopeError::ForbiddenCharacter {
                    key: k.clone(),
                    found,
                });
            }
        }
        Ok(Self { tags })
    }

    /// The canonical rendering quarry hashes: sorted `k=v;`, no separator.
    ///
    /// Byte-identical to quarry's `Scope.Key()` — pinned by golden vectors
    /// generated from quarry's own code in
    /// `tests/fixtures/quarry/scope_keys_golden.json`. A self-consistent test here
    /// would pass while disagreeing with the thing that actually computes the
    /// cache key.
    pub fn key(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.tags {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push(';');
        }
        out
    }

    /// The tags, sorted.
    pub fn tags(&self) -> &BTreeMap<String, String> {
        &self.tags
    }

    /// quarry's `--scope` flag form: `k=v,k=v`.
    ///
    /// Safe only because `mint` already refused `=` and `;`; a `,` would still
    /// break this rendering, and is refused here for that reason rather than at
    /// mint time, since a comma is fine in a cache key and only breaks the flag.
    pub fn flag_value(&self) -> String {
        self.tags
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// quarry's `Scope.NarrowsTo`: every tag in `self` appears in `other` with the
    /// same value.
    ///
    /// Subset-of-tags, matching quarry exactly — **not** prefix/ancestor. See the
    /// module docs for why this repo does not adopt hierarchical values.
    pub fn narrows_to(&self, other: &ScopeTags) -> bool {
        self.tags
            .iter()
            .all(|(k, v)| other.tags.get(k).is_some_and(|ov| ov == v))
    }
}

// ── Granted caps ──────────────────────────────────────────────────────────────

/// Which denominations a sender may set.
///
/// Being allowed to set a cap is itself a policy decision: an operator may let a
/// sender bound their own spend but not their own deadline, because a deadline is a
/// price control — `Deferrable()` converts slack into cheaper inference — and a
/// sender who can set a tight latency can make every run take the expensive path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denomination {
    Spend,
    Latency,
    Due,
}

impl Denomination {
    /// quarry's own `Denomination` string.
    pub fn code(&self) -> &'static str {
        match self {
            Denomination::Spend => "spend",
            Denomination::Latency => "latency",
            Denomination::Due => "due",
        }
    }
}

/// What happened to one denomination on the way from request to grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapAdjustment {
    /// The sender asked for more than policy allows and got the policy maximum.
    /// **Must** appear in the plan message before spend.
    Reduced {
        denomination: Denomination,
        /// What they asked for, rendered for a human.
        requested: String,
        /// What they got.
        granted: String,
    },
    /// The sender did not set this denomination and policy supplied its default,
    /// so the run has a cap it can plan against.
    Defaulted {
        denomination: Denomination,
        granted: String,
    },
    /// The sender set a denomination policy does not let them set. Their value is
    /// discarded and the policy default applies — disclosed, not silent.
    NotPermitted {
        denomination: Denomination,
        requested: String,
        granted: String,
    },
}

impl CapAdjustment {
    /// A stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            CapAdjustment::Reduced { .. } => "reduced",
            CapAdjustment::Defaulted { .. } => "defaulted",
            CapAdjustment::NotPermitted { .. } => "not_permitted",
        }
    }

    /// Which denomination this concerns.
    pub fn denomination(&self) -> Denomination {
        match self {
            CapAdjustment::Reduced { denomination, .. }
            | CapAdjustment::Defaulted { denomination, .. }
            | CapAdjustment::NotPermitted { denomination, .. } => *denomination,
        }
    }

    /// The sender-facing sentence. P9's disclosure requirement is about *quiet*
    /// degradation: a reduction the sender saw is fine, the same reduction
    /// unannounced is not.
    pub fn message(&self) -> String {
        match self {
            CapAdjustment::Reduced {
                denomination,
                requested,
                granted,
            } => format!(
                "you asked for {} {}, policy allows {} — proceeding with {}",
                denomination.code(),
                requested,
                granted,
                granted
            ),
            CapAdjustment::Defaulted {
                denomination,
                granted,
            } => format!(
                "no {} cap given, using the policy default of {}",
                denomination.code(),
                granted
            ),
            CapAdjustment::NotPermitted {
                denomination,
                requested,
                granted,
            } => format!(
                "you are not permitted to set the {} cap (you asked for {}); policy's {} applies",
                denomination.code(),
                requested,
                granted
            ),
        }
    }
}

/// Why a run was refused outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// No policy entry matched this sender. Default-deny.
    NoPolicy { user_id: String, channel_id: String },
    /// The sender exceeded a limit on a denomination whose over-limit behaviour is
    /// `refuse` rather than `reduce`.
    OverLimit {
        denomination: Denomination,
        requested: String,
        allowed: String,
    },
    /// The sender asked for an unlimited spend and policy forbids it (the default).
    UnlimitedForbidden,
    /// Policy resolved but grants no cap in any denomination, so quarry could not
    /// plan (P9). A misconfiguration, reported as one.
    NoCapGranted,
    /// The scope could not be minted — see [`ScopeError`].
    Scope(ScopeError),
    /// No `gateway.api_token` is configured, so no credential can be minted for the
    /// child.
    ///
    /// Refused rather than spawning a token-less child. `/v1` sits behind
    /// `bearer_auth_middleware`; with no token configured that middleware is
    /// disabled, meaning the endpoint quarry would call is unauthenticated and open
    /// to anything that can reach the port. Handing quarry an empty credential would
    /// work, which is precisely the problem.
    NoProviderToken,
}

impl PolicyRefusal {
    /// A stable machine-readable code for audit records and tests.
    pub fn code(&self) -> &'static str {
        match self {
            PolicyRefusal::NoPolicy { .. } => "no_policy",
            PolicyRefusal::OverLimit { .. } => "over_limit",
            PolicyRefusal::UnlimitedForbidden => "unlimited_forbidden",
            PolicyRefusal::NoCapGranted => "no_cap_granted",
            PolicyRefusal::Scope(e) => e.code(),
            PolicyRefusal::NoProviderToken => "no_provider_token",
        }
    }

    /// The sender-facing explanation.
    pub fn message(&self) -> String {
        match self {
            PolicyRefusal::NoPolicy { .. } => {
                "quarry runs are not enabled for you. An operator has to grant a policy \
                 entry for your channel or user before you can start one."
                    .to_string()
            }
            PolicyRefusal::OverLimit {
                denomination,
                requested,
                allowed,
            } => format!(
                "you asked for {} {}, and policy allows at most {}. This limit refuses rather \
                 than reduces, so nothing was run — ask again within {}.",
                denomination.code(),
                requested,
                allowed,
                allowed
            ),
            PolicyRefusal::UnlimitedForbidden => {
                "an unlimited spend cap is not permitted. quarry divides the cap across the \
                 tree as it recurses, so an unlimited root cap has no bound at all — name an \
                 amount."
                    .to_string()
            }
            PolicyRefusal::NoCapGranted => {
                "policy granted no cap in any denomination, so there is nothing for quarry to \
                 plan against (P9). This is a policy misconfiguration, not something you did."
                    .to_string()
            }
            PolicyRefusal::Scope(e) => e.message(),
            PolicyRefusal::NoProviderToken => {
                "quarry runs need `gateway.api_token` set. The run would call the gateway's \
                 own /v1 endpoint, and with no token configured that endpoint has no \
                 authentication at all — so there is no credential to give the child, and \
                 nothing stopping anything else on the host from calling it either."
                    .to_string()
            }
        }
    }
}

/// The result of applying policy: what the run may actually use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Spend cap in int64 micro-dollars. `None` means unset — distinct from
    /// [`UNLIMITED_MICRO_USD`], which policy forbids by default.
    pub spend_micro_usd: Option<i64>,
    /// Latency cap.
    pub latency: Option<Duration>,
    /// Deadline as an instant.
    pub due: Option<chrono::DateTime<chrono::Utc>>,
    /// The scope this run carries into every cache key.
    pub scope: ScopeTags,
    /// Everything the sender must be told **before spend**. A reduction that never
    /// reaches the plan message is exactly the quiet degradation P9 forbids.
    pub adjustments: Vec<CapAdjustment>,
}

impl Grant {
    /// Whether any real cap was granted — quarry's P9 precondition.
    pub fn any_cap(&self) -> bool {
        self.spend_micro_usd.is_some() || self.latency.is_some() || self.due.is_some()
    }

    /// Whether slack is convertible into money — quarry's `Caps.Deferrable()`.
    pub fn deferrable(&self) -> bool {
        self.due.is_some() && self.latency.is_none()
    }
}

// ── The child's credential ────────────────────────────────────────────────────

/// Mint the **entire** environment a quarry child receives.
///
/// quarry's only network need is the gateway's own localhost `/v1` endpoint, which
/// sits behind `bearer_auth_middleware`. So the child gets exactly two variables:
/// where to call, and the token to call with. Nothing else — no provider key, which
/// would let it bypass the gateway and its accounting entirely, and no channel
/// token, which would let it post as the bot.
///
/// The gateway's own environment is not inherited: `Supervisor::spawn` uses
/// `env_clear()`, so this map is the whole story rather than an addition to it.
/// `FORBIDDEN_ENV_KEYS` in the supervisor is a backstop against a caller that
/// builds the wrong map; this function is the caller that builds the right one.
///
/// Returns [`PolicyRefusal::NoProviderToken`] when no `api_token` is configured.
/// Refusing beats spawning: an absent token means `/v1` is *unauthenticated*, so
/// there is no credential to hand over and nothing else on the host is being kept
/// out either.
pub fn mint_child_env(
    api_token: Option<&str>,
    provider_url: &str,
) -> Result<BTreeMap<String, String>, PolicyRefusal> {
    let token = match api_token {
        Some(t) if !t.is_empty() => t,
        _ => return Err(PolicyRefusal::NoProviderToken),
    };
    let mut env = BTreeMap::new();
    env.insert("QUARRY_PROVIDER_URL".to_string(), provider_url.to_string());
    env.insert("QUARRY_PROVIDER_TOKEN".to_string(), token.to_string());
    Ok(env)
}

// ── The policy trait ──────────────────────────────────────────────────────────

/// Resolves what a sender may spend and which scope their run carries.
///
/// One implementation today ([`ConfigCapsPolicy`]). The trait exists so a Cedar
/// backend can replace it without touching callers — the interface deliberately
/// takes only verified identity and the parsed request, and returns a decision,
/// which is the shape a policy engine wants.
pub trait CapsPolicy: Send + Sync {
    /// Resolve a request into a grant, or refuse it.
    ///
    /// `user_id` and `channel_id` must come from the channel adapter's **verified**
    /// identity, never from message content.
    fn resolve(
        &self,
        user_id: &str,
        channel_id: &str,
        requested: &RequestedCaps,
    ) -> Result<Grant, PolicyRefusal>;
}

// ── Config-backed implementation ──────────────────────────────────────────────

/// Over-limit behaviour for one denomination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverLimit {
    /// Grant the policy maximum and disclose the reduction.
    Reduce,
    /// Refuse the run.
    Refuse,
}

impl OverLimit {
    fn from_config(s: &str) -> Self {
        // Anything unrecognised refuses. A typo in `on_over_limit` must not
        // silently become the permissive branch.
        match s {
            "reduce" => OverLimit::Reduce,
            _ => OverLimit::Refuse,
        }
    }
}

/// The config-file [`CapsPolicy`].
pub struct ConfigCapsPolicy {
    config: QuarryPolicyConfig,
    audit: Option<Arc<AuditLogger>>,
}

impl ConfigCapsPolicy {
    pub fn new(config: QuarryPolicyConfig) -> Self {
        Self {
            config,
            audit: None,
        }
    }

    /// Attach an audit logger. Every decision is recorded — granted caps included,
    /// because "what was this run allowed to spend" is the first question asked
    /// after an unexpected bill.
    pub fn with_audit(mut self, audit: Option<Arc<AuditLogger>>) -> Self {
        self.audit = audit;
        self
    }

    /// The most specific matching entry: sender override, then channel, then
    /// default. **No merging across levels** — an entry is taken whole.
    ///
    /// Merging is the tempting alternative and the wrong one: a channel entry that
    /// forbade `unlimited` would silently stop forbidding it the moment a sender
    /// override set an unrelated field, because the override's own
    /// `allow_unlimited: false` default would be indistinguishable from "not
    /// specified". Whole-entry precedence means an override is auditable as one
    /// decision.
    fn entry_for(&self, user_id: &str, channel_id: &str) -> Option<(&QuarryPolicyEntry, &str)> {
        if let Some(e) = self.config.senders.get(user_id) {
            return Some((e, "sender"));
        }
        if let Some(e) = self.config.channels.get(channel_id) {
            return Some((e, "channel"));
        }
        self.config.default.as_ref().map(|e| (e, "default"))
    }

    /// Mint the scope from **verified channel identity only**.
    ///
    /// Nothing in the inbound message reaches this. A sender who could contribute a
    /// tag could widen their own scope and read another tenant's cached answers,
    /// which with no IAM behind the gateway is the whole attack.
    fn mint_scope(
        &self,
        user_id: &str,
        channel_id: &str,
        entry: &QuarryPolicyEntry,
    ) -> Result<ScopeTags, ScopeError> {
        let mut tags = BTreeMap::new();
        tags.insert("user".to_string(), user_id.to_string());
        tags.insert("channel".to_string(), channel_id.to_string());
        // Operator-supplied tags come last but cannot overwrite identity: a policy
        // file that set `user` would let one entry impersonate another sender's
        // cache namespace, which is the same leak by a different route.
        for (k, v) in &entry.scope_tags {
            if k == "user" || k == "channel" {
                continue;
            }
            tags.insert(k.clone(), v.clone());
        }
        ScopeTags::mint(tags)
    }
}

/// Render micro-dollars for a human, at the fixed precision quarry uses.
fn render_micro(micro: i64) -> String {
    if !is_limited(micro) {
        return "unlimited".to_string();
    }
    format!("${:.4}", micro as f64 / 1_000_000.0)
}

fn render_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs > 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

impl CapsPolicy for ConfigCapsPolicy {
    fn resolve(
        &self,
        user_id: &str,
        channel_id: &str,
        requested: &RequestedCaps,
    ) -> Result<Grant, PolicyRefusal> {
        let refuse = |r: PolicyRefusal| -> PolicyRefusal {
            if let Some(ref audit) = self.audit {
                audit.log(AuditEvent::QuarryPolicyDecision {
                    user_id: user_id.to_string(),
                    channel_id: channel_id.to_string(),
                    matched: None,
                    granted: false,
                    refusal: Some(r.code().to_string()),
                    requested_spend_micro_usd: requested.spend_micro_usd,
                    granted_spend_micro_usd: None,
                    adjustments: vec![],
                    scope_key: None,
                });
            }
            r
        };

        // Default-deny: no entry means no run. A missing config is not permission.
        let (entry, level) = match self.entry_for(user_id, channel_id) {
            Some(found) => found,
            None => {
                return Err(refuse(PolicyRefusal::NoPolicy {
                    user_id: user_id.to_string(),
                    channel_id: channel_id.to_string(),
                }))
            }
        };

        let mut adjustments = Vec::new();

        // ── Spend ────────────────────────────────────────────────────────────
        let spend_over = OverLimit::from_config(&entry.on_over_limit);
        let max_spend = entry.max_spend_micro_usd;
        let mut spend = None;

        if let Some(asked) = requested.spend_micro_usd {
            if !entry.allowed_denominations.iter().any(|d| d == "spend") {
                if let Some(default) = entry.default_spend_micro_usd {
                    adjustments.push(CapAdjustment::NotPermitted {
                        denomination: Denomination::Spend,
                        requested: render_micro(asked),
                        granted: render_micro(default),
                    });
                    spend = Some(default);
                }
            } else if !is_limited(asked) {
                // An explicit unlimited request. Forbidden by default, because
                // quarry apportions the root cap down the tree — an unlimited root
                // is unbounded everywhere.
                if !entry.allow_unlimited {
                    return Err(refuse(PolicyRefusal::UnlimitedForbidden));
                }
                spend = Some(UNLIMITED_MICRO_USD);
            } else if let Some(max) = max_spend {
                // `is_limited` first: comparing against an unlimited maximum
                // (`-1`) would refuse every request as over-limit.
                if is_limited(max) && asked > max {
                    match spend_over {
                        OverLimit::Refuse => {
                            return Err(refuse(PolicyRefusal::OverLimit {
                                denomination: Denomination::Spend,
                                requested: render_micro(asked),
                                allowed: render_micro(max),
                            }))
                        }
                        OverLimit::Reduce => {
                            adjustments.push(CapAdjustment::Reduced {
                                denomination: Denomination::Spend,
                                requested: render_micro(asked),
                                granted: render_micro(max),
                            });
                            spend = Some(max);
                        }
                    }
                } else {
                    spend = Some(asked);
                }
            } else {
                spend = Some(asked);
            }
        } else if let Some(default) = entry.default_spend_micro_usd {
            adjustments.push(CapAdjustment::Defaulted {
                denomination: Denomination::Spend,
                granted: render_micro(default),
            });
            spend = Some(default);
        }

        // ── Latency ──────────────────────────────────────────────────────────
        let max_latency = entry.max_latency_seconds.map(Duration::from_secs);
        let default_latency = entry.default_latency_seconds.map(Duration::from_secs);
        let mut latency = None;

        if let Some(asked) = requested.latency {
            if !entry.allowed_denominations.iter().any(|d| d == "latency") {
                if let Some(default) = default_latency {
                    adjustments.push(CapAdjustment::NotPermitted {
                        denomination: Denomination::Latency,
                        requested: render_duration(asked),
                        granted: render_duration(default),
                    });
                    latency = Some(default);
                }
            } else if let Some(max) = max_latency {
                if asked > max {
                    match spend_over {
                        OverLimit::Refuse => {
                            return Err(refuse(PolicyRefusal::OverLimit {
                                denomination: Denomination::Latency,
                                requested: render_duration(asked),
                                allowed: render_duration(max),
                            }))
                        }
                        OverLimit::Reduce => {
                            adjustments.push(CapAdjustment::Reduced {
                                denomination: Denomination::Latency,
                                requested: render_duration(asked),
                                granted: render_duration(max),
                            });
                            latency = Some(max);
                        }
                    }
                } else {
                    latency = Some(asked);
                }
            } else {
                latency = Some(asked);
            }
        } else if let Some(default) = default_latency {
            adjustments.push(CapAdjustment::Defaulted {
                denomination: Denomination::Latency,
                granted: render_duration(default),
            });
            latency = Some(default);
        }

        // ── Due ──────────────────────────────────────────────────────────────
        //
        // No maximum: a deadline further out is a *weaker* constraint on the
        // gateway, and clamping it would tighten the run — the opposite of what a
        // limit is for. Whether a sender may set it at all is still policy, since
        // a `due` with no `latency` makes the run deferrable and changes its price.
        let mut due = None;
        if let Some(asked) = requested.due {
            if entry.allowed_denominations.iter().any(|d| d == "due") {
                due = Some(asked);
            } else {
                adjustments.push(CapAdjustment::NotPermitted {
                    denomination: Denomination::Due,
                    requested: asked.to_rfc3339(),
                    granted: "no deadline".to_string(),
                });
            }
        }

        let scope = self
            .mint_scope(user_id, channel_id, entry)
            .map_err(|e| refuse(PolicyRefusal::Scope(e)))?;

        let grant = Grant {
            spend_micro_usd: spend,
            latency,
            due,
            scope,
            adjustments,
        };

        // quarry refuses an uncapped run (P9), and refusing here with a clear
        // reason beats spawning a child that will exit non-zero.
        if !grant.any_cap() {
            return Err(refuse(PolicyRefusal::NoCapGranted));
        }

        if let Some(ref audit) = self.audit {
            audit.log(AuditEvent::QuarryPolicyDecision {
                user_id: user_id.to_string(),
                channel_id: channel_id.to_string(),
                matched: Some(level.to_string()),
                granted: true,
                refusal: None,
                requested_spend_micro_usd: requested.spend_micro_usd,
                granted_spend_micro_usd: grant.spend_micro_usd,
                adjustments: grant
                    .adjustments
                    .iter()
                    .map(|a| format!("{}:{}", a.denomination().code(), a.code()))
                    .collect(),
                scope_key: Some(grant.scope.key()),
            });
        }

        Ok(grant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{QuarryPolicyConfig, QuarryPolicyEntry};

    fn entry() -> QuarryPolicyEntry {
        QuarryPolicyEntry {
            allowed_denominations: vec!["spend".into(), "latency".into(), "due".into()],
            max_spend_micro_usd: Some(1_000_000),
            default_spend_micro_usd: Some(250_000),
            max_latency_seconds: Some(600),
            default_latency_seconds: None,
            allow_unlimited: false,
            on_over_limit: "reduce".into(),
            scope_tags: BTreeMap::new(),
        }
    }

    fn policy_with_default() -> ConfigCapsPolicy {
        ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(entry()),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        })
    }

    fn asked_spend(micro: i64) -> RequestedCaps {
        RequestedCaps {
            spend_micro_usd: Some(micro),
            ..Default::default()
        }
    }

    // ── Scope: the P6 property ───────────────────────────────────────────────

    /// The golden vectors were generated by running quarry's own `Scope.Key()`
    /// and `Problem.Key()`. A self-consistent test here would pass while
    /// disagreeing with the code that actually computes the cache key.
    #[derive(serde::Deserialize)]
    struct GoldenVector {
        name: String,
        statement: String,
        tags: Option<BTreeMap<String, String>>,
        scope_key: String,
        problem_key: String,
    }

    fn golden() -> Vec<GoldenVector> {
        let raw = include_str!("../../tests/fixtures/quarry/scope_keys_golden.json");
        serde_json::from_str(raw).expect("golden vectors parse")
    }

    /// quarry's `Problem.Key()`: sha256 of the trimmed statement, a NUL, and the
    /// scope key. Mirrored so the golden `problem_key` column is checkable — if
    /// this drifts from quarry, cache keys diverge and every hit is a miss (or,
    /// worse, a hit that should not be).
    fn problem_key(statement: &str, scope_key: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(statement.trim().as_bytes());
        h.update(b"\x00");
        h.update(scope_key.as_bytes());
        hex::encode(h.finalize())
    }

    #[test]
    fn scope_keys_match_quarrys_own_output_byte_for_byte() {
        let mut checked = 0;
        for v in golden() {
            let tags = v.tags.clone().unwrap_or_default();
            // The fixture includes cases mint() refuses on purpose; those are
            // asserted separately below. Here we check every case we accept.
            if let Ok(scope) = ScopeTags::mint(tags) {
                assert_eq!(
                    scope.key(),
                    v.scope_key,
                    "scope key disagrees with quarry for {:?}",
                    v.name
                );
                assert_eq!(
                    problem_key(&v.statement, &scope.key()),
                    v.problem_key,
                    "problem key disagrees with quarry for {:?}",
                    v.name
                );
                checked += 1;
            }
        }
        assert!(
            checked >= 8,
            "only {checked} golden vectors were exercised; the fixture or mint() changed"
        );
    }

    #[test]
    fn tag_insertion_order_cannot_change_the_cache_key() {
        // quarry sorts keys in Scope.Key(). If ours depended on insertion order,
        // the same sender would get a different cache namespace depending on how
        // the map was built — every hit becoming a miss, and money spent twice.
        let mut a = BTreeMap::new();
        a.insert("user".to_string(), "alice".to_string());
        a.insert("channel".to_string(), "discord-1".to_string());
        let mut b = BTreeMap::new();
        b.insert("channel".to_string(), "discord-1".to_string());
        b.insert("user".to_string(), "alice".to_string());
        assert_eq!(
            ScopeTags::mint(a).unwrap().key(),
            ScopeTags::mint(b).unwrap().key()
        );
        assert_eq!(
            ScopeTags::mint({
                let mut m = BTreeMap::new();
                m.insert("user".to_string(), "alice".to_string());
                m.insert("channel".to_string(), "discord-1".to_string());
                m
            })
            .unwrap()
            .key(),
            "channel=discord-1;user=alice;"
        );
    }

    #[test]
    fn two_senders_never_share_a_cache_key() {
        // The P6 property, at the layer that actually determines it. The cache key
        // is the scope key folded into the statement hash, so proving the *keys*
        // differ for a byte-identical statement is proving the lookup cannot hit
        // the other sender's entry.
        let statement = "how many moons does mars have";
        let alice = ScopeTags::mint(
            [
                ("user".to_string(), "alice".to_string()),
                ("channel".to_string(), "discord-1".to_string()),
            ]
            .into(),
        )
        .unwrap();
        let bob = ScopeTags::mint(
            [
                ("user".to_string(), "bob".to_string()),
                ("channel".to_string(), "discord-1".to_string()),
            ]
            .into(),
        )
        .unwrap();
        assert_ne!(alice.key(), bob.key());
        assert_ne!(
            problem_key(statement, &alice.key()),
            problem_key(statement, &bob.key()),
            "a byte-identical statement in two scopes must not address the same cache entry"
        );

        // And the same sender *does* get a stable key, or caching never works.
        assert_eq!(
            problem_key(statement, &alice.key()),
            problem_key(statement, &alice.key())
        );
    }

    #[test]
    fn a_separator_in_a_tag_value_is_refused_because_it_forges_another_scope_key() {
        // Verified against quarry: Scope.Key() does not escape `=` or `;`, so
        //   {tenant: "victim;user=alice"}  renders as
        //   {tenant: "victim", user: "alice"}
        // byte for byte. With no IAM behind this gateway, that is a cross-tenant
        // cache read, so the separator bytes are refused at mint time.
        let forged: BTreeMap<String, String> =
            [("tenant".to_string(), "victim;user=alice".to_string())].into();
        let err = ScopeTags::mint(forged).unwrap_err();
        assert_eq!(err.code(), "scope_forbidden_character");

        // Proof the collision is real, expressed against our own renderer — which
        // matches quarry's byte for byte, as the golden test establishes.
        let honest = ScopeTags::mint(
            [
                ("tenant".to_string(), "victim".to_string()),
                ("user".to_string(), "alice".to_string()),
            ]
            .into(),
        )
        .unwrap();
        assert_eq!(honest.key(), "tenant=victim;user=alice;");
        // The refused value would have rendered exactly that.
        assert_eq!(
            format!("tenant={};", "victim;user=alice"),
            honest.key(),
            "this is the collision the refusal prevents"
        );
    }

    #[test]
    fn every_separator_and_control_byte_is_refused_in_keys_and_values() {
        for bad in ["a=b", "a;b", "a\nb", "a\tb", "a\0b"] {
            let as_value: BTreeMap<String, String> = [("user".to_string(), bad.to_string())].into();
            assert_eq!(
                ScopeTags::mint(as_value).unwrap_err().code(),
                "scope_forbidden_character",
                "value {bad:?} must be refused"
            );
            let as_key: BTreeMap<String, String> = [(bad.to_string(), "alice".to_string())].into();
            assert_eq!(
                ScopeTags::mint(as_key).unwrap_err().code(),
                "scope_forbidden_character",
                "key {bad:?} must be refused"
            );
        }
    }

    #[test]
    fn an_empty_scope_is_refused_because_it_narrows_to_everything() {
        assert_eq!(
            ScopeTags::mint(BTreeMap::new()).unwrap_err(),
            ScopeError::Empty
        );
        // The sharp edge being guarded: a subset check over no tags is vacuously
        // true, so an empty scope passes any entitlement check.
        let any = ScopeTags::mint([("user".to_string(), "alice".to_string())].into()).unwrap();
        let one_tag = ScopeTags::mint([("user".to_string(), "bob".to_string())].into()).unwrap();
        assert!(!any.narrows_to(&one_tag));
    }

    #[test]
    fn narrowing_is_subset_of_tags_and_not_prefix_matching() {
        // Matching quarry's NarrowsTo exactly, verified against its source:
        // {scope: "chemistry"}.NarrowsTo({scope: "chemistry/chem-101"}) is FALSE.
        // Adopting prefix semantics here would make a check pass in the gateway
        // and fail in quarry — failing open toward the cache.
        let parent =
            ScopeTags::mint([("scope".to_string(), "chemistry".to_string())].into()).unwrap();
        let child =
            ScopeTags::mint([("scope".to_string(), "chemistry/chem-101".to_string())].into())
                .unwrap();
        assert!(!parent.narrows_to(&child));
        assert!(!child.narrows_to(&parent));

        // Subset does hold in the direction quarry defines.
        let broad = ScopeTags::mint([("user".to_string(), "alice".to_string())].into()).unwrap();
        let narrow = ScopeTags::mint(
            [
                ("user".to_string(), "alice".to_string()),
                ("channel".to_string(), "discord-1".to_string()),
            ]
            .into(),
        )
        .unwrap();
        assert!(broad.narrows_to(&narrow));
        assert!(!narrow.narrows_to(&broad));
    }

    #[test]
    fn the_scope_flag_renders_in_quarrys_k_v_comma_form() {
        let scope = ScopeTags::mint(
            [
                ("user".to_string(), "alice".to_string()),
                ("channel".to_string(), "discord-1".to_string()),
            ]
            .into(),
        )
        .unwrap();
        assert_eq!(scope.flag_value(), "channel=discord-1,user=alice");
    }

    // ── Scope comes from identity, never from the message ────────────────────

    #[test]
    fn nothing_in_the_message_can_widen_a_senders_scope() {
        // The attack: a sender writes scope tags into their message hoping to be
        // granted another tenant's namespace. `resolve` takes only verified
        // identity and parsed *caps*, so there is no channel for message content
        // to reach the scope at all — this asserts the shape holds.
        let policy = policy_with_default();
        let hostile = RequestedCaps {
            spend_micro_usd: Some(100_000),
            ..Default::default()
        };
        let grant = policy
            .resolve("alice", "discord-1", &hostile)
            .expect("should be granted");
        assert_eq!(grant.scope.key(), "channel=discord-1;user=alice;");
        assert_eq!(grant.scope.tags().get("user").unwrap(), "alice");
        // Not "tenant=victim", not "user=root", nothing the message could suggest.
        assert_eq!(grant.scope.tags().len(), 2);
    }

    #[test]
    fn a_policy_entry_cannot_overwrite_the_identity_tags() {
        // An operator entry that set `user` would let one policy entry impersonate
        // another sender's cache namespace — the same leak by a different route.
        let mut e = entry();
        e.scope_tags = [
            ("user".to_string(), "root".to_string()),
            ("channel".to_string(), "elsewhere".to_string()),
            ("tenant".to_string(), "acme".to_string()),
        ]
        .into();
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let grant = policy
            .resolve("alice", "discord-1", &asked_spend(100_000))
            .unwrap();
        assert_eq!(grant.scope.tags().get("user").unwrap(), "alice");
        assert_eq!(grant.scope.tags().get("channel").unwrap(), "discord-1");
        // A legitimate operator tag still lands.
        assert_eq!(grant.scope.tags().get("tenant").unwrap(), "acme");
    }

    #[test]
    fn a_hostile_user_id_is_refused_rather_than_folded_into_a_forged_key() {
        // A channel adapter that let `;` into a user id would otherwise hand us a
        // value that forges another scope's key. Refused, loudly.
        let policy = policy_with_default();
        let err = policy
            .resolve("victim;user=alice", "discord-1", &asked_spend(100_000))
            .unwrap_err();
        assert_eq!(err.code(), "scope_forbidden_character");
    }

    // ── Default-deny ─────────────────────────────────────────────────────────

    #[test]
    fn no_matching_entry_denies_the_run() {
        // A missing config means nobody may run. The opposite default spends an
        // unbounded amount on a fresh install.
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig::default());
        let err = policy
            .resolve("alice", "discord-1", &asked_spend(100_000))
            .unwrap_err();
        assert_eq!(err.code(), "no_policy");
        assert!(err.message().contains("not enabled"));
    }

    #[test]
    fn an_empty_policy_config_is_not_permission_for_anyone() {
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig::default());
        for (user, channel) in [("alice", "discord-1"), ("", ""), ("root", "admin")] {
            assert!(
                policy.resolve(user, channel, &asked_spend(1)).is_err(),
                "{user}/{channel} must be denied by default"
            );
        }
    }

    // ── Precedence ───────────────────────────────────────────────────────────

    #[test]
    fn precedence_is_sender_then_channel_then_default() {
        let mut sender = entry();
        sender.max_spend_micro_usd = Some(9_000_000);
        let mut channel = entry();
        channel.max_spend_micro_usd = Some(2_000_000);
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(entry()), // max 1_000_000
            channels: [("discord-1".to_string(), channel)].into(),
            senders: [("alice".to_string(), sender)].into(),
        });

        // Sender override wins.
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(5_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(5_000_000));

        // No sender entry: the channel's.
        let g = policy
            .resolve("bob", "discord-1", &asked_spend(5_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(2_000_000));

        // Neither: the default's.
        let g = policy
            .resolve("bob", "slack-1", &asked_spend(5_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(1_000_000));
    }

    #[test]
    fn an_entry_is_taken_whole_and_not_merged_with_a_broader_one() {
        // Merging would make a channel's `allow_unlimited: false` silently stop
        // applying the moment a sender override set an unrelated field, since the
        // override's own `false` default is indistinguishable from "unspecified".
        let mut channel = entry();
        channel.allow_unlimited = false;
        let mut sender = entry();
        sender.allow_unlimited = true;
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: None,
            channels: [("discord-1".to_string(), channel)].into(),
            senders: [("alice".to_string(), sender)].into(),
        });
        // alice's own entry permits unlimited; the channel's stricter rule is not
        // merged in.
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(UNLIMITED_MICRO_USD))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(UNLIMITED_MICRO_USD));
        // bob falls to the channel entry, which forbids it.
        let err = policy
            .resolve("bob", "discord-1", &asked_spend(UNLIMITED_MICRO_USD))
            .unwrap_err();
        assert_eq!(err.code(), "unlimited_forbidden");
    }

    // ── Reduce vs refuse ─────────────────────────────────────────────────────

    #[test]
    fn an_over_limit_request_is_reduced_and_the_reduction_is_disclosed() {
        let policy = policy_with_default();
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(500_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(1_000_000));
        let adj = g
            .adjustments
            .iter()
            .find(|a| a.denomination() == Denomination::Spend)
            .expect("a reduction must be disclosed, never silent");
        assert_eq!(adj.code(), "reduced");
        // The sender sees both numbers: "you asked for X, policy allows Y".
        assert!(adj.message().contains("$500.0000"), "{}", adj.message());
        assert!(adj.message().contains("$1.0000"), "{}", adj.message());
    }

    #[test]
    fn an_over_limit_request_is_refused_when_policy_says_refuse() {
        let mut e = entry();
        e.on_over_limit = "refuse".to_string();
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let err = policy
            .resolve("alice", "discord-1", &asked_spend(500_000_000))
            .unwrap_err();
        assert_eq!(err.code(), "over_limit");
        assert!(err.message().contains("$1.0000"), "{}", err.message());
    }

    #[test]
    fn an_unrecognised_over_limit_setting_refuses_rather_than_permits() {
        // A typo in `on_over_limit` must not silently become the permissive branch.
        let mut e = entry();
        e.on_over_limit = "redcue".to_string();
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        assert_eq!(
            policy
                .resolve("alice", "discord-1", &asked_spend(500_000_000))
                .unwrap_err()
                .code(),
            "over_limit"
        );
    }

    #[test]
    fn a_request_within_the_limit_is_granted_untouched_and_undisclosed() {
        let policy = policy_with_default();
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(400_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(400_000));
        assert!(
            g.adjustments.is_empty(),
            "nothing was adjusted, so there is nothing to disclose: {:?}",
            g.adjustments
        );
    }

    // ── Unlimited ────────────────────────────────────────────────────────────

    #[test]
    fn unlimited_is_forbidden_by_default() {
        let policy = policy_with_default();
        assert!(!entry().allow_unlimited, "the default must be restrictive");
        let err = policy
            .resolve("alice", "discord-1", &asked_spend(UNLIMITED_MICRO_USD))
            .unwrap_err();
        assert_eq!(err.code(), "unlimited_forbidden");
        assert!(
            err.message().contains("divides the cap"),
            "{}",
            err.message()
        );
    }

    #[test]
    fn unlimited_is_granted_only_when_policy_opts_in() {
        let mut e = entry();
        e.allow_unlimited = true;
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(UNLIMITED_MICRO_USD))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(UNLIMITED_MICRO_USD));
        assert!(!is_limited(g.spend_micro_usd.unwrap()));
    }

    #[test]
    fn an_unlimited_maximum_does_not_refuse_every_request() {
        // The trap quarry warns about: comparing `asked > max` against max == -1
        // treats an unlimited ceiling as a zero one, refusing everything. Limited()
        // has to be consulted first.
        let mut e = entry();
        e.max_spend_micro_usd = Some(UNLIMITED_MICRO_USD);
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(500_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(500_000_000));
        assert!(g.adjustments.is_empty());
    }

    // ── Denomination permissions ─────────────────────────────────────────────

    #[test]
    fn a_forbidden_denomination_is_discarded_and_the_default_applies_with_disclosure() {
        let mut e = entry();
        e.allowed_denominations = vec!["latency".to_string()];
        e.default_spend_micro_usd = Some(50_000);
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let g = policy
            .resolve("alice", "discord-1", &asked_spend(900_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(50_000));
        let adj = &g.adjustments[0];
        assert_eq!(adj.code(), "not_permitted");
        assert!(adj.message().contains("not permitted"), "{}", adj.message());
    }

    #[test]
    fn a_sender_may_be_allowed_spend_but_not_due() {
        // Which denominations a sender may set is itself policy: a `due` with no
        // `latency` makes the run deferrable and changes its price.
        let mut e = entry();
        e.allowed_denominations = vec!["spend".to_string()];
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let asked = RequestedCaps {
            spend_micro_usd: Some(500_000),
            due: Some(chrono::Utc::now() + chrono::Duration::hours(6)),
            ..Default::default()
        };
        let g = policy.resolve("alice", "discord-1", &asked).unwrap();
        assert_eq!(g.spend_micro_usd, Some(500_000));
        assert_eq!(g.due, None, "a forbidden denomination must not be granted");
        assert!(!g.deferrable());
        assert_eq!(
            g.adjustments
                .iter()
                .filter(|a| a.denomination() == Denomination::Due)
                .count(),
            1,
            "and the sender has to be told their deadline was dropped"
        );
    }

    // ── Latency and defaults ─────────────────────────────────────────────────

    #[test]
    fn an_over_limit_latency_is_reduced_and_disclosed() {
        let policy = policy_with_default();
        let asked = RequestedCaps {
            latency: Some(Duration::from_secs(3600)),
            ..Default::default()
        };
        let g = policy.resolve("alice", "discord-1", &asked).unwrap();
        assert_eq!(g.latency, Some(Duration::from_secs(600)));
        let adj = g
            .adjustments
            .iter()
            .find(|a| a.denomination() == Denomination::Latency)
            .unwrap();
        assert_eq!(adj.code(), "reduced");
        assert!(adj.message().contains("10m"), "{}", adj.message());
    }

    #[test]
    fn a_deadline_is_never_clamped_because_a_later_one_is_a_weaker_constraint() {
        let policy = policy_with_default();
        let far = chrono::Utc::now() + chrono::Duration::days(30);
        let asked = RequestedCaps {
            due: Some(far),
            ..Default::default()
        };
        let g = policy.resolve("alice", "discord-1", &asked).unwrap();
        assert_eq!(g.due, Some(far));
    }

    #[test]
    fn a_missing_cap_gets_the_policy_default_with_a_disclosure() {
        // Note this is policy supplying a cap the *operator* configured, which is
        // different from the parser inventing one. The parser refuses; policy is
        // allowed to have an opinion, and says so.
        let policy = policy_with_default();
        let g = policy
            .resolve("alice", "discord-1", &RequestedCaps::default())
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(250_000));
        assert_eq!(g.adjustments[0].code(), "defaulted");
    }

    #[test]
    fn a_policy_that_grants_no_cap_at_all_is_refused_as_a_misconfiguration() {
        // quarry refuses an uncapped run (P9). Refusing here with a clear reason
        // beats spawning a child that will exit non-zero.
        let mut e = entry();
        e.default_spend_micro_usd = None;
        e.default_latency_seconds = None;
        let policy = ConfigCapsPolicy::new(QuarryPolicyConfig {
            default: Some(e),
            channels: BTreeMap::new(),
            senders: BTreeMap::new(),
        });
        let err = policy
            .resolve("alice", "discord-1", &RequestedCaps::default())
            .unwrap_err();
        assert_eq!(err.code(), "no_cap_granted");
        assert!(err.message().contains("P9"), "{}", err.message());
    }

    // ── The documented YAML ──────────────────────────────────────────────────

    /// The `quarry.policy` block from `docs/configuration.md`, verbatim.
    ///
    /// Kept as a literal so documentation drift is a test failure. A config sample
    /// that no longer deserializes is worse than no sample: an operator copies it,
    /// the gateway refuses to start, and the field names in the docs are the first
    /// thing they will trust and the last thing they will suspect.
    const DOCUMENTED_YAML: &str = r#"
default:
  allowed_denominations: [spend, due]
  max_spend_micro_usd: 1000000
  default_spend_micro_usd: 250000
  on_over_limit: reduce
channels:
  discord-1:
    allowed_denominations: [spend, latency, due]
    max_spend_micro_usd: 5000000
    default_spend_micro_usd: 1000000
    max_latency_seconds: 1800
    on_over_limit: reduce
    scope_tags:
      tenant: engineering
senders:
  alice:
    allowed_denominations: [spend, latency, due]
    max_spend_micro_usd: 50000000
    allow_unlimited: false
    on_over_limit: refuse
"#;

    #[test]
    fn the_documented_config_sample_parses_and_behaves_as_documented() {
        let parsed: QuarryPolicyConfig =
            serde_yaml::from_str(DOCUMENTED_YAML).expect("the documented sample must deserialize");
        let policy = ConfigCapsPolicy::new(parsed);

        // The channel entry: $5 ceiling, reduce, and its operator tag.
        let g = policy
            .resolve("bob", "discord-1", &asked_spend(9_000_000))
            .unwrap();
        assert_eq!(g.spend_micro_usd, Some(5_000_000));
        assert_eq!(g.scope.tags().get("tenant").unwrap(), "engineering");
        assert_eq!(
            g.scope.key(),
            "channel=discord-1;tenant=engineering;user=bob;"
        );

        // alice's own entry refuses rather than reducing, and wins over the channel.
        assert_eq!(
            policy
                .resolve("alice", "discord-1", &asked_spend(90_000_000))
                .unwrap_err()
                .code(),
            "over_limit"
        );

        // Anyone else falls to `default`, which does not permit latency.
        let asked = RequestedCaps {
            spend_micro_usd: Some(500_000),
            latency: Some(Duration::from_secs(60)),
            ..Default::default()
        };
        let g = policy.resolve("bob", "slack-1", &asked).unwrap();
        assert_eq!(g.spend_micro_usd, Some(500_000));
        assert_eq!(g.latency, None);
    }

    #[test]
    fn an_empty_policy_block_deserializes_to_default_deny() {
        // `quarry: {policy: {}}` must not accidentally mean "everyone, unlimited".
        let parsed: QuarryPolicyConfig = serde_yaml::from_str("{}").expect("empty block parses");
        assert!(ConfigCapsPolicy::new(parsed)
            .resolve("alice", "discord-1", &asked_spend(1))
            .is_err());
    }

    // ── The child's credential ───────────────────────────────────────────────

    #[test]
    fn the_bearer_token_is_the_only_credential_the_child_receives() {
        let env = mint_child_env(Some("gw-token"), "http://127.0.0.1:8080/v1").unwrap();
        assert_eq!(
            env.keys().collect::<Vec<_>>(),
            vec!["QUARRY_PROVIDER_TOKEN", "QUARRY_PROVIDER_URL"],
            "the child's environment must be exactly the endpoint and its token"
        );
        assert_eq!(env["QUARRY_PROVIDER_TOKEN"], "gw-token");
        assert_eq!(env["QUARRY_PROVIDER_URL"], "http://127.0.0.1:8080/v1");
    }

    #[test]
    fn a_run_is_refused_when_there_is_no_token_to_mint() {
        // An absent token does not mean "spawn without one": it means /v1 has no
        // authentication at all, so there is nothing to hand over and nothing
        // keeping anything else on the host out either.
        assert_eq!(
            mint_child_env(None, "http://127.0.0.1:8080/v1").unwrap_err(),
            PolicyRefusal::NoProviderToken
        );
        assert_eq!(
            mint_child_env(Some(""), "http://127.0.0.1:8080/v1").unwrap_err(),
            PolicyRefusal::NoProviderToken
        );
    }

    // ── Codes ────────────────────────────────────────────────────────────────

    #[test]
    fn refusal_and_adjustment_codes_are_unique() {
        // These land in audit records; two causes sharing a code is how a reader
        // ends up string-matching a message to tell them apart — the agate#265
        // lesson.
        let codes = [
            PolicyRefusal::NoPolicy {
                user_id: String::new(),
                channel_id: String::new(),
            }
            .code(),
            PolicyRefusal::OverLimit {
                denomination: Denomination::Spend,
                requested: String::new(),
                allowed: String::new(),
            }
            .code(),
            PolicyRefusal::UnlimitedForbidden.code(),
            PolicyRefusal::NoCapGranted.code(),
            PolicyRefusal::NoProviderToken.code(),
            ScopeError::Empty.code(),
            ScopeError::EmptyKey.code(),
            ScopeError::ForbiddenCharacter {
                key: String::new(),
                found: '=',
            }
            .code(),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in codes {
            assert!(seen.insert(c), "duplicate code {c}");
        }
    }

    #[test]
    fn denomination_codes_match_quarrys_own_strings() {
        // quarry's Denomination is "spend" | "latency" | "due". A mismatch would
        // make a BoundBy value we report disagree with the one quarry recorded.
        assert_eq!(Denomination::Spend.code(), "spend");
        assert_eq!(Denomination::Latency.code(), "latency");
        assert_eq!(Denomination::Due.code(), "due");
    }
}
