//! The verification gate, asserted by **absence of effect**.
//!
//! # Why an integration test rather than more unit tests
//!
//! `src/quarry/verify.rs` already tests every refusal path in isolation: each check
//! fires, each has its own code, each produces its own operator message. What those
//! tests cannot show is the thing that actually matters — that a refusal leaves
//! **nothing behind**. An error return with the child already running is exactly the
//! #90 failure mode: the shell allowlist returned an error while having already done
//! the work.
//!
//! So every test here asserts on the filesystem and on a real spawn counter, not on
//! an `Err` value:
//!
//! - **No child process was started.** A shim in front of the fake appends a line per
//!   invocation, so "was a process spawned" is a fact on disk. Asserting the absence
//!   of events would not do: a refused spawn and a spawn that emitted nothing produce
//!   the same empty stream.
//! - **No run record exists.** Not merely that `record.json` is absent — that the run
//!   *directory* was never created. A refusal that left an empty directory behind
//!   would look, to the retention reaper and to an operator reading `quarry-runs/`,
//!   like a run that happened and produced nothing.
//!
//! The non-vacuity guard is `a_verified_run_does_spawn_and_leaves_a_record`. Without
//! it, every assertion below would pass against a supervisor that refused
//! unconditionally.

use rustynail::config::{QuarryConfig, QuarryVerificationConfig};
use rustynail::quarry::verify::{
    development_manifest_json, SignatureVerifier, SignedMaterial, VerificationRefusal,
    VerifyRequest,
};
use rustynail::quarry::{RunRequest, Supervisor};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The port every manifest here declares as its sole egress target.
const GATEWAY_PORT: u16 = 8080;

// ── Harness ───────────────────────────────────────────────────────────────────

/// A fake quarry behind a shim that records each invocation.
struct Harness {
    _dir: tempfile::TempDir,
    /// The path the supervisor is configured with — the shim, not the fake.
    binary: PathBuf,
    /// One line per spawn.
    invocations: PathBuf,
    /// The configured run-record directory.
    runs: PathBuf,
    /// Where a development-mode manifest goes.
    manifest: PathBuf,
}

impl Harness {
    fn build() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let invocations = dir.path().join("invocations.txt");
        let runs = dir.path().join("runs");
        let binary = dir.path().join("quarry");

        // Emits a minimal but *framed* stream: without the frame the supervisor
        // classifies the run as malformed, and a test asserting a successful run
        // would be asserting the wrong success.
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
printf '%s\n' '{{"type":"answer","text":"two"}}'
printf '%s\n' '{{"type":"receipt","rows":[{{"label":"n0 q","kind":"llm","cost":0.05}}],"total":0.05}}'
printf '%s\n' '{{"type":"quarry_outcome","outcome":"complete","bound_by":"","gaps":0,"unfunded":0,"total_micros":50000,"cap_micros":1000000}}'
[ -n "$OUT" ] && printf '%s' '{{"RunID":"fake","BoundBy":"","Caps":{{"Spend":1000000,"Latency":0,"Due":"0001-01-01T00:00:00Z"}},"Unverified":null,"Outcomes":[{{"NodeID":"n0","Content":"two","Cost":50000,"Model":"m-1","Verified":true,"Children":null}}]}}' > "$OUT"
exit 0
"#,
            log = invocations.display()
        );
        std::fs::write(&binary, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake");
        }

        let mut manifest = binary.as_os_str().to_os_string();
        manifest.push(".manifest.json");

        Self {
            _dir: dir,
            binary,
            invocations,
            runs,
            manifest: PathBuf::from(manifest),
        }
    }

    /// Write a manifest declaring exactly quarry's two capabilities.
    fn write_good_manifest(&self) {
        std::fs::write(
            &self.manifest,
            development_manifest_json(GATEWAY_PORT, &self.runs.display().to_string()),
        )
        .expect("write manifest");
    }

    fn config(&self, verification: QuarryVerificationConfig) -> QuarryConfig {
        QuarryConfig {
            enabled: true,
            binary_path: self.binary.display().to_string(),
            max_concurrent_runs: 4,
            run_record_dir: self.runs.display().to_string(),
            retention_max_runs: 0,
            retention_max_age_seconds: 0,
            run_timeout_seconds: 0,
            verification,
            ..QuarryConfig::default()
        }
    }

    fn supervisor(&self, verification: QuarryVerificationConfig) -> Supervisor {
        Supervisor::new(self.config(verification)).with_gateway_port(GATEWAY_PORT)
    }

    /// How many quarry children were actually started.
    fn spawns(&self) -> usize {
        std::fs::read_to_string(&self.invocations)
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// Every entry under the run-record directory.
    ///
    /// An empty vector means the refusal left nothing at all — not even a directory
    /// that a reaper or an operator would read as a run.
    fn run_dirs(&self) -> Vec<String> {
        match std::fs::read_dir(&self.runs) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Whether any `record.json` exists anywhere beneath the run directory.
    fn any_record(&self) -> bool {
        fn walk(dir: &Path) -> bool {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return false;
            };
            for e in entries.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    if walk(&p) {
                        return true;
                    }
                } else if p.file_name().is_some_and(|n| n == "record.json") {
                    return true;
                }
            }
            false
        }
        walk(&self.runs)
    }
}

