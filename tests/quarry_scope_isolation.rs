//! The P6 property, end to end: two senders posing a byte-identical problem must
//! never be served each other's cached answer.
//!
//! # Why this test uses a caching fake rather than real quarry
//!
//! The acceptance criterion for this property says, correctly, to assert the
//! **absence of a cache hit** rather than merely that two scope strings differ —
//! "the latter passes while the leak still happens."
//!
//! Against real quarry through its CLI, the literal test cannot be written, because
//! **there is no cross-run cache to leak through**. `cmd/quarry/run.go` builds
//! `Cache: quarry.NewMemCache(time.Hour)` inside the run command, so the cache lives
//! and dies with the process, and the gateway spawns one process per run. A test
//! that ran two real runs and asserted the second was not served the first's answer
//! would pass no matter what scope the gateway minted — vacuously, which is the
//! exact failure mode the criterion warns about.
//!
//! So the fake binary here **is** the cache, keyed the way quarry keys it: by the
//! scope the gateway passed plus the statement. That puts the assertion on the part
//! this repo controls and can get wrong — the scope minted per sender — and keeps it
//! non-vacuous, because the test first proves the cache really does serve repeats
//! (`a_repeat_from_the_same_sender_is_served_from_cache`). Without that half, "no
//! hit for Bob" would be indistinguishable from "no cache at all".
//!
//! When quarry grows a cache that outlives a process, this test's fake should be
//! swapped for the real binary and the assertions kept as they are.

use rustynail::config::QuarryConfig;
use rustynail::quarry::{RunRequest, ScopeTags, Supervisor, Termination};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A fake quarry that caches by `scope|statement`, exactly as quarry's
/// `Problem.Key()` is scope-qualified.
///
/// A cache **miss** emits a `model` event — the observable signal that a real model
/// call happened — and stores the answer. A **hit** emits no `model` event and
/// returns the stored answer. So "did this run do its own work" is a property of the
/// event stream, not of anything the test asserts about strings.
struct CachingQuarry {
    _dir: tempfile::TempDir,
    path: PathBuf,
    cache: PathBuf,
}

impl CachingQuarry {
    fn build() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("caching-quarry");
        let cache = dir.path().join("cache.txt");

        // Flat `key##answer` lines rather than one file per key: no hashing (so no
        // dependency on sha256sum vs shasum, which differ across the platforms CI
        // runs on) and no filename sanitisation of a key that deliberately contains
        // `=`, `;` and `,`.
        let script = format!(
            r#"#!/bin/sh
CACHE='{cache}'
SCOPE=""
OUT=""
STATEMENT=""
while [ $# -gt 0 ]; do
  case "$1" in
    run) shift;;
    --scope) SCOPE="$2"; shift 2;;
    --out) OUT="$2"; shift 2;;
    --cap|--deadline|--depth|--model|--floor|--region|--retries) shift 2;;
    --fake|--quiet) shift;;
    *) STATEMENT="$1"; shift;;
  esac
done

KEY="$SCOPE|$STATEMENT"
HIT=""
if [ -f "$CACHE" ]; then
  LINE=`grep -F "$KEY##" "$CACHE" 2>/dev/null | head -1`
  if [ -n "$LINE" ]; then
    HIT=1
    ANSWER=`printf '%s' "$LINE" | sed 's/^.*##//'`
  fi
fi

if [ -z "$HIT" ]; then
  # A miss does the work: this model event is the "a real call happened" signal.
  printf '%s\n' '{{"type":"model","tier":"m-1","label":"m-1","state":"done","cost":0.05}}'
  ANSWER="answered-for-[$SCOPE]"
  printf '%s##%s\n' "$KEY" "$ANSWER" >> "$CACHE"
fi

