//! Signed-binary verification at the quarry spawn path.
//!
//! # What this module is, and what it is not
//!
//! quarry is a **signed artifact**: the binary is cosign-verified before spawn, and
//! its capability manifest declares exactly two things — network egress to the
//! localhost gateway endpoint, and one writable run-record directory. Nothing else.
//! No outbound internet, no filesystem beyond that directory, and none of this
//! gateway's provider credentials or channel tokens (which
//! [`crate::quarry::policy::mint_child_env`] and the supervisor's `env_clear()`
//! already enforce independently).
//!
//! This module is the **spawn-path wiring**. It does not implement cosign
//! verification: that is [`SignatureVerifier`], a trait with no implementation in
//! this repo yet. The mechanism belongs to the signed-skills work (issue #103), and
//! duplicating it here would produce two verifiers that eventually disagree.
//!
//! ## Which means, today, that verification refuses everything
//!
//! With no [`SignatureVerifier`] installed, [`SpawnGate`] returns
//! [`VerificationRefusal::MechanismUnavailable`] for every spawn. That is not a
//! stub, an oversight, or a state to be worked around — it is the fail-closed
//! contract behaving correctly. A gateway configured with
//! `quarry.verification.enabled: true` and no verifier runs **nothing**, and says
//! why. Development uses `enabled: false`, which is loud (see below).
//!
//! # Fail closed, on every path
//!
//! Every error refuses the spawn. There is no fall-through to an unverified run:
//! not for a missing signature, not for a cosign binary that isn't installed, not
//! for a transparency log that can't be reached, not for a manifest that won't
//! parse. This repo has precedent for getting that direction wrong in a security
//! control — the shell allowlist (#90) had to be rewritten because it *filtered*
//! where it should have *rejected* — and precedent for a green check meaning nothing
//! was verified at all. So every refusal path here has its own test, and the
//! negative tests assert **absence of effect**: no child process, no run record. An
//! error return with the child already running is the #90 failure mode wearing a
//! different hat.
//!
//! # Identity-constrained, not signature-present
//!
//! A signature that merely *exists* proves someone signed something. Verification
//! must be constrained to the expected issuer and subject identity
//! (`--certificate-identity-regexp` / `--certificate-oidc-issuer`), or an attacker
//! who can sign anything with any Sigstore identity passes the check. That is the
//! single most common way cosign is deployed and defeated, so
//! [`SpawnGate`] refuses with [`VerificationRefusal::IdentityNotConfigured`] rather
//! than calling a verifier with no identity to check against — an unconstrained
//! verification is worse than none, because it produces a reassuring log line.
//!
//! # Verify once, spawn many — but hash every time
//!
//! The supervisor spawns per run, and cosign-verifying on every spawn is slow and
//! pointless *if the binary cannot change between spawns*. But "cannot change" is an
//! assumption, not a fact. So the two costs are split by how expensive they are:
//!
//! - The **digest is recomputed on every spawn**. It is one file read, and it is
//!   what detects an upgrade or a swap.
//! - The **signature check is cached, keyed by that digest** — never by path, and
//!   never by time. A digest that has been verified stays verified; a digest that
//!   has not is verified now. An upgrade changes the digest and so re-verifies
//!   itself, with no expiry to tune and no window during which a new binary runs on
//!   an old binary's verification.
//!
//! ## The residual TOCTOU window, stated rather than papered over
//!
//! Between the final digest read and the kernel's `execve`, the file at that path
//! could be replaced. That window is **open**, and closing it properly needs
//! `fexecve` against the same file descriptor that was hashed — which
//! `tokio::process::Command` cannot express and whose portable form (`/proc/self/fd`)
//! does not exist on macOS. Rather than pretend, the window is narrowed from the
//! other end: [`SpawnGate`] refuses when the binary **or its containing directory**
//! is writable by this process, which is what an attacker would need to perform the
//! swap. A read-only binary in a read-only directory leaves nothing to race. See
//! `allow_writable_binary` for the escape hatch and what it costs.
//!
//! Kernel-level enforcement (seccomp, landlock, a container) is a separate and
//! better answer to the whole class, and is deliberately out of scope here.
//!
//! # Two audiences, two messages
//!
//! Verification failure is an **operator configuration problem, not a user
//! problem**. So the sender gets [`VerificationRefusal::sender_message`] — one
//! constant string, naming no path, digest, or identity — and the operator gets the
//! `Display` form, the audit entry, and a `warn!` naming exactly which check failed.
//! "Verification failed" alone sends an operator hunting; "the manifest declares a
//! capability outside quarry's contract: `network-egress`" does not.

use crate::audit::{AuditEvent, AuditLogger};
use crate::config::QuarryVerificationConfig;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tracing::{info, warn};

// ── The capability manifest ───────────────────────────────────────────────────

/// quarry's declared capabilities.
///
/// The manifest vocabulary here is deliberately narrow, and deliberately *not* a
/// new language: it must reconcile with the manifest/ABI spec (#100) that also has
/// to cover Wasm components (#104). A subprocess and a Wasm component should not
/// need two unrelated capability languages, so this defines the smallest thing that
/// expresses quarry's two capabilities and nothing more. When #100 lands, this is
/// the side that changes.
///
/// Parsed from JSON of the form:
///
/// ```json
/// {
///   "schema": "quarry-capability-manifest/1",
///   "capabilities": [
///     {"kind": "localhost-egress", "port": 8080},
///     {"kind": "writable-dir", "path": "quarry-runs"}
///   ]
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityManifest {
    /// Schema identifier. Checked, because a future incompatible manifest must be
    /// refused rather than half-understood.
    pub schema: String,
    /// Every capability the artifact declares.
    pub capabilities: Vec<RawCapability>,
}

/// The schema string this module understands.
pub const MANIFEST_SCHEMA: &str = "quarry-capability-manifest/1";

/// One declared capability, before it is classified.
///
/// Kept as a flat struct with a string `kind` rather than a tagged enum so that an
/// **unrecognised kind is data this module can refuse with a name**, instead of a
/// deserialization error that reports only "unknown variant". The operator needs to
/// be told which capability was rejected.
///
/// `deny_unknown_fields` matters more than it looks: without it, a capability could
/// carry an extra field that a future, more permissive reader would honour, and this
/// reader would silently approve a manifest it did not fully understand.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawCapability {
    /// `localhost-egress` or `writable-dir`. Anything else is refused.
    pub kind: String,
    /// TCP port, for `localhost-egress`.
    #[serde(default)]
    pub port: Option<u16>,
    /// Host, for `localhost-egress`. Absent means loopback; a non-loopback value is
    /// refused.
    #[serde(default)]
    pub host: Option<String>,
    /// Directory path, for `writable-dir`.
    #[serde(default)]
    pub path: Option<String>,
}

/// Host names that count as loopback.
const LOOPBACK_HOSTS: &[&str] = &["127.0.0.1", "localhost", "::1", "[::1]"];

/// The verification config a test or local development build uses.
///
/// Signatures off (there is no signed fake binary to produce) and the writable-path
/// check waived (a temp dir is writable by definition). The **manifest check stays
/// on**, so a fake spawned through this config still has to declare exactly quarry's
/// two capabilities — which is the point of running tests through the real gate
/// rather than around it.
///
/// Not `#[cfg(test)]`: an operator running a locally-built quarry needs the same
/// settings, and a constructor they can read is better documentation than a config
/// snippet they have to retype.
pub fn development_config() -> QuarryVerificationConfig {
    QuarryVerificationConfig {
        enabled: false,
        allow_writable_binary: true,
        ..QuarryVerificationConfig::default()
    }
}

/// Render the manifest a development build needs beside its binary.
///
/// For `quarry.verification.enabled: false`, which still checks capabilities and so
/// still needs a manifest to read. Provided as a function because the alternative is
/// every operator and every test hand-writing the schema string, and a typo there
/// presents as "the manifest is unparseable" rather than as a typo.
///
/// **Not for production.** A manifest this produces is unsigned, and the verified
/// path deliberately refuses to read one.
pub fn development_manifest_json(gateway_port: u16, run_record_dir: &str) -> String {
    serde_json::json!({
        "schema": MANIFEST_SCHEMA,
        "capabilities": [
            {"kind": "localhost-egress", "port": gateway_port, "host": "127.0.0.1"},
            {"kind": "writable-dir", "path": run_record_dir},
        ],
    })
    .to_string()
}

/// What the manifest is required to say, and nothing more.
///
/// `gateway_port` is an [`Option`] on purpose, and `None` **refuses every manifest
/// that declares egress**. A supervisor built without being told the gateway's port
/// cannot check the port a manifest names, and the fail-closed reading of "cannot
/// check" is "refuse" — so forgetting to wire the port in is caught by the first
/// test that spawns, rather than quietly accepting any port at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedCapabilities {
    /// The localhost port quarry may reach: this gateway's own HTTP port.
    pub gateway_port: Option<u16>,
    /// The one directory quarry may write to.
    pub run_record_dir: String,
}