/// A verifier that always refuses, with the reason a test picks.
struct Refusing(VerificationRefusal);

impl SignatureVerifier for Refusing {
    fn verify(&self, _req: &VerifyRequest) -> Result<SignedMaterial, VerificationRefusal> {
        Err(self.0.clone())
    }
}

/// A verifier that approves, echoing back the digest it was given.
struct Approving(String);

impl SignatureVerifier for Approving {
    fn verify(&self, req: &VerifyRequest) -> Result<SignedMaterial, VerificationRefusal> {
        Ok(SignedMaterial {
            digest: req.digest.clone(),
            manifest_json: self.0.clone(),
        })
    }
}

fn identity_config() -> QuarryVerificationConfig {
    QuarryVerificationConfig {
        enabled: true,
        expected_identity:
            "https://github.com/scttfrdmn/quarry/.github/workflows/release.yml@refs/tags/*"
                .to_string(),
        expected_issuer: "https://token.actions.githubusercontent.com".to_string(),
        // The fake lives in a temp dir, which is writable by definition. Waived so
        // that these tests exercise the *signature* paths rather than all failing at
        // the writability check — which has its own tests in `verify.rs`.
        allow_writable_binary: true,
        ..QuarryVerificationConfig::default()
    }
}

fn request() -> RunRequest {
    RunRequest::new("u1", "discord", "how many moons does mars have", 1_000_000)
}

/// Run once and assert the refusal produced no effect at all.
async fn assert_refused_without_effect(h: &Harness, s: &Supervisor, expected_code: &str) {
    let err = s.run(request(), None, None).await.unwrap_err();
    assert_eq!(err.code(), expected_code, "wrong refusal: {err}");

    // The three assertions this file exists for.
    assert_eq!(
        h.spawns(),
        0,
        "a refused run started a child process: {err}"
    );
    assert!(
        h.run_dirs().is_empty(),
        "a refused run left {:?} behind; an empty run directory reads as a run that \
         happened and produced nothing",
        h.run_dirs()
    );
    assert!(!h.any_record(), "a refused run wrote a record");
    assert_eq!(
        s.active_runs(),
        0,
        "a refused run held its concurrency slot"
    );
}

// ── The non-vacuity guard ─────────────────────────────────────────────────────

/// Establishes that this harness *can* spawn and *does* leave a record.
///
/// Without it, every assertion below would pass against a supervisor that refused
/// unconditionally — the vacuous pass that makes a negative test worthless. Read this
/// and the refusal tests as one property.
#[tokio::test]
async fn a_verified_run_does_spawn_and_leaves_a_record() {
    let h = Harness::build();
    let manifest = development_manifest_json(GATEWAY_PORT, &h.runs.display().to_string());
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving(manifest)));

    let out = s.run(request(), None, None).await.expect("verified run");
    assert_eq!(h.spawns(), 1, "the harness can spawn");
    assert_eq!(h.run_dirs().len(), 1, "and leaves a run directory");
    assert!(h.any_record(), "and a record inside it");
    assert_eq!(out.answer(), Some("two"));
}

// ── Every refusal path leaves nothing behind ───────────────────────────────────

#[tokio::test]
async fn no_verifier_installed_refuses_and_spawns_nothing() {
    // The state this repo ships in: `enabled: true` with the mechanism (#103)
    // unimplemented. Fail-closed means no run, not a warned-about run.
    let h = Harness::build();
    let s = h.supervisor(identity_config());
    assert_refused_without_effect(&h, &s, "mechanism_unavailable").await;
}