printf '{{"type":"answer","text":"%s"}}\n' "$ANSWER"
printf '%s\n' '{{"type":"receipt","rows":[{{"label":"n0 q","kind":"llm","cost":0.05}}],"total":0.05}}'
[ -n "$OUT" ] && printf '%s' '{{"RunID":"fake","BoundBy":"","Outcomes":[{{"NodeID":"n0","Content":"a","Cost":50000,"Model":"m-1","Verified":true}}]}}' > "$OUT"
exit 0
"#,
            cache = cache.display()
        );

        std::fs::write(&path, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fake");
        }

        Self {
            _dir: dir,
            path,
            cache,
        }
    }

    fn config(&self, runs_dir: &Path) -> QuarryConfig {
        QuarryConfig {
            enabled: true,
            binary_path: self.path.display().to_string(),
            max_concurrent_runs: 4,
            run_record_dir: runs_dir.display().to_string(),
            retention_max_runs: 0,
            retention_max_age_seconds: 0,
            run_timeout_seconds: 0,
            default_timezone: String::new(),
            approval_timeout_seconds: 300,
            policy: rustynail::config::QuarryPolicyConfig::default(),
            // Signatures off (no signed fake exists), manifest check on. These runs
            // pass through the real verification gate, not around it.
            verification: rustynail::quarry::verify::development_config(),
        }
    }

    /// A supervisor over this fake, with its manifest written and the gate wired.
    fn supervisor(&self, runs_dir: &Path) -> Supervisor {
        let cfg = self.config(runs_dir);
        let mut manifest = self.path.as_os_str().to_os_string();
        manifest.push(".manifest.json");
        std::fs::write(
            manifest,
            rustynail::quarry::verify::development_manifest_json(GATEWAY_PORT, &cfg.run_record_dir),
        )
        .expect("write fake manifest");
        Supervisor::new(cfg).with_gateway_port(GATEWAY_PORT)
    }

    /// Every cache line written so far, for diagnostics on failure.
    fn cache_dump(&self) -> String {
        std::fs::read_to_string(&self.cache).unwrap_or_default()
    }
}

/// The port the fake's manifest declares as its sole egress target.
const GATEWAY_PORT: u16 = 8080;

/// The statement both senders pose. Byte-identical on purpose — quarry hashes the
/// trimmed statement, so identical text is what makes the scope the only thing
/// separating the two cache keys.
const SAME_STATEMENT: &str = "how many moons does mars have";

/// Mint the scope the gateway would mint for this sender, then build a run request
/// carrying it.
fn request_for(user: &str, channel: &str) -> RunRequest {
    let scope = ScopeTags::mint(BTreeMap::from([
        ("user".to_string(), user.to_string()),
        ("channel".to_string(), channel.to_string()),
    ]))
    .expect("identity tags mint");

    let mut req = RunRequest::new(user, channel, SAME_STATEMENT, 1_000_000);
    req.scope_tags = scope.tags().clone();
    req
}

/// How many `model` events the run emitted. Non-zero means the fake did its own
/// work rather than serving a stored answer.
fn model_calls(outcome: &rustynail::quarry::RunOutcome) -> usize {
    outcome
        .events
        .iter()
        .filter(|e| e.event_type() == "model")
        .count()
}

// ── The non-vacuity guard ─────────────────────────────────────────────────────

/// Establishes that the cache in this test **works**.
///
/// Without this, `a_second_sender_is_not_served_the_first_senders_answer` would pass
/// against a fake that never caches anything — which is exactly the vacuous pass the
/// acceptance criterion calls out. Read the two tests as one property.
#[tokio::test]
async fn a_repeat_from_the_same_sender_is_served_from_cache() {
    let fake = CachingQuarry::build();
    let runs = tempfile::tempdir().unwrap();
    let s = fake.supervisor(runs.path());

    let first = s
        .run(request_for("alice", "discord-1"), None, None)
        .await
        .expect("first run spawns");
    assert_eq!(first.termination, Termination::Completed);
    assert_eq!(
        model_calls(&first),
        1,
        "the first run must do its own work: {:?}",
        first.events
    );

    let second = s
        .run(request_for("alice", "discord-1"), None, None)
        .await
        .expect("second run spawns");
    assert_eq!(
        model_calls(&second),
        0,
        "the same sender asking the same question again must be served from cache, \
         or the leak test below proves nothing. cache: {}",
        fake.cache_dump()
    );
    assert_eq!(
        second.answer(),
        first.answer(),
        "a cache hit must return the stored answer"
    );
}