impl CapabilityManifest {
    /// Check the declared capabilities against what quarry is allowed.
    ///
    /// Refuses a manifest declaring **anything more** than localhost egress to the
    /// gateway plus the one writable run-record directory — including a second copy
    /// of either, which would otherwise let a manifest name two writable
    /// directories and pass a check that only looked for one.
    pub fn check(&self, expected: &ExpectedCapabilities) -> Result<(), ManifestFault> {
        if self.schema != MANIFEST_SCHEMA {
            return Err(ManifestFault::UnknownSchema {
                found: self.schema.clone(),
            });
        }

        let mut egress = None;
        let mut writable = None;
        for cap in &self.capabilities {
            match cap.kind.as_str() {
                "localhost-egress" => {
                    if egress.is_some() {
                        return Err(ManifestFault::Duplicate {
                            kind: cap.kind.clone(),
                        });
                    }
                    egress = Some(cap);
                }
                "writable-dir" => {
                    if writable.is_some() {
                        return Err(ManifestFault::Duplicate {
                            kind: cap.kind.clone(),
                        });
                    }
                    writable = Some(cap);
                }
                other => {
                    return Err(ManifestFault::Overbroad {
                        capability: other.to_string(),
                    })
                }
            }
        }

        let egress = egress.ok_or(ManifestFault::Missing {
            kind: "localhost-egress".to_string(),
        })?;
        let writable = writable.ok_or(ManifestFault::Missing {
            kind: "writable-dir".to_string(),
        })?;

        if let Some(host) = &egress.host {
            if !LOOPBACK_HOSTS.contains(&host.as_str()) {
                return Err(ManifestFault::NonLoopbackHost { host: host.clone() });
            }
        }
        let Some(port) = egress.port else {
            return Err(ManifestFault::EgressPortMissing);
        };
        // `None` refuses rather than accepts: see `ExpectedCapabilities`.
        match expected.gateway_port {
            None => return Err(ManifestFault::GatewayPortUnknown),
            Some(want) if want != port => {
                return Err(ManifestFault::WrongEgressPort {
                    declared: port,
                    expected: want,
                })
            }
            Some(_) => {}
        }

        let Some(declared_dir) = &writable.path else {
            return Err(ManifestFault::WritableDirMissing);
        };
        if !same_dir(declared_dir, &expected.run_record_dir) {
            return Err(ManifestFault::WrongWritableDir {
                declared: declared_dir.clone(),
                expected: expected.run_record_dir.clone(),
            });
        }
        Ok(())
    }
}

/// Whether two path strings name the same directory.
///
/// Canonicalizes when both paths exist, because `quarry-runs` and
/// `/srv/rustynail/quarry-runs` can be the same directory and a string comparison
/// would call them different. Falls back to comparing normalized components when
/// either does not exist yet — the run-record directory is created lazily, so a
/// manifest checked before the first run must still match.
///
/// The fallback is a *stricter* comparison, not a looser one: it can reject a pair
/// that canonicalization would have accepted, which fails closed.
fn same_dir(a: &str, b: &str) -> bool {
    let (pa, pb) = (
        Path::new(a.trim_end_matches('/')),
        Path::new(b.trim_end_matches('/')),
    );
    if let (Ok(ca), Ok(cb)) = (pa.canonicalize(), pb.canonicalize()) {
        return ca == cb;
    }
    pa.components().eq(pb.components())
}

/// Why a manifest was rejected.
///
/// Separate from [`VerificationRefusal`] so the manifest check is testable without
/// standing up a gate, and so the operator message can name the specific clause the
/// manifest broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestFault {
    /// The `schema` field names something this reader does not understand.
    UnknownSchema { found: String },
    /// A capability outside quarry's contract.
    Overbroad { capability: String },
    /// A required capability is absent.
    Missing { kind: String },
    /// The same capability kind twice.
    Duplicate { kind: String },
    /// `localhost-egress` naming a host that is not loopback.
    NonLoopbackHost { host: String },
    /// `localhost-egress` with no port.
    EgressPortMissing,
    /// `localhost-egress` to a port that is not the gateway's.
    WrongEgressPort { declared: u16, expected: u16 },
    /// The supervisor was never told which port to expect.
    GatewayPortUnknown,
    /// `writable-dir` with no path.
    WritableDirMissing,
    /// `writable-dir` naming a directory that is not the run-record directory.
    WrongWritableDir { declared: String, expected: String },
}

impl std::fmt::Display for ManifestFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSchema { found } => write!(
                f,
                "manifest schema '{found}' is not understood (expected '{MANIFEST_SCHEMA}')"
            ),
            Self::Overbroad { capability } => write!(
                f,
                "manifest declares '{capability}', which is outside quarry's contract of \
                 localhost egress to the gateway plus one writable run-record directory"
            ),
            Self::Missing { kind } => {
                write!(
                    f,
                    "manifest does not declare the required '{kind}' capability"
                )
            }
            Self::Duplicate { kind } => write!(
                f,
                "manifest declares '{kind}' more than once; quarry's contract allows exactly one"
            ),
            Self::NonLoopbackHost { host } => write!(
                f,
                "manifest declares egress to '{host}', which is not loopback; quarry may reach \
                 only this gateway"
            ),
            Self::EgressPortMissing => {
                write!(f, "manifest declares localhost egress with no port")
            }
            Self::WrongEgressPort { declared, expected } => write!(
                f,
                "manifest declares egress to port {declared}, but this gateway listens on \
                 {expected}"
            ),
            Self::GatewayPortUnknown => write!(
                f,
                "the supervisor was not told this gateway's HTTP port, so the manifest's \
                 declared egress port cannot be checked; refused rather than accepted"
            ),
            Self::WritableDirMissing => {
                write!(f, "manifest declares a writable directory with no path")
            }
            Self::WrongWritableDir { declared, expected } => write!(
                f,
                "manifest declares '{declared}' writable, but quarry's run-record directory is \
                 '{expected}'"
            ),
        }
    }
}

impl ManifestFault {
    /// A stable machine-readable slug.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownSchema { .. } => "manifest_unknown_schema",
            Self::Overbroad { .. } => "manifest_overbroad",
            Self::Missing { .. } => "manifest_missing_capability",
            Self::Duplicate { .. } => "manifest_duplicate_capability",
            Self::NonLoopbackHost { .. } => "manifest_non_loopback_host",
            Self::EgressPortMissing => "manifest_egress_port_missing",
            Self::WrongEgressPort { .. } => "manifest_wrong_egress_port",
            Self::GatewayPortUnknown => "manifest_gateway_port_unknown",
            Self::WritableDirMissing => "manifest_writable_dir_missing",
            Self::WrongWritableDir { .. } => "manifest_wrong_writable_dir",
        }
    }
}

// ── The verifier boundary ─────────────────────────────────────────────────────

/// What a signature verifier is asked to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRequest {
    /// Path to the artifact.
    pub path: PathBuf,
    /// Its sha256, lowercase hex. Already computed, so a verifier does not read the
    /// file a second time and risk verifying different bytes than the caller hashed.
    pub digest: String,
    /// Expected subject identity (`--certificate-identity-regexp`). Never empty —
    /// [`SpawnGate`] refuses before building a request without one.
    pub expected_identity: String,
    /// Expected OIDC issuer (`--certificate-oidc-issuer`). Never empty.
    pub expected_issuer: String,
    /// Path to the cosign binary, or however the mechanism is invoked.
    pub cosign_path: String,
}

/// What a successful verification yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedMaterial {
    /// The digest that was verified. Compared against the digest that will be
    /// spawned, so a verifier that answered about a different artifact is caught
    /// rather than trusted.
    pub digest: String,
    /// The capability manifest, **recovered from the signed material** — an
    /// attestation payload or a signed blob — never read from a sidecar file beside
    /// the binary. An unsigned manifest is a manifest an attacker writes.
    pub manifest_json: String,
}

/// Identity-constrained artifact verification.
///
/// # No implementation ships in this repo
///
/// The mechanism belongs to the signed-skills work (#103): cosign invocation,
/// certificate identity and issuer constraints, transparency-log policy, and the
/// Cedar gate. This trait is the seam that work plugs into, so that the spawn path
/// and the verifier can be built and tested independently — and so that there is
/// exactly one verifier rather than one per consumer.
///
/// Until it is filled, [`SpawnGate`] has no verifier and refuses every spawn with
/// [`VerificationRefusal::MechanismUnavailable`]. Implementors must **fail closed**:
/// a missing cosign binary, an unreachable transparency log, and an unparseable
/// certificate are all refusals, never `Ok`.
pub trait SignatureVerifier: Send + Sync {
    /// Verify `req`, returning the signed material or the reason it was refused.
    fn verify(&self, req: &VerifyRequest) -> Result<SignedMaterial, VerificationRefusal>;
}

// ── Refusals ──────────────────────────────────────────────────────────────────