#[tokio::test]
async fn an_unsigned_binary_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(VerificationRefusal::Unsigned {
            digest: "abc".into(),
        })));
    assert_refused_without_effect(&h, &s, "unsigned").await;
}

#[tokio::test]
async fn a_validly_signed_binary_from_the_wrong_identity_refuses_and_spawns_nothing() {
    // The case that distinguishes identity-constrained verification from
    // signature-present verification. The artifact is properly signed — by someone
    // else.
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(VerificationRefusal::WrongIdentity {
            digest: "abc".into(),
            expected: "quarry-release".into(),
            found: "https://github.com/attacker/evil/.github/workflows/x.yml@refs/heads/main"
                .into(),
        })));
    assert_refused_without_effect(&h, &s, "wrong_identity").await;
}

#[tokio::test]
async fn a_signature_from_the_wrong_issuer_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(VerificationRefusal::WrongIssuer {
            digest: "abc".into(),
            expected: "https://token.actions.githubusercontent.com".into(),
            found: "https://attacker.example/oidc".into(),
        })));
    assert_refused_without_effect(&h, &s, "wrong_issuer").await;
}

#[tokio::test]
async fn an_unconfigured_identity_refuses_and_spawns_nothing() {
    // An unconstrained check is worse than none because it succeeds, so the gate
    // refuses rather than calling a verifier with nothing to constrain it.
    let h = Harness::build();
    let mut cfg = identity_config();
    cfg.expected_identity = String::new();
    let manifest = development_manifest_json(GATEWAY_PORT, &h.runs.display().to_string());
    let s = h
        .supervisor(cfg)
        .with_verifier(Arc::new(Approving(manifest)));
    assert_refused_without_effect(&h, &s, "identity_not_configured").await;
}

#[tokio::test]
async fn an_unreachable_transparency_log_refuses_and_spawns_nothing() {
    // An infrastructure failure must not become an unverified run. "The log was
    // down" is how an unsigned binary ends up executing.
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(
            VerificationRefusal::TransparencyLogUnreachable {
                digest: "abc".into(),
                detail: "dial tcp rekor.sigstore.dev:443: i/o timeout".into(),
            },
        )));
    assert_refused_without_effect(&h, &s, "transparency_log_unreachable").await;
}

#[tokio::test]
async fn a_missing_cosign_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(VerificationRefusal::CosignUnavailable {
            path: "cosign".into(),
            detail: "No such file or directory (os error 2)".into(),
        })));
    assert_refused_without_effect(&h, &s, "cosign_unavailable").await;
}

#[tokio::test]
async fn a_manifest_declaring_more_than_quarrys_contract_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let overbroad = serde_json::json!({
        "schema": "quarry-capability-manifest/1",
        "capabilities": [
            {"kind": "localhost-egress", "port": GATEWAY_PORT},
            {"kind": "writable-dir", "path": h.runs.display().to_string()},
            {"kind": "shell"},
        ],
    })
    .to_string();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving(overbroad)));
    assert_refused_without_effect(&h, &s, "manifest_rejected").await;
}

#[tokio::test]
async fn a_manifest_naming_a_different_writable_directory_refuses_and_spawns_nothing() {
    // The declared directory must be *the* run-record directory. A manifest naming
    // somewhere else has not declared what it will actually do.
    let h = Harness::build();
    let elsewhere = development_manifest_json(GATEWAY_PORT, "/tmp/somewhere-else");
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving(elsewhere)));
    assert_refused_without_effect(&h, &s, "manifest_rejected").await;
}

#[tokio::test]
async fn a_manifest_declaring_egress_to_a_different_port_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let wrong_port = development_manifest_json(9999, &h.runs.display().to_string());
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving(wrong_port)));
    assert_refused_without_effect(&h, &s, "manifest_rejected").await;
}

#[tokio::test]
async fn an_unparseable_manifest_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving("{not json".to_string())));
    assert_refused_without_effect(&h, &s, "manifest_unparseable").await;
}

#[tokio::test]
async fn signed_material_with_no_manifest_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Approving(String::new())));
    assert_refused_without_effect(&h, &s, "manifest_missing").await;
}