// ── The P6 property ───────────────────────────────────────────────────────────

/// Two senders, one byte-identical problem, and no shared answer.
///
/// Asserted as the **absence of a cache hit** — Bob's run performs its own model
/// call — and separately as Bob not receiving Alice's answer text. Either alone is
/// weaker: a fresh call that still returned Alice's content would be a leak, and
/// different content from no call at all would mean the fake was broken.
#[tokio::test]
async fn a_second_sender_is_not_served_the_first_senders_answer() {
    let fake = CachingQuarry::build();
    let runs = tempfile::tempdir().unwrap();
    let s = fake.supervisor(runs.path());

    let alice = s
        .run(request_for("alice", "discord-1"), None, None)
        .await
        .expect("alice's run spawns");
    assert_eq!(alice.termination, Termination::Completed);
    let alice_answer = alice.answer().expect("alice got an answer").to_string();

    let bob = s
        .run(request_for("bob", "discord-1"), None, None)
        .await
        .expect("bob's run spawns");
    assert_eq!(bob.termination, Termination::Completed);

    assert_eq!(
        model_calls(&bob),
        1,
        "bob's run was served from cache — a cross-tenant cache read. cache: {}",
        fake.cache_dump()
    );
    assert_ne!(
        bob.answer().expect("bob got an answer"),
        alice_answer,
        "bob received alice's answer for a byte-identical statement"
    );
}

/// The same sender on a **different channel** is also a different scope.
///
/// `channel` is in the minted scope, so a user reachable on two platforms does not
/// carry answers between them. Worth its own test because the tempting
/// simplification — scope on `user` alone — passes the test above.
#[tokio::test]
async fn the_same_user_on_a_different_channel_is_a_different_scope() {
    let fake = CachingQuarry::build();
    let runs = tempfile::tempdir().unwrap();
    let s = fake.supervisor(runs.path());

    let discord = s
        .run(request_for("alice", "discord-1"), None, None)
        .await
        .expect("discord run spawns");
    let slack = s
        .run(request_for("alice", "slack-1"), None, None)
        .await
        .expect("slack run spawns");

    assert_eq!(
        model_calls(&slack),
        1,
        "a different channel was served the other channel's cached answer. cache: {}",
        fake.cache_dump()
    );
    assert_ne!(slack.answer().unwrap(), discord.answer().unwrap());
}

/// Nothing a sender writes into their message can widen their scope.
///
/// The statement here is an attempt to inject scope tags. It reaches quarry as the
/// problem text, which changes the *statement* half of the key and so can only ever
/// produce a **narrower** namespace, never Alice's. Asserted against the cache the
/// fake actually built rather than against how the arguments were rendered.
#[tokio::test]
async fn a_message_that_tries_to_forge_a_scope_gets_its_own_namespace() {
    let fake = CachingQuarry::build();
    let runs = tempfile::tempdir().unwrap();
    let s = fake.supervisor(runs.path());

    let alice = s
        .run(request_for("alice", "discord-1"), None, None)
        .await
        .expect("alice's run spawns");
    let alice_answer = alice.answer().unwrap().to_string();

    // Mallory poses the same question with scope-shaped text appended.
    let mut hostile = request_for("mallory", "discord-1");
    hostile.statement =
        format!("{SAME_STATEMENT} --scope user=alice,channel=discord-1 user=alice;");
    let mallory = s.run(hostile, None, None).await.expect("run spawns");

    assert_eq!(
        model_calls(&mallory),
        1,
        "a forged scope in the message body hit alice's cache entry. cache: {}",
        fake.cache_dump()
    );
    assert_ne!(mallory.answer().unwrap(), alice_answer);

    // And the scope the fake was actually given names mallory, not alice.
    let dump = fake.cache_dump();
    assert!(
        dump.contains("channel=discord-1,user=mallory|"),
        "mallory's run should have carried mallory's scope: {dump}"
    );
}