/// Why a quarry spawn was refused before it started.
///
/// Every variant refuses. There is no variant that means "proceed with a warning" —
/// the two things that do warn and proceed (`enabled: false` and
/// `allow_writable_binary: true`) are explicit operator choices in config, not
/// outcomes of a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerificationRefusal {
    /// The binary does not exist, or is not a file.
    BinaryMissing { path: String, detail: String },
    /// The binary exists but could not be read to compute its digest.
    DigestUnreadable { path: String, detail: String },
    /// The binary is writable by this process, and `allow_writable_binary` is off.
    WritableBinary { path: String },
    /// The binary's directory is writable by this process, so the binary can be
    /// replaced by rename even if the file itself is read-only.
    WritableBinaryDir { path: String },
    /// `verification.enabled` is true but no [`SignatureVerifier`] is installed.
    ///
    /// **The state this repo ships in.** See the module docs: the mechanism is #103.
    MechanismUnavailable { digest: String },
    /// `verification.enabled` is true but no expected identity or issuer is set.
    ///
    /// Refused rather than verified unconstrained: a signature from *any* Sigstore
    /// identity would otherwise pass, and would log as a success.
    IdentityNotConfigured { missing: String },
    /// No signature was found for the artifact.
    Unsigned { digest: String },
    /// A valid signature, from an identity that is not the expected one.
    WrongIdentity {
        digest: String,
        expected: String,
        found: String,
    },
    /// A valid signature, from an issuer that is not the expected one.
    WrongIssuer {
        digest: String,
        expected: String,
        found: String,
    },
    /// The transparency log could not be reached, so inclusion could not be proven.
    TransparencyLogUnreachable { digest: String, detail: String },
    /// The verification mechanism itself could not be run.
    CosignUnavailable { path: String, detail: String },
    /// The signed material carried no manifest.
    ManifestMissing { digest: String },
    /// The manifest could not be parsed.
    ManifestUnparseable { digest: String, detail: String },
    /// The manifest parsed but declares the wrong capabilities.
    ManifestRejected {
        digest: String,
        fault: ManifestFault,
    },
    /// The verifier answered about a different artifact than the one being spawned.
    DigestMismatch { verified: String, spawning: String },
}

/// The one thing a sender is ever told.
///
/// A single constant for every variant, because the branch that decides *which*
/// detail is safe to reveal is the branch that eventually reveals a path or an
/// identity regex. Verification failure is an operator problem; the sender's only
/// actionable fact is that the capability is unavailable.
const SENDER_MESSAGE: &str =
    "That capability is unavailable right now. This is a configuration problem on \
     my side, not something wrong with your request — the operator has been notified.";

impl VerificationRefusal {
    /// A stable machine-readable slug, one per check.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BinaryMissing { .. } => "binary_missing",
            Self::DigestUnreadable { .. } => "digest_unreadable",
            Self::WritableBinary { .. } => "writable_binary",
            Self::WritableBinaryDir { .. } => "writable_binary_dir",
            Self::MechanismUnavailable { .. } => "mechanism_unavailable",
            Self::IdentityNotConfigured { .. } => "identity_not_configured",
            Self::Unsigned { .. } => "unsigned",
            Self::WrongIdentity { .. } => "wrong_identity",
            Self::WrongIssuer { .. } => "wrong_issuer",
            Self::TransparencyLogUnreachable { .. } => "transparency_log_unreachable",
            Self::CosignUnavailable { .. } => "cosign_unavailable",
            Self::ManifestMissing { .. } => "manifest_missing",
            Self::ManifestUnparseable { .. } => "manifest_unparseable",
            Self::ManifestRejected { .. } => "manifest_rejected",
            Self::DigestMismatch { .. } => "digest_mismatch",
        }
    }

    /// The digest involved, when one had been computed.
    ///
    /// Absent for the checks that run *before* hashing — a missing binary has no
    /// digest, and reporting an empty string as one would put a lie in the audit log.
    pub fn digest(&self) -> Option<&str> {
        match self {
            Self::MechanismUnavailable { digest }
            | Self::Unsigned { digest }
            | Self::WrongIdentity { digest, .. }
            | Self::WrongIssuer { digest, .. }
            | Self::TransparencyLogUnreachable { digest, .. }
            | Self::ManifestMissing { digest }
            | Self::ManifestUnparseable { digest, .. }
            | Self::ManifestRejected { digest, .. } => Some(digest),
            Self::DigestMismatch { spawning, .. } => Some(spawning),
            Self::BinaryMissing { .. }
            | Self::DigestUnreadable { .. }
            | Self::WritableBinary { .. }
            | Self::WritableBinaryDir { .. }
            | Self::IdentityNotConfigured { .. }
            | Self::CosignUnavailable { .. } => None,
        }
    }

    /// What the sender sees. Names no path, digest, identity, or config key.
    pub fn sender_message(&self) -> &'static str {
        SENDER_MESSAGE
    }
}

impl std::fmt::Display for VerificationRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryMissing { path, detail } => {
                write!(f, "quarry binary not found at '{path}': {detail}")
            }
            Self::DigestUnreadable { path, detail } => write!(
                f,
                "cannot read '{path}' to compute its digest, so it cannot be verified: {detail}"
            ),
            Self::WritableBinary { path } => write!(
                f,
                "the quarry binary at '{path}' is writable by this gateway's own process, so a \
                 verified digest proves nothing about what will execute; make it read-only, or \
                 set quarry.verification.allow_writable_binary to accept the risk explicitly"
            ),
            Self::WritableBinaryDir { path } => write!(
                f,
                "the directory '{path}' holding the quarry binary is writable by this gateway's \
                 own process, so the binary can be replaced by rename even while read-only; make \
                 it read-only, or set quarry.verification.allow_writable_binary to accept the \
                 risk explicitly"
            ),
            Self::MechanismUnavailable { digest } => write!(
                f,
                "quarry.verification.enabled is true but no signature verifier is installed, so \
                 the binary with digest {digest} cannot be verified and will not be run. The \
                 verification mechanism is the signed-skills work (issue #103) and is not yet \
                 implemented in this build; until it is, set quarry.verification.enabled to \
                 false for development, understanding that runs are then unverified"
            ),
            Self::IdentityNotConfigured { missing } => write!(
                f,
                "quarry.verification.enabled is true but {missing} is not set; an unconstrained \
                 verification would accept a signature from any Sigstore identity, which is \
                 worse than none because it succeeds"
            ),
            Self::Unsigned { digest } => write!(
                f,
                "no signature found for the quarry binary with digest {digest}"
            ),
            Self::WrongIdentity {
                digest,
                expected,
                found,
            } => write!(
                f,
                "the quarry binary with digest {digest} is validly signed, but by '{found}' \
                 rather than the expected identity '{expected}'; a signature that merely exists \
                 proves only that someone signed something"
            ),
            Self::WrongIssuer {
                digest,
                expected,
                found,
            } => write!(
                f,
                "the quarry binary with digest {digest} is validly signed, but its certificate \
                 was issued by '{found}' rather than the expected issuer '{expected}'"
            ),
            Self::TransparencyLogUnreachable { digest, detail } => write!(
                f,
                "cannot reach the transparency log to prove inclusion for digest {digest}, so \
                 the signature is unproven and the run is refused: {detail}"
            ),
            Self::CosignUnavailable { path, detail } => write!(
                f,
                "cannot run the verification mechanism at '{path}': {detail}"
            ),
            Self::ManifestMissing { digest } => write!(
                f,
                "the signed material for digest {digest} carries no capability manifest, so what \
                 the artifact is permitted to do is unknown"
            ),
            Self::ManifestUnparseable { digest, detail } => write!(
                f,
                "the capability manifest for digest {digest} could not be parsed: {detail}"
            ),
            Self::ManifestRejected { digest, fault } => {
                write!(
                    f,
                    "capability manifest for digest {digest} refused: {fault}"
                )
            }
            Self::DigestMismatch { verified, spawning } => write!(
                f,
                "the verifier reported on digest {verified} but the binary about to be spawned \
                 hashes to {spawning}; refusing rather than trusting a verification of different \
                 bytes"
            ),
        }
    }
}

impl std::error::Error for VerificationRefusal {}

// ── The gate ──────────────────────────────────────────────────────────────────

/// What passed verification, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The sha256 that was checked and is about to be spawned.
    pub digest: String,
    /// Whether a signature was actually checked.
    ///
    /// `false` in development mode (`verification.enabled: false`). Recorded rather
    /// than assumed, because an audit log that cannot tell a verified run from a
    /// development-mode run cannot answer the one question it exists for.
    pub signature_checked: bool,
}