#[tokio::test]
async fn a_missing_binary_refuses_and_spawns_nothing() {
    let h = Harness::build();
    let mut cfg = h.config(identity_config());
    cfg.binary_path = "/nonexistent/definitely-not-quarry".to_string();
    let s = Supervisor::new(cfg).with_gateway_port(GATEWAY_PORT);
    assert_refused_without_effect(&h, &s, "binary_missing").await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_writable_binary_refuses_and_spawns_nothing() {
    // The default. The temp-dir fake is writable, so the refusal here needs no
    // arrangement beyond *not* setting the escape hatch.
    let h = Harness::build();
    let mut cfg = identity_config();
    cfg.allow_writable_binary = false;
    let s = h.supervisor(cfg);
    assert_refused_without_effect(&h, &s, "writable_binary").await;
}

#[tokio::test]
async fn an_unknown_gateway_port_refuses_and_spawns_nothing() {
    // A supervisor never told the gateway's port cannot check the manifest's declared
    // egress, and "cannot check" fails closed. Built without `with_gateway_port`.
    let h = Harness::build();
    let manifest = development_manifest_json(GATEWAY_PORT, &h.runs.display().to_string());
    let s =
        Supervisor::new(h.config(identity_config())).with_verifier(Arc::new(Approving(manifest)));
    assert_refused_without_effect(&h, &s, "manifest_rejected").await;
}

// ── The development escape hatch ──────────────────────────────────────────────

#[tokio::test]
async fn development_mode_spawns_but_still_enforces_the_manifest() {
    // `enabled: false` bypasses the signature only. Provenance and sandboxing are
    // separable, and only the first is a development convenience — so a development
    // build whose manifest asks for too much is still refused.
    let h = Harness::build();
    h.write_good_manifest();
    let dev = QuarryVerificationConfig {
        enabled: false,
        allow_writable_binary: true,
        ..QuarryVerificationConfig::default()
    };
    let out = h
        .supervisor(dev.clone())
        .run(request(), None, None)
        .await
        .expect("development mode runs");
    assert_eq!(out.answer(), Some("two"));
    assert_eq!(h.spawns(), 1);

    // Now break the manifest and try again: still refused, and nothing new spawned.
    let h2 = Harness::build();
    std::fs::write(
        &h2.manifest,
        serde_json::json!({
            "schema": "quarry-capability-manifest/1",
            "capabilities": [
                {"kind": "localhost-egress", "port": GATEWAY_PORT},
                {"kind": "writable-dir", "path": "/"},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let s2 = h2.supervisor(dev);
    assert_refused_without_effect(&h2, &s2, "manifest_rejected").await;
}

#[tokio::test]
async fn development_mode_with_no_manifest_refuses_and_spawns_nothing() {
    // Turning off signatures does not turn off the capability contract, so a build
    // with no manifest at all does not run.
    let h = Harness::build();
    let dev = QuarryVerificationConfig {
        enabled: false,
        allow_writable_binary: true,
        ..QuarryVerificationConfig::default()
    };
    let s = h.supervisor(dev);
    assert_refused_without_effect(&h, &s, "manifest_unparseable").await;
}

#[tokio::test]
async fn a_verified_sidecar_does_not_satisfy_the_verified_path() {
    // A manifest file sitting beside the binary must not stand in for signed
    // material. An unsigned manifest is a manifest an attacker writes.
    let h = Harness::build();
    h.write_good_manifest();
    let s = h.supervisor(identity_config());
    assert_refused_without_effect(&h, &s, "mechanism_unavailable").await;
}

// ── The sender never learns the host's configuration ──────────────────────────

#[tokio::test]
async fn the_sender_facing_message_names_no_path_digest_or_identity() {
    let h = Harness::build();
    let s = h
        .supervisor(identity_config())
        .with_verifier(Arc::new(Refusing(VerificationRefusal::WrongIdentity {
            digest: "deadbeefcafe".into(),
            expected: "quarry-release-workflow".into(),
            found: "attacker-identity".into(),
        })));
    let err = s.run(request(), None, None).await.unwrap_err();

    let sender = err.sender_message();
    for leak in [
        "deadbeefcafe",
        "quarry-release-workflow",
        "attacker-identity",
        "cosign",
        "verification",
        &h.binary.display().to_string(),
    ] {
        assert!(
            !sender.contains(leak),
            "sender message leaks '{leak}': {sender}"
        );
    }
    // And the operator form does name them, so the refusal is diagnosable.
    let operator = err.to_string();
    assert!(operator.contains("attacker-identity"), "{operator}");
}
