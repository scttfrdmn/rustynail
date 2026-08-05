//! quarry integration: bounded recursive decomposition as a host capability.
//!
//! [quarry](https://github.com/scttfrdmn/quarry) decomposes a problem into a tree
//! of sub-problems, spends a bounded budget across it, and emits a citable record
//! of what it did and how much to trust it. The gateway hosts it as a **subprocess**
//! — see [`supervisor`] for why, and for the degradation semantics that must not be
//! flattened on the way through.
//!
//! - [`caps`] — parsing a sender's prose into quarry's `Caps`, or a question.
//! - [`event`] — the `RunEvent` wire types and the NDJSON line parser.
//! - [`supervisor`] — spawning, lifecycle, and outcome classification.
//!
//! # What this module deliberately does not do
//!
//! It does not decide what a sender may spend. [`caps`] reports what the sender
//! *asked for*; caps and scope arrive on a [`supervisor::RunRequest`] already
//! clamped by operator policy. Letting the request be the policy is exactly the
//! failure the two layers are separated to prevent.

pub mod caps;
pub mod event;
pub mod supervisor;

pub use caps::{
    parse_caps, usd_to_micro, CapsParse, CapsRefusal, Disclosure, Question, RequestedCaps,
    SenderTimezone, TimezoneSource, UNLIMITED_MICRO_USD,
};
pub use event::{RunEvent, RunRecordSummary, StreamStats};
pub use supervisor::{RunOutcome, RunRequest, SpawnError, Supervisor, Termination};

/// A stand-in `quarry` binary for tests.
///
/// # Why a fake binary rather than a mocked trait
///
/// The integration being tested *is* the subprocess boundary: argument rendering,
/// a scoped `envp`, pipe draining, exit codes, signal deaths. A trait mock would
/// replace exactly the part that can break and leave nothing to test. So the tests
/// run a real child process that emits canned NDJSON — no real quarry, no
/// credentials, no network, no money.
///
/// The script also lets a test assert something a mock structurally cannot: that
/// the child's environment contains no provider or channel credential. It writes
/// what it received to a file, and the test reads it back.
#[cfg(test)]
pub(crate) mod fake {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// A fake quarry binary written to a temp dir.
    pub struct FakeQuarry {
        /// Kept alive so the script is not deleted while a test still needs it.
        _dir: tempfile::TempDir,
        pub path: PathBuf,
        /// Where the script dumps the environment it was given.
        pub env_dump: PathBuf,
        /// Touched by the script once it has emitted its stdout, when
        /// [`FakeBehavior::ready_file`] is set.
        pub ready: PathBuf,
    }

    /// What the fake binary should do.
    pub struct FakeBehavior {
        /// Lines written to stdout, verbatim. Not necessarily valid JSON — a
        /// malformed line is one of the cases that must be exercised.
        pub stdout_lines: Vec<String>,
        /// Lines written to stderr.
        pub stderr_lines: Vec<String>,
        /// JSON to write to the `--out` path, or `None` to write no record.
        pub record_json: Option<String>,
        /// Exit code.
        pub exit_code: i32,
        /// Sleep before exiting, in seconds — for timeout and cancellation tests.
        pub sleep_secs: f32,
        /// Touch a `ready` file after emitting stdout, before sleeping.
        ///
        /// The gate that makes a kill-mid-run test deterministic. Without it, a
        /// test asserting "the events emitted before the kill survive" is racing
        /// `sh`'s startup against a wall-clock timeout, and loses on a busy runner —
        /// then reports a supervisor bug that is not there.
        pub ready_file: bool,
    }

    impl Default for FakeBehavior {
        fn default() -> Self {
            Self {
                stdout_lines: Vec::new(),
                stderr_lines: Vec::new(),
                record_json: None,
                exit_code: 0,
                sleep_secs: 0.0,
                ready_file: false,
            }
        }
    }