/// Gates the spawn path on verification.
///
/// Holds the set of digests whose signatures have been checked. Keyed by digest and
/// never expired: see the module docs for why time-based re-verification is the
/// wrong axis.
pub struct SpawnGate {
    config: QuarryVerificationConfig,
    expected: ExpectedCapabilities,
    verifier: Option<Arc<dyn SignatureVerifier>>,
    /// Digests whose signature check has passed, and the manifest each was signed
    /// with.
    ///
    /// **The signature is cached; the manifest check is not.** Storing the manifest
    /// rather than a bare "this digest is fine" means the capability check re-runs on
    /// every spawn against the *current* expectations, which is what stops a
    /// reconfigured gateway port or run-record directory from being waved through by
    /// a cache entry that predates it.
    ///
    /// A `std::sync::Mutex` rather than an async one: the critical section is a map
    /// lookup with no await inside it, and holding an async lock across the verifier
    /// call would serialise every spawn behind one cosign invocation.
    verified: Mutex<HashMap<String, String>>,
    audit: Option<Arc<AuditLogger>>,
}

impl SpawnGate {
    /// Build a gate.
    ///
    /// `gateway_port` is `None` when the caller does not know this gateway's HTTP
    /// port, which **refuses every manifest** — see [`ExpectedCapabilities`].
    pub fn new(
        config: QuarryVerificationConfig,
        run_record_dir: String,
        gateway_port: Option<u16>,
    ) -> Self {
        Self {
            config,
            expected: ExpectedCapabilities {
                gateway_port,
                run_record_dir,
            },
            verifier: None,
            verified: Mutex::new(HashMap::new()),
            audit: None,
        }
    }

    /// Replace the expected localhost egress port.
    ///
    /// A setter rather than a rebuild so that callers can configure the gate in any
    /// order without one setter silently discarding another's work — the shape that
    /// loses a verifier because `with_audit` was called after it.
    pub fn set_gateway_port(&mut self, port: Option<u16>) {
        self.expected.gateway_port = port;
    }

    /// Install the signature verifier (#103's mechanism).
    pub fn with_verifier(mut self, verifier: Arc<dyn SignatureVerifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Install or clear the signature verifier.
    pub fn set_verifier(&mut self, verifier: Option<Arc<dyn SignatureVerifier>>) {
        self.verifier = verifier;
    }

    /// Attach an audit logger.
    pub fn with_audit(mut self, audit: Option<Arc<AuditLogger>>) -> Self {
        self.audit = audit;
        self
    }

    /// Attach or clear the audit logger.
    pub fn set_audit(&mut self, audit: Option<Arc<AuditLogger>>) {
        self.audit = audit;
    }

    /// Whether signature verification is switched on.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Whether a verifier is installed. `false` in every build that ships today.
    pub fn has_verifier(&self) -> bool {
        self.verifier.is_some()
    }

    /// Check the binary at `binary_path`, returning what may be spawned.
    ///
    /// Every error path refuses, is logged with the failing check named, and is
    /// audited. The caller must not spawn on `Err` — and must not have created a run
    /// directory before calling, so that a refusal leaves no artifact behind.
    pub async fn check(&self, binary_path: &str) -> Result<Verified, VerificationRefusal> {
        match self.check_inner(binary_path).await {
            Ok(v) => {
                if !v.signature_checked {
                    // On **every** spawn, not once at startup. A long-running
                    // gateway that warned only at boot is a gateway nobody knows is
                    // unverified — the warning has to be present in the logs that an
                    // operator is actually reading, which are the recent ones.
                    warn!(
                        digest = %v.digest,
                        "quarry signature verification is DISABLED \
                         (quarry.verification.enabled = false): this run is unverified. The \
                         capability manifest was still checked, but its source is an unsigned \
                         file beside the binary and proves nothing about provenance."
                    );
                }
                if let Some(audit) = &self.audit {
                    audit.log(AuditEvent::QuarryVerificationPassed {
                        binary_path: binary_path.to_string(),
                        digest: v.digest.clone(),
                        signature_checked: v.signature_checked,
                    });
                }
                Ok(v)
            }
            Err(refusal) => {
                warn!(
                    check = refusal.code(),
                    "refusing to spawn quarry: {refusal}"
                );
                if let Some(audit) = &self.audit {
                    audit.log(AuditEvent::QuarryVerificationRefused {
                        reason: refusal.code().to_string(),
                        detail: refusal.to_string(),
                        binary_path: binary_path.to_string(),
                        digest: refusal.digest().map(str::to_string),
                        expected_identity: non_empty(&self.config.expected_identity),
                        expected_issuer: non_empty(&self.config.expected_issuer),
                    });
                }
                Err(refusal)
            }
        }
    }

    async fn check_inner(&self, binary_path: &str) -> Result<Verified, VerificationRefusal> {
        let path = resolve_binary(binary_path)?;

        // Writability first, and before hashing: if the binary can be swapped, the
        // digest we are about to compute describes bytes that need not be the bytes
        // that execute, so there is no point spending the read.
        self.check_writability(&path)?;

        let digest = digest_of(&path).await?;

        if !self.config.enabled {
            // Development mode: skip the signature, keep the manifest check.
            //
            // Provenance and sandboxing are separable, and only the first is a
            // development convenience. So the manifest is still parsed and still
            // checked — from an unsigned sidecar, which is exactly the thing the
            // verified path refuses to trust. That asymmetry is the point, and the
            // per-spawn warning in `check` says so.
            let manifest = self.read_sidecar_manifest(&path, &digest)?;
            manifest.check(&self.expected).map_err(|fault| {
                VerificationRefusal::ManifestRejected {
                    digest: digest.clone(),
                    fault,
                }
            })?;
            return Ok(Verified {
                digest,
                signature_checked: false,
            });
        }

        // Identity is checked before the verifier is called, not by it: a verifier
        // handed an empty identity regex might match anything and report success.
        let mut missing = Vec::new();
        if self.config.expected_identity.trim().is_empty() {
            missing.push("quarry.verification.expected_identity");
        }
        if self.config.expected_issuer.trim().is_empty() {
            missing.push("quarry.verification.expected_issuer");
        }
        if !missing.is_empty() {
            return Err(VerificationRefusal::IdentityNotConfigured {
                missing: missing.join(" and "),
            });
        }

        // The signature check is what is cached, keyed by digest. The digest itself
        // was recomputed above, so an upgraded binary misses here and re-verifies.
        //
        // What is *not* cached is the manifest verdict. The signed manifest text is
        // stored and re-checked below on every spawn, so that a gateway whose port or
        // run-record directory changed under it re-evaluates rather than trusting a
        // decision made against the old configuration.
        let cached = self
            .verified
            .lock()
            .expect("verified digests")
            .get(&digest)
            .cloned();

        let manifest_json = match cached {
            Some(json) => json,
            None => {
                let Some(verifier) = &self.verifier else {
                    // Not a stub: the fail-closed contract, working. See module docs.
                    return Err(VerificationRefusal::MechanismUnavailable { digest });
                };
                let material = verifier.verify(&VerifyRequest {
                    path: path.clone(),
                    digest: digest.clone(),
                    expected_identity: self.config.expected_identity.clone(),
                    expected_issuer: self.config.expected_issuer.clone(),
                    cosign_path: self.config.cosign_path.clone(),
                })?;

                // A verifier that answered about other bytes is a verifier whose
                // answer does not apply. Checked rather than assumed: silent failure.
                if material.digest != digest {
                    return Err(VerificationRefusal::DigestMismatch {
                        verified: material.digest,
                        spawning: digest,
                    });
                }
                if material.manifest_json.trim().is_empty() {
                    return Err(VerificationRefusal::ManifestMissing { digest });
                }
                // Cached after the signature and digest checks pass but *before* the
                // manifest is evaluated, so the expensive part is not repeated while
                // the cheap policy check still is.
                self.verified
                    .lock()
                    .expect("verified digests")
                    .insert(digest.clone(), material.manifest_json.clone());
                info!(
                    digest = %digest,
                    identity = %self.config.expected_identity,
                    "quarry binary verified"
                );
                material.manifest_json
            }
        };

        let manifest: CapabilityManifest = serde_json::from_str(&manifest_json).map_err(|e| {
            VerificationRefusal::ManifestUnparseable {
                digest: digest.clone(),
                detail: e.to_string(),
            }
        })?;
        manifest
            .check(&self.expected)
            .map_err(|fault| VerificationRefusal::ManifestRejected {
                digest: digest.clone(),
                fault,
            })?;

        Ok(Verified {
            digest,
            signature_checked: true,
        })
    }

    /// Refuse — or, if the operator opted in, warn — when the binary can be replaced.
    fn check_writability(&self, path: &Path) -> Result<(), VerificationRefusal> {
        let dir_writable = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(is_writable_by_us)
            .unwrap_or(false);
        let file_writable = is_writable_by_us(path);

        if !file_writable && !dir_writable {
            return Ok(());
        }
        let refusal = if file_writable {
            VerificationRefusal::WritableBinary {
                path: path.display().to_string(),
            }
        } else {
            VerificationRefusal::WritableBinaryDir {
                path: path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            }
        };
        if self.config.allow_writable_binary {
            // Same discipline as `enabled: false`: an accepted risk is re-stated on
            // every spawn, because the operator who accepted it is often not the one
            // reading the logs a year later.
            warn!(
                check = refusal.code(),
                "quarry.verification.allow_writable_binary is set, so proceeding anyway: {refusal}"
            );
            return Ok(());
        }
        Err(refusal)
    }

    /// Read the development-mode manifest sidecar.
    ///
    /// Only reachable when `enabled` is false. The verified path never consults a
    /// sidecar — the manifest must come from the signed material — and keeping the
    /// two reads in separate functions is what stops that from becoming a fallback.
    fn read_sidecar_manifest(
        &self,
        binary: &Path,
        digest: &str,
    ) -> Result<CapabilityManifest, VerificationRefusal> {
        let sidecar = if self.config.manifest_path.trim().is_empty() {
            let mut p = binary.as_os_str().to_os_string();
            p.push(".manifest.json");
            PathBuf::from(p)
        } else {
            PathBuf::from(&self.config.manifest_path)
        };
        let text = std::fs::read_to_string(&sidecar).map_err(|e| {
            VerificationRefusal::ManifestUnparseable {
                digest: digest.to_string(),
                detail: format!("cannot read '{}': {e}", sidecar.display()),
            }
        })?;
        serde_json::from_str(&text).map_err(|e| VerificationRefusal::ManifestUnparseable {
            digest: digest.to_string(),
            detail: format!("in '{}': {e}", sidecar.display()),
        })
    }
}

fn non_empty(s: &str) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Resolve a binary name or path to a concrete file.
///
/// A bare name like `quarry` is looked up on `PATH`, because that is what
/// `QuarryConfig::binary_path` defaults to and what `Command` would itself do — and
/// verification has to hash the *same* file the kernel will execute, not a
/// different one found by a different rule.
fn resolve_binary(binary_path: &str) -> Result<PathBuf, VerificationRefusal> {
    let candidate = Path::new(binary_path);
    let resolved = if candidate.components().count() > 1 || candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        match which_on_path(binary_path) {
            Some(p) => p,
            None => {
                return Err(VerificationRefusal::BinaryMissing {
                    path: binary_path.to_string(),
                    detail: "not found on PATH".to_string(),
                })
            }
        }
    };
    let meta = std::fs::metadata(&resolved).map_err(|e| VerificationRefusal::BinaryMissing {
        path: resolved.display().to_string(),
        detail: e.to_string(),
    })?;
    if !meta.is_file() {
        return Err(VerificationRefusal::BinaryMissing {
            path: resolved.display().to_string(),
            detail: "not a regular file".to_string(),
        });
    }
    Ok(resolved)
}

/// First executable match for `name` on `PATH`.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|c| c.is_file())
}

/// sha256 of a file, lowercase hex.
///
/// Streamed in chunks rather than read whole: a quarry binary is tens of megabytes,
/// and buffering it per spawn would be a straightforward way to make a memory
/// problem out of a security control.
async fn digest_of(path: &Path) -> Result<String, VerificationRefusal> {
    let unreadable = |e: std::io::Error| VerificationRefusal::DigestUnreadable {
        path: path.display().to_string(),
        detail: e.to_string(),
    };
    let mut file = tokio::fs::File::open(path).await.map_err(unreadable)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await.map_err(unreadable)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Whether this process can write to `path`.
///
/// Asks the kernel via `faccessat(..., W_OK, AT_EACCESS)` rather than deriving an
/// answer from mode bits and uids. Mode arithmetic gets ACLs, supplementary groups,
/// read-only mounts, and immutable flags wrong, and every one of those errors points
/// the wrong way: it reports a writable file as safe.
///
/// `AT_EACCESS` uses the effective ids, which are the ones the kernel will check.
/// For a directory, write permission means entries can be created and removed —
/// which is what a binary-replacing rename needs.
#[cfg(unix)]
fn is_writable_by_us(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
        // An interior NUL cannot be a real path we opened; treat the unanswerable
        // question as the unsafe answer.
        return true;
    };
    unsafe { libc::faccessat(libc::AT_FDCWD, c.as_ptr(), libc::W_OK, libc::AT_EACCESS) == 0 }
}