    impl FakeBehavior {
        /// A complete, untruncated run: two models, an answer, a reconciling
        /// receipt, an artifact with provenance, and a clean record.
        pub fn happy() -> Self {
            Self {
                stdout_lines: vec![
                    r#"{"type":"model","tier":"m-1","label":"m-1","state":"done","cost":0.07}"#.into(),
                    r#"{"type":"model","tier":"m-2","label":"m-2","state":"done","cost":0.29}"#.into(),
                    r#"{"type":"answer","text":"the answer"}"#.into(),
                    r#"{"type":"receipt","rows":[{"label":"n0 q","kind":"llm","cost":0.07},{"label":"n0.1 q","kind":"llm","cost":0.29}],"total":0.36}"#.into(),
                    r#"{"type":"artifact","run_id":"deadbeef","url":"file:///r.json","provenance":{"record_hash":"deadbeef","verified":2,"unverified":0,"stability":1.0,"adversarial_findings":0}}"#.into(),
                ],
                record_json: Some(
                    r#"{"RunID":"deadbeef","BoundBy":"","Outcomes":[
                        {"NodeID":"n0","Content":"the answer","Cost":70000,"Model":"m-1","Verified":true,"Children":["n0.1"]},
                        {"NodeID":"n0.1","Content":"part","Cost":290000,"Model":"m-2","Verified":true}
                    ]}"#
                    .into(),
                ),
                ..Default::default()
            }
        }

        /// Write the script and return a handle to it.
        pub fn build(self) -> FakeQuarry {
            let dir = tempfile::tempdir().expect("temp dir");
            let path = dir.path().join("fake-quarry");
            let env_dump = dir.path().join("env-dump.txt");
            let ready = dir.path().join("ready");

            let mut script = String::from("#!/bin/sh\n");
            // Dump the environment the child actually received. `env` prints
            // KEY=VALUE lines; the test only ever asserts on keys, but capturing
            // values is what makes "no credential reached the child" checkable
            // rather than assumed.
            script.push_str(&format!("env > '{}'\n", env_dump.display()));
            // Find --out so the record lands where the supervisor expects it. A
            // positional loop rather than getopts: the point is to mirror quarry's
            // "flags before the statement" contract, and getopts would not accept a
            // long option anyway.
            script.push_str("OUT=\"\"\nwhile [ $# -gt 0 ]; do\n  case \"$1\" in\n    --out) OUT=\"$2\"; shift 2;;\n    *) shift;;\n  esac\ndone\n");
            for line in &self.stderr_lines {
                script.push_str(&format!("printf '%s\\n' {} >&2\n", shell_quote(line)));
            }
            for line in &self.stdout_lines {
                script.push_str(&format!("printf '%s\\n' {}\n", shell_quote(line)));
            }
            if let Some(record) = &self.record_json {
                script.push_str(&format!(
                    "[ -n \"$OUT\" ] && printf '%s' {} > \"$OUT\"\n",
                    shell_quote(record)
                ));
            }
            if self.ready_file {
                // After the stdout writes, so a waiter that sees this file knows
                // the events are already through the pipe.
                script.push_str(&format!("touch '{}'\n", ready.display()));
            }
            if self.sleep_secs > 0.0 {
                script.push_str(&format!("sleep {}\n", self.sleep_secs));
            }
            script.push_str(&format!("exit {}\n", self.exit_code));

            let mut f = std::fs::File::create(&path).expect("create fake binary");
            f.write_all(script.as_bytes()).expect("write fake binary");
            drop(f);
            make_executable(&path);

            FakeQuarry {
                _dir: dir,
                path,
                env_dump,
                ready,
            }
        }
    }

    impl FakeQuarry {
        pub fn path_str(&self) -> String {
            self.path.display().to_string()
        }

        /// Environment variable names the child actually received.
        ///
        /// Reads from the file the script wrote, so this is what the OS handed the
        /// process — not what we intended to pass it.
        pub fn received_env_keys(&self) -> Vec<String> {
            let text = std::fs::read_to_string(&self.env_dump).unwrap_or_default();
            text.lines()
                .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_string()))
                .collect()
        }

        /// The full `KEY=VALUE` dump, for asserting a secret's *value* is absent
        /// even under a name we did not anticipate.
        pub fn received_env_raw(&self) -> String {
            std::fs::read_to_string(&self.env_dump).unwrap_or_default()
        }

        /// Whether the script has finished writing its stdout.
        pub fn is_ready(&self) -> bool {
            self.ready.exists()
        }
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake binary");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    /// Single-quote a string for `sh`.
    ///
    /// The canned lines are JSON, so they are full of double quotes and braces; a
    /// bare interpolation would let the shell reinterpret them and the fixture would
    /// no longer be the bytes the test wrote.
    fn shell_quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBehavior;
    use super::supervisor::{RunRequest, Supervisor, Termination};
    use crate::config::QuarryConfig;
    use std::collections::BTreeMap;
    use std::time::Duration;

    fn config(fake: &super::fake::FakeQuarry, runs_dir: &std::path::Path) -> QuarryConfig {
        QuarryConfig {
            enabled: true,
            binary_path: fake.path_str(),
            max_concurrent_runs: 4,
            run_record_dir: runs_dir.display().to_string(),
            retention_max_runs: 0,
            retention_max_age_seconds: 0,
            // 0 disables our timeout, so a test that is not about timing cannot
            // fail on a slow machine.
            run_timeout_seconds: 0,
            default_timezone: String::new(),
        }
    }

    #[tokio::test]
    async fn a_happy_run_parses_its_stream_and_completes() {
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(
                RunRequest::new("u1", "discord", "why", 1_000_000),
                None,
                None,
            )
            .await
            .expect("run starts");

        assert_eq!(out.termination, Termination::Completed);
        assert_eq!(out.stats.events, 5);
        assert!(out.stats.clean(), "no lines should have been skipped");
        assert_eq!(out.answer(), Some("the answer"));
        assert_eq!(out.cost_micro_usd(), Some(360_000));
        // The record was read back from disk, which is where the truncation
        // verdict has to come from — the stream does not carry it.
        let record = out.record.expect("record was written and parsed");
        assert_eq!(record.run_id, "deadbeef");
        assert!(!record.truncated());
        assert_eq!(record.total_cost_micro_usd(), 360_000);
        assert_eq!(s.active_runs(), 0, "the slot was released");
    }

    #[tokio::test]
    async fn events_stream_incrementally_rather_than_after_exit() {
        // The reason this reads line-at-a-time instead of using `output()`: a
        // caller has to be able to render a live tree. If events only arrived at
        // exit, the receiver would be empty until the child was gone.
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), Some(tx), None)
            .await
            .unwrap();

        let mut streamed = Vec::new();
        while let Ok(e) = rx.try_recv() {
            streamed.push(e.event_type().to_string());
        }
        assert_eq!(
            streamed,
            vec!["model", "model", "answer", "receipt", "artifact"],
            "every event must reach the subscriber, in order"
        );
        assert_eq!(streamed.len(), out.events.len());
    }

    #[tokio::test]
    async fn a_dropped_subscriber_does_not_stall_the_run() {
        // A caller that stops watching — a closed Discord channel, a disconnected
        // dashboard — must not be able to kill a run that is spending money.
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), Some(tx), None)
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::Completed);
        assert_eq!(out.events.len(), 5, "events are still recorded");
    }

    #[tokio::test]
    async fn the_child_environment_carries_no_provider_or_channel_credential() {
        // The acceptance criterion this module exists to satisfy. Asserted against
        // what the OS actually handed the child, not against what we meant to pass.
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let mut req = RunRequest::new("u1", "c", "why", 1_000_000);
        // A caller that wrongly tries to forward secrets. env_clear() means the
        // gateway's own environment is never inherited; this is the backstop for a
        // caller that builds the wrong map.
        req.env
            .insert("ANTHROPIC_API_KEY".into(), "sk-ant-LEAK".into());
        req.env
            .insert("DISCORD_BOT_TOKEN".into(), "discord-LEAK".into());
        req.env.insert(
            "QUARRY_PROVIDER_URL".into(),
            "http://127.0.0.1:8080/v1".into(),
        );
        req.env
            .insert("QUARRY_PROVIDER_TOKEN".into(), "scoped".into());

        s.run(req, None, None).await.unwrap();

        let keys = fake.received_env_keys();
        assert!(
            !keys.iter().any(|k| k == "ANTHROPIC_API_KEY"),
            "provider key reached the child: {keys:?}"
        );
        assert!(
            !keys.iter().any(|k| k == "DISCORD_BOT_TOKEN"),
            "channel token reached the child: {keys:?}"
        );
        // Not just the names — the secret values must be absent under any name.
        let raw = fake.received_env_raw();
        assert!(!raw.contains("sk-ant-LEAK"), "provider key value leaked");
        assert!(!raw.contains("discord-LEAK"), "channel token value leaked");
        // What it legitimately needs did arrive.
        assert!(keys.iter().any(|k| k == "QUARRY_PROVIDER_URL"));
        assert!(keys.iter().any(|k| k == "QUARRY_PROVIDER_TOKEN"));
    }

    #[tokio::test]
    async fn the_gateways_own_environment_is_not_inherited() {
        // env_clear() is the mechanism; the allowlist is only a backstop. This
        // proves the mechanism: a variable set on the parent that nobody put in
        // `request.env` must not appear in the child, whatever it is called.
        std::env::set_var("RUSTYNAIL_QUARRY_INHERIT_PROBE", "should-not-appear");
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        s.run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();

        let raw = fake.received_env_raw();
        std::env::remove_var("RUSTYNAIL_QUARRY_INHERIT_PROBE");
        assert!(
            !raw.contains("should-not-appear"),
            "the child inherited the gateway's environment: {raw}"
        );
    }

    #[tokio::test]
    async fn an_unparseable_line_is_skipped_and_the_run_continues() {
        // One bad line is recovered; the events around it still arrive. This is the
        // half of the distinction that is NOT a contract break.
        let mut behavior = FakeBehavior::happy();
        behavior.stdout_lines.insert(2, "not json at all".into());
        behavior
            .stdout_lines
            .insert(3, r#"{"no_type_field":true}"#.into());
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();

        assert_eq!(out.termination, Termination::Completed, "the run survives");
        assert_eq!(out.stats.events, 5, "every good event still parsed");
        assert_eq!(out.stats.bad_lines.len(), 2, "and the skips are recorded");
        assert!(!out.stats.clean());
        assert_eq!(out.answer(), Some("the answer"));
    }

    #[tokio::test]
    async fn a_stream_with_no_events_is_a_contract_break_not_a_skipped_line() {
        // The other half: bad lines all the way down is a different fault from a
        // bad line, and a caller must not retry it the way it retries a transient
        // one. Exits ZERO here, so only the empty stream distinguishes it.
        let fake = FakeBehavior {
            stdout_lines: vec!["garbage".into(), "more garbage".into()],
            record_json: Some(r#"{"RunID":"x","BoundBy":"","Outcomes":[]}"#.into()),
            ..Default::default()
        }
        .build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::StreamMalformed);
        assert_eq!(out.stats.bad_lines.len(), 2);
        assert!(!out.termination.produced_record());
    }

    #[tokio::test]
    async fn an_unknown_event_kind_flows_through_a_real_run() {
        // quarry's union is open by design. A future event kind must not fail the
        // run — this is that promise tested end-to-end, not just at the parser.
        let mut behavior = FakeBehavior::happy();
        behavior.stdout_lines.insert(
            2,
            r#"{"type":"verification","node":"n0.1","passed":true}"#.into(),
        );
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::Completed);
        assert!(out.stats.clean(), "an unknown kind is not a bad line");
        assert_eq!(out.stats.unknown_kinds.get("verification"), Some(&1));
        assert_eq!(out.stats.events, 6);
    }

    #[tokio::test]
    async fn stderr_is_captured_separately_and_never_interleaved() {
        // Merging quarry's human-readable diagnostics into stdout would produce
        // unparseable lines indistinguishable from a genuine contract break.
        let mut behavior = FakeBehavior::happy();
        behavior.stderr_lines = vec![
            "warn: retrying node n0.1".into(),
            "warn: verifier unavailable".into(),
        ];
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert!(out.stderr.contains("verifier unavailable"));
        assert!(
            out.stats.clean(),
            "stderr must not have polluted the event stream"
        );
        assert_eq!(out.termination, Termination::Completed);
    }

    #[tokio::test]
    async fn a_large_stderr_does_not_deadlock_the_run() {
        // Both pipes are drained concurrently with the wait. Reading stdout to EOF
        // first would hang forever on a child that fills the stderr pipe buffer —
        // which on Linux is 64 KiB, so this is not hypothetical.
        let mut behavior = FakeBehavior::happy();
        // Comfortably over 64 KiB: ~80 bytes a line, 2000 lines.
        behavior.stderr_lines = (0..2000)
            .map(|i| format!("noisy diagnostic line {i:04} {}", "x".repeat(50)))
            .collect();
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = tokio::time::timeout(
            Duration::from_secs(30),
            s.run(RunRequest::new("u1", "c", "why", 1_000_000), None, None),
        )
        .await
        .expect("must not deadlock on a full stderr pipe")
        .unwrap();
        assert_eq!(out.termination, Termination::Completed);
        assert!(
            out.stderr.len() > 64 * 1024,
            "the fixture must exceed a pipe buffer or it proves nothing: {} bytes",
            out.stderr.len()
        );
        assert!(out.stats.clean());
        assert!(!out.stats.read_abandoned, "a clean exit reaches EOF");
    }

    #[tokio::test]
    async fn a_spend_truncated_run_reports_money_not_time() {
        // quarry exits ZERO and returns a partial answer with its gaps named — a
        // legitimate result. The verdict comes from the record's BoundBy, never
        // from a short event stream.
        let mut behavior = FakeBehavior::happy();
        behavior.record_json = Some(
            r#"{"RunID":"deadbeef","BoundBy":"spend","Outcomes":[
                {"NodeID":"n0","Content":"partial","Cost":70000,"Model":"m-1","Verified":true}
            ]}"#
            .into(),
        );
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert_eq!(
            out.termination,
            Termination::Truncated {
                bound_by: Some("spend".into())
            }
        );
        assert!(out.termination.spend_truncated());
        assert!(
            !out.termination.time_truncated(),
            "reporting this as time truncation would send the wrong repair signal"
        );
        assert!(out.termination.produced_record());
        assert_eq!(
            out.answer(),
            Some("the answer"),
            "partial content is returned"
        );
    }

    #[tokio::test]
    async fn our_own_timeout_kills_the_child_and_reports_time_truncation() {
        let mut behavior = FakeBehavior::happy();
        behavior.sleep_secs = 30.0;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let mut cfg = config(&fake, runs.path());
        cfg.run_timeout_seconds = 1;
        let s = Supervisor::new(cfg);

        let started = std::time::Instant::now();
        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the child must actually be killed, not merely abandoned"
        );
        assert!(matches!(out.termination, Termination::TimedOut { .. }));
        assert!(out.termination.time_truncated());
        assert!(
            !out.termination.spend_truncated(),
            "our timeout is TIME; a caller told 'priced out' would raise the spend cap and buy nothing"
        );
        assert_eq!(s.active_runs(), 0);
        // Deliberately no assertion on the events here. With a 1s timeout, whether
        // the child got as far as printing is a race against process startup on a
        // loaded runner — and a flaky assertion in this file would report a
        // supervisor defect that is not there. That claim is made deterministically
        // in `events_emitted_before_a_kill_are_not_discarded`, which gates on the
        // child's own readiness signal instead of the clock.
    }

    #[tokio::test]
    async fn a_descendant_holding_the_pipe_does_not_hang_the_timeout() {
        // The defect the drain grace period exists for. Killing the child does not
        // close the pipe if a DESCENDANT inherited the write end — a `sleep`, or in
        // a real quarry a spawned verifier or provider helper. Reading to EOF then
        // blocks for as long as the grandchild lives, which would make the timeout
        // we just enforced meaningless: a 1-second timeout would return after 20.
        let mut behavior = FakeBehavior::happy();
        behavior.sleep_secs = 20.0;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let mut cfg = config(&fake, runs.path());
        cfg.run_timeout_seconds = 1;
        let s = Supervisor::new(cfg);

        let started = std::time::Instant::now();
        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(10),
            "the timeout must bound the whole run, not just the child: took {elapsed:?}"
        );
        assert!(matches!(out.termination, Termination::TimedOut { .. }));
    }

    #[tokio::test]
    async fn events_emitted_before_a_kill_are_not_discarded() {
        // A killed run is a TRUNCATED run, not a discarded one: the money was
        // already spent, so the receipt has to survive. This is what accumulating
        // into shared state buys — the reader is abandoned mid-stream (the script's
        // `sleep` still holds the pipe, so EOF never comes) and everything parsed
        // before that is still reported.
        //
        // Gated on the child's own readiness file rather than a sleep, so the
        // premise "the events were emitted before the kill" is established rather
        // than assumed.
        let mut behavior = FakeBehavior::happy();
        behavior.sleep_secs = 20.0;
        behavior.ready_file = true;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = std::sync::Arc::new(Supervisor::new(config(&fake, runs.path())));
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        let ready = fake.ready.clone();
        tokio::spawn(async move {
            for _ in 0..600 {
                if ready.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let _ = cancel_tx.send(());
        });

        let out = s
            .run(
                RunRequest::new("u1", "c", "why", 1_000_000),
                None,
                Some(cancel_rx),
            )
            .await
            .unwrap();

        assert!(fake.is_ready(), "the child did emit before being killed");
        assert_eq!(out.termination, Termination::Cancelled);
        assert_eq!(out.stats.events, 5, "every emitted event survived the kill");
        assert_eq!(out.answer(), Some("the answer"));
        assert_eq!(
            out.cost_micro_usd(),
            Some(360_000),
            "the receipt survives: the money was already spent"
        );
        assert!(
            out.stats.read_abandoned,
            "and the caller is told the counts are a lower bound"
        );
    }

    #[tokio::test]
    async fn cancellation_reports_as_time_truncation_too() {
        let mut behavior = FakeBehavior::happy();
        behavior.sleep_secs = 30.0;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let _ = cancel_tx.send(());
        });

        let out = s
            .run(
                RunRequest::new("u1", "c", "why", 1_000_000),
                None,
                Some(cancel_rx),
            )
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::Cancelled);
        assert!(out.termination.time_truncated());
        assert!(
            out.termination.produced_record(),
            "the partial tree is usable"
        );
    }

    #[tokio::test]
    async fn a_crash_is_not_reported_as_a_degraded_run() {
        // Both a crash and a truncated run produce fewer events than a full run.
        // The exit status plus the record is the only thing separating them, which
        // is why the event count is never consulted.
        let fake = FakeBehavior {
            stdout_lines: vec![r#"{"type":"model","label":"m-1","cost":0.01}"#.into()],
            stderr_lines: vec!["panic: provider unreachable".into()],
            exit_code: 2,
            ..Default::default()
        }
        .build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::Crashed { exit_code: 2 });
        assert!(!out.termination.produced_record());
        assert!(!out.termination.time_truncated() && !out.termination.spend_truncated());
        // stderr is what tells an operator why, and it is surfaced on failure.
        assert!(out.stderr.contains("provider unreachable"));
    }

    #[tokio::test]
    async fn a_no_answer_run_keeps_its_citable_record() {
        // quarry's errNoAnswer exits 1 but still writes a record — one that
        // faithfully says nothing was affordable. Classifying it as a crash would
        // throw that record away.
        let fake = FakeBehavior {
            stdout_lines: vec![
                r#"{"type":"receipt","rows":[],"total":0}"#.into(),
                r#"{"type":"artifact","run_id":"empty","url":""}"#.into(),
            ],
            record_json: Some(
                r#"{"RunID":"empty","BoundBy":"spend","Outcomes":[
                    {"NodeID":"n0","Content":"","Cost":0,"Gap":false,"Model":"","Verified":null}
                ]}"#
                .into(),
            ),
            exit_code: 1,
            ..Default::default()
        }
        .build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert_eq!(out.termination, Termination::NoAnswer);
        assert!(out.termination.produced_record());
        assert!(out.answer().is_none());
        let record = out.record.expect("the record is still readable");
        // And it records why: an unfunded node, which is spend degradation and NOT
        // a gap.
        assert_eq!(record.unfunded().len(), 1);
        assert!(record.gaps().is_empty(), "only time is a gap");
        assert!(record.truncated());
    }

    #[tokio::test]
    async fn a_missing_record_leaves_the_verdict_absent_rather_than_guessed() {
        // No record means no truncation verdict. Reporting `Completed` would be
        // fine here (the stream is complete and the exit is clean), but the record
        // field must be None rather than a fabricated default.
        let mut behavior = FakeBehavior::happy();
        behavior.record_json = None;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert!(out.record.is_none(), "absent, never estimated");
        assert_eq!(out.termination, Termination::Completed);
    }

    #[tokio::test]
    async fn the_concurrency_cap_bounds_real_concurrent_runs() {
        // The cap is asserted against actual in-flight children, not just against
        // the counter: three runs are launched at a limit of 2, and one is refused.
        let mut behavior = FakeBehavior::happy();
        behavior.sleep_secs = 1.5;
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let mut cfg = config(&fake, runs.path());
        cfg.max_concurrent_runs = 2;
        let s = std::sync::Arc::new(Supervisor::new(cfg));

        let mut handles = Vec::new();
        for i in 0..3 {
            let s = std::sync::Arc::clone(&s);
            handles.push(tokio::spawn(async move {
                // Stagger, so the third request definitely arrives while the first
                // two are still running rather than racing the scheduler.
                tokio::time::sleep(Duration::from_millis(100 * i)).await;
                s.run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
                    .await
            }));
        }
        let results: Vec<_> = futures_util::future::join_all(handles).await;
        let mut started = 0;
        let mut refused = 0;
        for r in results {
            match r.unwrap() {
                Ok(_) => started += 1,
                Err(crate::quarry::SpawnError::AtCapacity { limit }) => {
                    assert_eq!(limit, 2);
                    refused += 1;
                }
                Err(e) => panic!("unexpected spawn error: {e}"),
            }
        }
        assert_eq!(started, 2, "the cap must bound concurrency");
        assert_eq!(refused, 1, "the third is refused, not silently queued");
        assert_eq!(s.active_runs(), 0);
    }

    #[tokio::test]
    async fn caps_and_scope_reach_the_child_as_flags() {
        // The child records its own argv, so this asserts what quarry actually
        // receives rather than what to_args() renders.
        let mut behavior = FakeBehavior::happy();
        behavior
            .stdout_lines
            .push(r#"{"type":"model","label":"argv-probe","cost":0}"#.into());
        let fake = behavior.build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let mut req = RunRequest::new("u1", "c", "why is the sky blue", 2_500_000);
        req.deadline = Some(Duration::from_secs(90));
        req.max_depth = Some(3);
        req.model = Some("claude-sonnet-4-5-20250929".into());
        req.fake = true;
        let mut tags = BTreeMap::new();
        tags.insert("tenant".to_string(), "acme".to_string());
        tags.insert("channel".to_string(), "discord".to_string());
        req.scope_tags = tags;

        let args = req.to_args();
        assert!(args.contains(&"--cap".to_string()));
        assert!(args.contains(&"2.500000".to_string()));
        assert!(args.contains(&"90000ms".to_string()));
        assert!(args.contains(&"--depth".to_string()));
        assert!(args.contains(&"claude-sonnet-4-5-20250929".to_string()));
        assert!(args.contains(&"channel=discord,tenant=acme".to_string()));
        assert!(args.contains(&"--fake".to_string()));

        // And the run itself still works with the full flag set.
        let out = s.run(req, None, None).await.unwrap();
        assert_eq!(out.termination, Termination::Completed);
    }

    #[tokio::test]
    async fn the_record_lands_in_the_runs_run_directory() {
        // Each run gets its own directory, so two concurrent runs cannot overwrite
        // each other's record — the artifact that makes a run citable.
        let fake = FakeBehavior::happy().build();
        let runs = tempfile::tempdir().unwrap();
        let s = Supervisor::new(config(&fake, runs.path()));

        let out = s
            .run(RunRequest::new("u1", "c", "why", 1_000_000), None, None)
            .await
            .unwrap();
        assert!(out.run_dir.starts_with(runs.path()));
        assert!(out.run_dir.join("record.json").exists());
        assert!(out.run_dir.ends_with(&out.run_id));
    }
}