/// Non-unix fallback: assume writable.
///
/// Fails closed on a platform where the check is not implemented, rather than
/// reporting "not writable" and letting the spawn through on an unverified
/// assumption.
#[cfg(not(unix))]
fn is_writable_by_us(_path: &Path) -> bool {
    true
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test scaffolding ──────────────────────────────────────────────────────

    /// A verifier that returns whatever the test tells it to.
    struct FakeVerifier {
        result: std::sync::Mutex<Result<SignedMaterial, VerificationRefusal>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeVerifier {
        fn ok(manifest_json: &str, digest_from_request: bool) -> Arc<Self> {
            Arc::new(Self {
                result: std::sync::Mutex::new(Ok(SignedMaterial {
                    // A sentinel the gate should reject unless the test wants the
                    // request's own digest echoed back.
                    digest: if digest_from_request {
                        String::new()
                    } else {
                        "deadbeef".to_string()
                    },
                    manifest_json: manifest_json.to_string(),
                })),
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn refusing(refusal: VerificationRefusal) -> Arc<Self> {
            Arc::new(Self {
                result: std::sync::Mutex::new(Err(refusal)),
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl SignatureVerifier for FakeVerifier {
        fn verify(&self, req: &VerifyRequest) -> Result<SignedMaterial, VerificationRefusal> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            // A real verifier never sees an empty identity — the gate refuses first.
            assert!(
                !req.expected_identity.is_empty() && !req.expected_issuer.is_empty(),
                "the gate must not call a verifier with no identity to check"
            );
            let mut out = self.result.lock().unwrap().clone();
            if let Ok(m) = &mut out {
                if m.digest.is_empty() {
                    m.digest = req.digest.clone();
                }
            }
            out
        }
    }

    /// A binary-shaped file in a read-only directory, so the writability checks pass.
    struct Fixture {
        _dir: tempfile::TempDir,
        binary: PathBuf,
    }

    impl Fixture {
        fn new(contents: &[u8]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            let binary = bin_dir.join("quarry");
            std::fs::write(&binary, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o555)).unwrap();
                std::fs::set_permissions(&bin_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
            Self { _dir: dir, binary }
        }

        fn path(&self) -> String {
            self.binary.display().to_string()
        }

        /// Make the containing directory writable again, so the TempDir can clean up.
        fn unlock(&self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(p) = self.binary.parent() {
                    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755));
                }
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.unlock();
        }
    }

    fn manifest_json(port: u16, dir: &str) -> String {
        format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":{port}}},
                 {{"kind":"writable-dir","path":"{dir}"}}]}}"#
        )
    }

    fn verified_config() -> QuarryVerificationConfig {
        QuarryVerificationConfig {
            enabled: true,
            expected_identity:
                "https://github.com/scttfrdmn/quarry/.github/workflows/release.yml@refs/tags/*"
                    .to_string(),
            expected_issuer: "https://token.actions.githubusercontent.com".to_string(),
            cosign_path: "cosign".to_string(),
            allow_writable_binary: false,
            manifest_path: String::new(),
        }
    }

    fn gate(fx: &Fixture, cfg: QuarryVerificationConfig) -> SpawnGate {
        let _ = fx;
        SpawnGate::new(cfg, "quarry-runs".to_string(), Some(8080))
    }

    // ── The shipped state: enabled, no mechanism, refuses ─────────────────────

    #[tokio::test]
    async fn verification_enabled_with_no_verifier_refuses_every_spawn() {
        // The state this repo ships in. Not a stub — the fail-closed contract. A
        // future change that makes this pass without a verifier has broken the
        // control, which is why the assertion is on the specific reason.
        let fx = Fixture::new(b"#!/bin/sh\n");
        let err = gate(&fx, verified_config())
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "mechanism_unavailable");
        assert!(
            err.to_string().contains("#103"),
            "the operator must be told where the mechanism lives: {err}"
        );
        assert!(err.digest().is_some(), "the digest was computed by then");
    }

    #[tokio::test]
    async fn a_default_config_is_verification_enabled() {
        // Default-enabled, so an operator who writes no `verification` block gets
        // the safe behaviour rather than an unverified one.
        assert!(QuarryVerificationConfig::default().enabled);
    }

    // ── Identity constraint ───────────────────────────────────────────────────

    #[tokio::test]
    async fn a_missing_identity_refuses_before_the_verifier_is_called() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let mut cfg = verified_config();
        cfg.expected_identity = String::new();
        let err = gate(&fx, cfg)
            .with_verifier(v.clone())
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "identity_not_configured");
        assert!(err.to_string().contains("expected_identity"));
        assert_eq!(
            v.calls(),
            0,
            "an unconstrained verification is worse than none: it succeeds"
        );
    }

    #[tokio::test]
    async fn a_missing_issuer_refuses_too_and_names_both_when_both_are_absent() {
        let fx = Fixture::new(b"bin");
        let mut cfg = verified_config();
        cfg.expected_issuer = String::new();
        let err = gate(&fx, cfg.clone()).check(&fx.path()).await.unwrap_err();
        assert_eq!(err.code(), "identity_not_configured");
        assert!(err.to_string().contains("expected_issuer"));

        cfg.expected_identity = String::new();
        let err = gate(&fx, cfg).check(&fx.path()).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected_identity") && msg.contains("expected_issuer"),
            "{msg}"
        );
    }

    #[tokio::test]
    async fn a_validly_signed_artifact_from_the_wrong_identity_is_refused() {
        // The case that separates identity-constrained verification from
        // signature-present verification. Not an unsigned artifact — a properly
        // signed one, by the wrong signer.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::refusing(VerificationRefusal::WrongIdentity {
            digest: "abc".to_string(),
            expected: "quarry-release".to_string(),
            found: "some-other-project".to_string(),
        });
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "wrong_identity");
        assert!(err.to_string().contains("some-other-project"));
    }

    #[tokio::test]
    async fn a_signature_from_the_wrong_issuer_is_refused() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::refusing(VerificationRefusal::WrongIssuer {
            digest: "abc".to_string(),
            expected: "https://token.actions.githubusercontent.com".to_string(),
            found: "https://attacker.example/oidc".to_string(),
        });
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "wrong_issuer");
    }

    #[tokio::test]
    async fn an_unsigned_artifact_is_refused() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::refusing(VerificationRefusal::Unsigned {
            digest: "abc".to_string(),
        });
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "unsigned");
    }

    #[tokio::test]
    async fn an_unreachable_transparency_log_refuses_rather_than_proceeding() {
        // Fail closed on infrastructure failure. A network problem must not become
        // an unverified run: "the log was down" is how an unsigned binary runs.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::refusing(VerificationRefusal::TransparencyLogUnreachable {
            digest: "abc".to_string(),
            detail: "dial tcp: i/o timeout".to_string(),
        });
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "transparency_log_unreachable");
        assert!(err.to_string().contains("refused"));
    }

    #[tokio::test]
    async fn a_missing_cosign_binary_refuses_rather_than_skipping_the_check() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::refusing(VerificationRefusal::CosignUnavailable {
            path: "cosign".to_string(),
            detail: "No such file or directory".to_string(),
        });
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "cosign_unavailable");
    }

    // ── Digest keying ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn the_signature_check_is_cached_by_digest_not_repeated_per_spawn() {
        let fx = Fixture::new(b"bin-v1");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let g = gate(&fx, verified_config()).with_verifier(v.clone());
        let a = g.check(&fx.path()).await.unwrap();
        let b = g.check(&fx.path()).await.unwrap();
        assert_eq!(a.digest, b.digest);
        assert!(a.signature_checked && b.signature_checked);
        assert_eq!(v.calls(), 1, "verified once, spawned twice");
    }

    #[tokio::test]
    async fn an_upgraded_binary_re_verifies_because_its_digest_changed() {
        // Re-verification is triggered by content, not by a timer. An upgrade must
        // never run on the previous binary's verification.
        let fx = Fixture::new(b"bin-v1");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let g = gate(&fx, verified_config()).with_verifier(v.clone());
        let first = g.check(&fx.path()).await.unwrap();
        assert_eq!(v.calls(), 1);

        fx.unlock();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fx.binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(&fx.binary, b"bin-v2-upgraded").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fx.binary, std::fs::Permissions::from_mode(0o555)).unwrap();
            std::fs::set_permissions(
                fx.binary.parent().unwrap(),
                std::fs::Permissions::from_mode(0o555),
            )
            .unwrap();
        }

        let second = g.check(&fx.path()).await.unwrap();
        assert_ne!(first.digest, second.digest, "the content changed");
        assert_eq!(v.calls(), 2, "a new digest is a new verification");
    }

    #[tokio::test]
    async fn a_verifier_answering_about_other_bytes_is_refused() {
        // The verified digest must be the spawned digest. A verifier that reported on
        // a different artifact has produced an answer that does not apply here.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), false);
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "digest_mismatch");
    }

    #[tokio::test]
    async fn the_digest_is_the_sha256_of_the_file() {
        let fx = Fixture::new(b"hello");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let out = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap();
        // sha256("hello")
        assert_eq!(
            out.digest,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    // ── Manifest ──────────────────────────────────────────────────────────────

    fn parse(json: &str) -> Result<CapabilityManifest, String> {
        serde_json::from_str(json).map_err(|e| e.to_string())
    }

    fn expect() -> ExpectedCapabilities {
        ExpectedCapabilities {
            gateway_port: Some(8080),
            run_record_dir: "quarry-runs".to_string(),
        }
    }

    #[test]
    fn the_exact_two_capabilities_are_accepted() {
        let m = parse(&manifest_json(8080, "quarry-runs")).unwrap();
        assert!(m.check(&expect()).is_ok());
    }

    #[test]
    fn a_manifest_declaring_anything_more_is_refused() {
        let m = parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":8080}},
                 {{"kind":"writable-dir","path":"quarry-runs"}},
                 {{"kind":"network-egress"}}]}}"#
        ))
        .unwrap();
        let fault = m.check(&expect()).unwrap_err();
        assert_eq!(fault.code(), "manifest_overbroad");
        assert!(fault.to_string().contains("network-egress"));
    }

    #[test]
    fn a_second_writable_directory_is_refused_not_ignored() {
        // A check that merely looked for "one writable dir" would find one here and
        // pass, granting the second silently.
        let m = parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":8080}},
                 {{"kind":"writable-dir","path":"quarry-runs"}},
                 {{"kind":"writable-dir","path":"/etc"}}]}}"#
        ))
        .unwrap();
        assert_eq!(
            m.check(&expect()).unwrap_err().code(),
            "manifest_duplicate_capability"
        );
    }

    #[test]
    fn egress_to_a_non_loopback_host_is_refused() {
        let m = parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":8080,"host":"api.anthropic.com"}},
                 {{"kind":"writable-dir","path":"quarry-runs"}}]}}"#
        ))
        .unwrap();
        assert_eq!(
            m.check(&expect()).unwrap_err().code(),
            "manifest_non_loopback_host"
        );
    }

    #[test]
    fn loopback_spellings_are_all_accepted() {
        for host in LOOPBACK_HOSTS {
            let m = parse(&format!(
                r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                     {{"kind":"localhost-egress","port":8080,"host":"{host}"}},
                     {{"kind":"writable-dir","path":"quarry-runs"}}]}}"#
            ))
            .unwrap();
            assert!(m.check(&expect()).is_ok(), "{host} should be loopback");
        }
    }

    #[test]
    fn egress_to_the_wrong_port_is_refused() {
        let m = parse(&manifest_json(9999, "quarry-runs")).unwrap();
        let fault = m.check(&expect()).unwrap_err();
        assert_eq!(fault.code(), "manifest_wrong_egress_port");
        assert!(fault.to_string().contains("9999"));
    }

    #[test]
    fn an_unknown_gateway_port_refuses_rather_than_accepting_any_port() {
        // `None` is not "no constraint". A supervisor that was never told the port
        // cannot check it, and the fail-closed reading of "cannot check" is "refuse".
        let m = parse(&manifest_json(8080, "quarry-runs")).unwrap();
        let mut e = expect();
        e.gateway_port = None;
        assert_eq!(
            m.check(&e).unwrap_err().code(),
            "manifest_gateway_port_unknown"
        );
    }

    #[test]
    fn a_writable_dir_outside_the_run_record_dir_is_refused() {
        let m = parse(&manifest_json(8080, "/etc")).unwrap();
        let fault = m.check(&expect()).unwrap_err();
        assert_eq!(fault.code(), "manifest_wrong_writable_dir");
        assert!(fault.to_string().contains("/etc"));
    }

    #[test]
    fn a_missing_required_capability_is_refused() {
        for (json, missing) in [
            (
                format!(
                    r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                         {{"kind":"writable-dir","path":"quarry-runs"}}]}}"#
                ),
                "localhost-egress",
            ),
            (
                format!(
                    r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                         {{"kind":"localhost-egress","port":8080}}]}}"#
                ),
                "writable-dir",
            ),
        ] {
            let fault = parse(&json).unwrap().check(&expect()).unwrap_err();
            assert_eq!(fault.code(), "manifest_missing_capability");
            assert!(fault.to_string().contains(missing), "{fault}");
        }
    }

    #[test]
    fn an_empty_capability_list_is_refused_not_treated_as_harmless() {
        // Declaring nothing is not declaring nothing dangerous — it is a manifest
        // that does not describe quarry, and quarry needs both capabilities to run.
        let m = parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[]}}"#
        ))
        .unwrap();
        assert_eq!(
            m.check(&expect()).unwrap_err().code(),
            "manifest_missing_capability"
        );
    }

    #[test]
    fn an_unknown_schema_is_refused() {
        let m = parse(
            r#"{"schema":"quarry-capability-manifest/2","capabilities":[
                 {"kind":"localhost-egress","port":8080},
                 {"kind":"writable-dir","path":"quarry-runs"}]}"#,
        )
        .unwrap();
        assert_eq!(
            m.check(&expect()).unwrap_err().code(),
            "manifest_unknown_schema"
        );
    }

    #[test]
    fn an_extra_field_fails_to_parse_rather_than_being_ignored() {
        // A capability carrying a field this reader does not know may mean something
        // to a more permissive reader. Refusing to parse is the only safe reading.
        assert!(parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":8080,"allow_all":true}},
                 {{"kind":"writable-dir","path":"quarry-runs"}}]}}"#
        ))
        .is_err());
        assert!(parse(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[],"trusted":true}}"#
        ))
        .is_err());
    }

    #[test]
    fn manifest_fault_codes_are_unique() {
        let all = [
            ManifestFault::UnknownSchema {
                found: String::new(),
            },
            ManifestFault::Overbroad {
                capability: String::new(),
            },
            ManifestFault::Missing {
                kind: String::new(),
            },
            ManifestFault::Duplicate {
                kind: String::new(),
            },
            ManifestFault::NonLoopbackHost {
                host: String::new(),
            },
            ManifestFault::EgressPortMissing,
            ManifestFault::WrongEgressPort {
                declared: 0,
                expected: 0,
            },
            ManifestFault::GatewayPortUnknown,
            ManifestFault::WritableDirMissing,
            ManifestFault::WrongWritableDir {
                declared: String::new(),
                expected: String::new(),
            },
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|f| f.code()).collect();
        assert_eq!(codes.len(), all.len());
    }

    #[tokio::test]
    async fn a_signed_manifest_declaring_too_much_refuses_the_spawn() {
        // The manifest check is part of verification, not decoration on top of it: a
        // correctly-signed artifact whose manifest asks for more is still refused.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok(
            &format!(
                r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                     {{"kind":"localhost-egress","port":8080}},
                     {{"kind":"writable-dir","path":"quarry-runs"}},
                     {{"kind":"shell"}}]}}"#
            ),
            true,
        );
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "manifest_rejected");
        assert!(err.to_string().contains("shell"));
    }

    #[tokio::test]
    async fn signed_material_with_no_manifest_is_refused() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok("   ", true);
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "manifest_missing");
    }

    #[tokio::test]
    async fn an_unparseable_signed_manifest_is_refused() {
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok("{not json", true);
        let err = gate(&fx, verified_config())
            .with_verifier(v)
            .check(&fx.path())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "manifest_unparseable");
    }

    #[tokio::test]
    async fn a_rejected_manifest_is_rejected_again_on_the_next_spawn() {
        // The cache holds the signed manifest text, not a verdict. So the second
        // spawn skips the expensive signature check and still re-runs the capability
        // check — and still refuses. A cache that stored "digest ok" would let a
        // rejected manifest through the moment it was consulted.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok(&manifest_json(9999, "quarry-runs"), true);
        let g = gate(&fx, verified_config()).with_verifier(v.clone());
        assert_eq!(
            g.check(&fx.path()).await.unwrap_err().code(),
            "manifest_rejected"
        );
        assert_eq!(
            g.check(&fx.path()).await.unwrap_err().code(),
            "manifest_rejected"
        );
    }

    #[tokio::test]
    async fn the_capability_check_re_runs_against_current_config_not_a_cached_verdict() {
        // A gateway whose port changed under it must re-evaluate. Storing a verdict
        // rather than the manifest would wave the old decision through.
        let fx = Fixture::new(b"bin");
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let mut g = gate(&fx, verified_config()).with_verifier(v.clone());
        assert!(g.check(&fx.path()).await.is_ok());

        g.set_gateway_port(Some(9090));
        let err = g.check(&fx.path()).await.unwrap_err();
        assert_eq!(err.code(), "manifest_rejected");
        assert_eq!(
            v.calls(),
            1,
            "the signature was cached; only the cheap policy check re-ran"
        );
    }

    // ── The binary itself ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn a_missing_binary_is_refused_with_its_own_reason() {
        let fx = Fixture::new(b"bin");
        let g = gate(&fx, verified_config());
        let err = g
            .check("/nonexistent/definitely-not-quarry")
            .await
            .unwrap_err();
        assert_eq!(err.code(), "binary_missing");
        assert!(err.digest().is_none(), "nothing was hashed");
    }

    #[tokio::test]
    async fn a_directory_is_not_a_binary() {
        let dir = tempfile::tempdir().unwrap();
        let g = SpawnGate::new(verified_config(), "quarry-runs".to_string(), Some(8080));
        let err = g
            .check(&dir.path().display().to_string())
            .await
            .unwrap_err();
        assert_eq!(err.code(), "binary_missing");
        assert!(err.to_string().contains("not a regular file"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_writable_binary_is_refused_by_default() {
        // The TOCTOU window this closes: a verified digest proves nothing about what
        // executes if the file can be rewritten between the two.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let g = SpawnGate::new(verified_config(), "quarry-runs".to_string(), Some(8080));
        let err = g.check(&bin.display().to_string()).await.unwrap_err();
        assert_eq!(err.code(), "writable_binary");
        assert!(err.digest().is_none(), "refused before hashing");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_writable_directory_is_refused_even_when_the_binary_is_read_only() {
        // A read-only file in a writable directory can be replaced by rename. The
        // file's own mode bits are not the whole story.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
        // The temp dir itself is writable by us, which is the condition under test.

        let g = SpawnGate::new(verified_config(), "quarry-runs".to_string(), Some(8080));
        let err = g.check(&bin.display().to_string()).await.unwrap_err();
        assert_eq!(err.code(), "writable_binary_dir");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allow_writable_binary_proceeds_but_the_manifest_check_still_applies() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"x").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut cfg = verified_config();
        cfg.allow_writable_binary = true;
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let g = SpawnGate::new(cfg, "quarry-runs".to_string(), Some(8080)).with_verifier(v);
        let out = g.check(&bin.display().to_string()).await.unwrap();
        assert!(out.signature_checked);
    }

    // ── Development mode ──────────────────────────────────────────────────────

    fn dev_fixture(manifest: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"dev-binary").unwrap();
        std::fs::write(dir.path().join("quarry.manifest.json"), manifest).unwrap();
        (dir, bin)
    }

    fn dev_config() -> QuarryVerificationConfig {
        QuarryVerificationConfig {
            enabled: false,
            // A writable binary is the normal case for a developer build, and the
            // escape hatch has to actually be usable or nobody will use it.
            allow_writable_binary: true,
            ..QuarryVerificationConfig::default()
        }
    }

    #[tokio::test]
    async fn development_mode_skips_the_signature_and_records_that_it_did() {
        let (_d, bin) = dev_fixture(&manifest_json(8080, "quarry-runs"));
        let g = SpawnGate::new(dev_config(), "quarry-runs".to_string(), Some(8080));
        let out = g.check(&bin.display().to_string()).await.unwrap();
        assert!(
            !out.signature_checked,
            "an audit log that cannot tell a verified run from a development one is useless"
        );
    }

    #[tokio::test]
    async fn development_mode_does_not_relax_the_manifest_capability_check() {
        // Provenance and sandboxing are separable, and only the first is a
        // development convenience. Turning off signatures must not turn off the
        // capability contract.
        let (_d, bin) = dev_fixture(&format!(
            r#"{{"schema":"{MANIFEST_SCHEMA}","capabilities":[
                 {{"kind":"localhost-egress","port":8080}},
                 {{"kind":"writable-dir","path":"/"}}]}}"#
        ));
        let g = SpawnGate::new(dev_config(), "quarry-runs".to_string(), Some(8080));
        let err = g.check(&bin.display().to_string()).await.unwrap_err();
        assert_eq!(err.code(), "manifest_rejected");
    }

    #[tokio::test]
    async fn development_mode_with_no_manifest_at_all_is_still_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"dev").unwrap();
        let g = SpawnGate::new(dev_config(), "quarry-runs".to_string(), Some(8080));
        let err = g.check(&bin.display().to_string()).await.unwrap_err();
        assert_eq!(err.code(), "manifest_unparseable");
        assert!(err.to_string().contains("cannot read"));
    }

    #[tokio::test]
    async fn development_mode_never_consults_a_verifier() {
        let (_d, bin) = dev_fixture(&manifest_json(8080, "quarry-runs"));
        let v = FakeVerifier::ok(&manifest_json(8080, "quarry-runs"), true);
        let g = SpawnGate::new(dev_config(), "quarry-runs".to_string(), Some(8080))
            .with_verifier(v.clone());
        g.check(&bin.display().to_string()).await.unwrap();
        assert_eq!(v.calls(), 0);
    }

    #[tokio::test]
    async fn the_verified_path_never_falls_back_to_the_sidecar() {
        // A sidecar manifest sitting beside the binary must not satisfy the verified
        // path — an unsigned manifest is a manifest an attacker writes.
        let (_d, bin) = dev_fixture(&manifest_json(8080, "quarry-runs"));
        let mut cfg = verified_config();
        cfg.allow_writable_binary = true;
        let g = SpawnGate::new(cfg, "quarry-runs".to_string(), Some(8080));
        let err = g.check(&bin.display().to_string()).await.unwrap_err();
        assert_eq!(
            err.code(),
            "mechanism_unavailable",
            "a valid sidecar next to the binary must not stand in for signed material"
        );
    }

    #[tokio::test]
    async fn an_explicit_manifest_path_is_honoured_in_development_mode() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quarry");
        std::fs::write(&bin, b"dev").unwrap();
        let elsewhere = dir.path().join("caps.json");
        std::fs::write(&elsewhere, manifest_json(8080, "quarry-runs")).unwrap();
        let mut cfg = dev_config();
        cfg.manifest_path = elsewhere.display().to_string();
        let g = SpawnGate::new(cfg, "quarry-runs".to_string(), Some(8080));
        assert!(g.check(&bin.display().to_string()).await.is_ok());
    }

    // ── Two audiences ─────────────────────────────────────────────────────────

    #[test]
    fn the_sender_message_leaks_no_path_digest_identity_or_config_key() {
        let secrets = [
            "/opt/secret/quarry",
            "deadbeefcafe",
            "https://token.actions.githubusercontent.com",
            "quarry.verification",
            "cosign",
            "#103",
        ];
        let all = [
            VerificationRefusal::BinaryMissing {
                path: "/opt/secret/quarry".into(),
                detail: "no such file".into(),
            },
            VerificationRefusal::DigestUnreadable {
                path: "/opt/secret/quarry".into(),
                detail: "denied".into(),
            },
            VerificationRefusal::WritableBinary {
                path: "/opt/secret/quarry".into(),
            },
            VerificationRefusal::WritableBinaryDir {
                path: "/opt/secret".into(),
            },
            VerificationRefusal::MechanismUnavailable {
                digest: "deadbeefcafe".into(),
            },
            VerificationRefusal::IdentityNotConfigured {
                missing: "quarry.verification.expected_identity".into(),
            },
            VerificationRefusal::Unsigned {
                digest: "deadbeefcafe".into(),
            },
            VerificationRefusal::WrongIdentity {
                digest: "deadbeefcafe".into(),
                expected: "quarry".into(),
                found: "attacker".into(),
            },
            VerificationRefusal::WrongIssuer {
                digest: "deadbeefcafe".into(),
                expected: "https://token.actions.githubusercontent.com".into(),
                found: "https://attacker.example".into(),
            },
            VerificationRefusal::TransparencyLogUnreachable {
                digest: "deadbeefcafe".into(),
                detail: "timeout".into(),
            },
            VerificationRefusal::CosignUnavailable {
                path: "cosign".into(),
                detail: "missing".into(),
            },
            VerificationRefusal::ManifestMissing {
                digest: "deadbeefcafe".into(),
            },
            VerificationRefusal::ManifestUnparseable {
                digest: "deadbeefcafe".into(),
                detail: "bad".into(),
            },
            VerificationRefusal::ManifestRejected {
                digest: "deadbeefcafe".into(),
                fault: ManifestFault::Overbroad {
                    capability: "shell".into(),
                },
            },
            VerificationRefusal::DigestMismatch {
                verified: "deadbeefcafe".into(),
                spawning: "0000".into(),
            },
        ];
        for r in &all {
            let msg = r.sender_message();
            for s in secrets {
                assert!(
                    !msg.contains(s),
                    "{} leaks '{s}' to the sender: {msg}",
                    r.code()
                );
            }
        }
    }

    #[test]
    fn the_operator_form_says_what_the_sender_form_deliberately_omits() {
        // The two audiences are the whole point: the sender learns nothing
        // diagnostic, and the operator learns which check failed and against what. A
        // refusal that told neither of them would satisfy the leak test above.
        let cases: &[(VerificationRefusal, &str)] = &[
            (
                VerificationRefusal::BinaryMissing {
                    path: "/opt/secret/quarry".into(),
                    detail: "No such file or directory".into(),
                },
                "/opt/secret/quarry",
            ),
            (
                VerificationRefusal::WritableBinary {
                    path: "/opt/secret/quarry".into(),
                },
                "/opt/secret/quarry",
            ),
            (
                VerificationRefusal::WrongIdentity {
                    digest: "deadbeefcafe".into(),
                    expected: "quarry-release".into(),
                    found: "attacker".into(),
                },
                "attacker",
            ),
            (
                VerificationRefusal::ManifestRejected {
                    digest: "deadbeefcafe".into(),
                    fault: ManifestFault::Overbroad {
                        capability: "shell".into(),
                    },
                },
                "shell",
            ),
        ];
        for (r, detail) in cases {
            assert!(
                r.to_string().contains(detail),
                "{} must name '{detail}' to the operator: {r}",
                r.code()
            );
            assert!(!r.sender_message().contains(detail));
        }
    }

    #[test]
    fn refusal_codes_are_unique_so_one_check_cannot_be_mistaken_for_another() {
        // "verification failed" alone sends an operator hunting. Each check must be
        // separately nameable in logs and metrics.
        let all: Vec<VerificationRefusal> = vec![
            VerificationRefusal::BinaryMissing {
                path: String::new(),
                detail: String::new(),
            },
            VerificationRefusal::DigestUnreadable {
                path: String::new(),
                detail: String::new(),
            },
            VerificationRefusal::WritableBinary {
                path: String::new(),
            },
            VerificationRefusal::WritableBinaryDir {
                path: String::new(),
            },
            VerificationRefusal::MechanismUnavailable {
                digest: String::new(),
            },
            VerificationRefusal::IdentityNotConfigured {
                missing: String::new(),
            },
            VerificationRefusal::Unsigned {
                digest: String::new(),
            },
            VerificationRefusal::WrongIdentity {
                digest: String::new(),
                expected: String::new(),
                found: String::new(),
            },
            VerificationRefusal::WrongIssuer {
                digest: String::new(),
                expected: String::new(),
                found: String::new(),
            },
            VerificationRefusal::TransparencyLogUnreachable {
                digest: String::new(),
                detail: String::new(),
            },
            VerificationRefusal::CosignUnavailable {
                path: String::new(),
                detail: String::new(),
            },
            VerificationRefusal::ManifestMissing {
                digest: String::new(),
            },
            VerificationRefusal::ManifestUnparseable {
                digest: String::new(),
                detail: String::new(),
            },
            VerificationRefusal::ManifestRejected {
                digest: String::new(),
                fault: ManifestFault::EgressPortMissing,
            },
            VerificationRefusal::DigestMismatch {
                verified: String::new(),
                spawning: String::new(),
            },
        ];
        let codes: std::collections::HashSet<_> = all.iter().map(|r| r.code()).collect();
        assert_eq!(codes.len(), all.len());
    }

    // ── same_dir ──────────────────────────────────────────────────────────────

    #[test]
    fn a_trailing_slash_does_not_make_a_different_directory() {
        assert!(same_dir("quarry-runs", "quarry-runs/"));
        assert!(same_dir("/srv/runs/", "/srv/runs"));
    }

    #[test]
    fn a_relative_and_an_absolute_path_to_the_same_directory_match() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir.path().canonicalize().unwrap();
        assert!(same_dir(
            &dir.path().display().to_string(),
            &canonical.display().to_string()
        ));
    }

    #[test]
    fn different_directories_do_not_match() {
        assert!(!same_dir("quarry-runs", "/etc"));
        assert!(!same_dir("quarry-runs", "quarry-runs-2"));
        // A path that traverses out must not match its own prefix.
        assert!(!same_dir("quarry-runs/../etc", "quarry-runs"));
    }

    // ── PATH resolution ───────────────────────────────────────────────────────

    #[test]
    fn a_bare_name_is_resolved_on_path_so_the_hashed_file_is_the_executed_one() {
        // `binary_path` defaults to the bare name `quarry`, and `Command` resolves it
        // on PATH. Hashing a different file than the kernel executes would make the
        // digest meaningless.
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("rustynail-verify-probe");
        std::fs::write(&bin, b"x").unwrap();
        let old = std::env::var_os("PATH");
        // Serialised against nothing: this test is the only one that touches PATH.
        std::env::set_var("PATH", dir.path());
        let found = which_on_path("rustynail-verify-probe");
        if let Some(p) = old {
            std::env::set_var("PATH", p);
        }
        assert_eq!(found.as_deref(), Some(bin.as_path()));
    }

    #[test]
    fn a_bare_name_not_on_path_is_a_missing_binary() {
        let err = resolve_binary("definitely-not-a-real-binary-xyzzy").unwrap_err();
        assert_eq!(err.code(), "binary_missing");
        assert!(err.to_string().contains("PATH"));
    }
}
