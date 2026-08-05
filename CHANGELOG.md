# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `src/quarry/verify.rs` — the signed-binary gate every quarry spawn passes through, before the run directory is created and before the child exists. Checks, in order: the binary resolves (a bare name is resolved on `PATH`, so the file that is hashed is the file that is executed); neither it nor its containing directory is writable by the gateway; its sha256; its signature, identity-constrained; and the capability manifest that signature covers. Closes #115.
- **Verification is on by default, and with the cosign mechanism (#103) unimplemented it refuses every spawn.** That is the control working, not a stub: the alternative default is a deployment that silently runs unverified binaries because nobody changed a setting. The refusal names #103 and points at `verification.enabled: false` for development, and a test asserts that specific reason so a future change that makes a spawn succeed *without* a verifier fails the suite rather than passing quietly.
- **Identity-constrained, not signature-present.** A signature that merely exists proves someone signed something. `expected_identity` and `expected_issuer` (cosign's `--certificate-identity-regexp` and `--certificate-oidc-issuer`) are checked for presence *before* a verifier is called, because a verifier handed an empty identity regex may match anything and report success — an unconstrained check is worse than none, since it succeeds. An empty value refuses.
- The capability manifest must declare **exactly** quarry's contract — localhost egress to this gateway's own `http_port`, and the one `run_record_dir` it may write — and nothing else. A third capability, a duplicate of either, a non-loopback host, or an unknown field refuses the spawn. `#[serde(deny_unknown_fields)]` on both manifest structs: a capability carrying a field this reader does not know may mean something to a more permissive one. The kind is a flat string rather than a tagged enum, so an unrecognised kind is refusable **by name** instead of producing "unknown variant".
- **The digest is recomputed on every spawn; only the signature result is cached, keyed by that digest.** An upgraded or swapped binary hashes differently and re-verifies itself, with no cache to invalidate and no time-based expiry — time is the wrong axis, since a binary replaced one second after verification is as unverified as one replaced a week later. The cache stores the *signed manifest text*, not a "this digest is fine" verdict, so the capability check re-runs each spawn against current config: a gateway whose `http_port` or `run_record_dir` changed under it is re-evaluated rather than waved through by an entry that predates the change.
- Writability is asked of the kernel (`faccessat` with `AT_EACCESS`), not computed from mode bits. Mode arithmetic gets ACLs, supplementary groups, read-only mounts and immutable flags wrong, and **every one of those errors reports a writable file as safe**. The non-unix fallback returns "writable", which fails closed.
- A writable *containing directory* is refused as well as a writable binary: it permits the same replace-between-hash-and-exec substitution. The residual window is documented rather than papered over — closing it entirely needs `fexecve` against the descriptor that was hashed, which `tokio::process::Command` cannot express and whose portable `/proc/self/fd` form does not exist on macOS.
- `quarry.verification` config (`QUARRY_VERIFY_ENABLED`, `QUARRY_VERIFY_IDENTITY`, `QUARRY_VERIFY_ISSUER`, `QUARRY_VERIFY_COSIGN_PATH`, `QUARRY_VERIFY_ALLOW_WRITABLE_BINARY`, `QUARRY_VERIFY_MANIFEST_PATH`). `QUARRY_VERIFY_ENABLED` parses as a bool and **anything unparseable leaves verification on** — the opposite reading would let `QUARRY_VERIFY_ENABLED=yes`, which is not a bool, silently disable a security control.
- `enabled: false` warns on **every spawn**, not once at startup: a long-running gateway that warned only at boot is a gateway nobody knows is unverified, and the warning has to appear in the logs an operator is actually reading. It also does **not** relax the manifest check — provenance and sandboxing are separable, and only the first is a development convenience. `allow_writable_binary` warns per-spawn for the same reason: the operator who accepted a risk is often not the one reading the logs a year later.
- Two audiences, two messages. One `sender_message()` constant for all fifteen refusals, naming no path, digest, identity or config key — the sender cannot act on any of it and it maps the host's filesystem. The `Display` form, the `warn!` and the audit entry name exactly which check failed, because an operator hunting a refused run needs precisely what the sender must not see.
- Audit events `quarry_verification_refused` (with a `reason` slug, the detail, and the digest **omitted rather than empty** for checks that fire before hashing — an empty string there would put a lie in the audit log) and `quarry_verification_passed`. `quarry_run_started` now carries `binary_digest` and `signature_checked`, on the *started* event because "what exactly ran" is asked of a specific run, and correlating two events by timestamp is how that question gets answered wrongly.
- `rustynail config validate` reports which of the four verification states a deployment is in, and `config check` summarises it. Both report the **effect on a run** rather than the value of a setting: "verified" and "every run refused" are both `enabled: true`. Whether a verifier exists is asked of a real gate rather than hardcoded, so both start reporting success on their own the day #103 lands instead of going stale. Verification on with no mechanism exits non-zero — a gateway that will refuse every quarry run should not report a clean preflight — while the two deliberate operator choices warn with `[!]` and do not fail.
- `tests/quarry_verification_gate.rs` — 20 integration tests asserting **absence of effect**, not that an `Err` was returned. Each refusal is checked to have started no child (a shim in front of the fake records invocations on disk, since asserting an empty event stream cannot distinguish a refused spawn from a silent one), created no run *directory* (an empty one reads to the retention reaper and to an operator as a run that happened and produced nothing), written no record, and released its concurrency slot. Follows the #90 lesson, where the shell allowlist *filtered* what it should have *rejected*. A non-vacuity guard proves the harness can spawn and does leave a record — without it every assertion in the file would pass against a supervisor that refused unconditionally.
- `docs/deployment.md` gains a quarry deployment section: obtaining a release artifact, verifying it by hand with `cosign verify-blob` at install time, installing it root-owned `0555` in a root-owned directory, installing the manifest, and a table mapping each refusal `reason` to what to fix. The env-var cheat sheet, which claimed to be complete, was missing all sixteen `QUARRY_*` variables.
- `src/quarry/receipt.rs` — the reply a quarry run produces: its answer **and** a receipt stating what it cost, which cap bound it, what was left out, and how much of it replicated. There is no `quarry.receipt.enabled`, no per-channel exemption and no length threshold past which the footer is dropped; an answer with no cost and no trust information attached is exactly the artifact quarry exists to replace. Closes #114.
- `Gateway::deliver_quarry_outcome` is the **only** path a run takes to a sender, precisely so no path can deliver an answer without its receipt. `Receipt::render` emits both together and offers no way to emit just the first. Verified by mutation: rewiring it to send `receipt.answer` alone fails two integration tests.
- **Gaps and planned degradation are reported as two separate, differently-labelled sections**, and their sum appears nowhere. Only *time* produces a gap; a node the budget priced out has empty content and no gap flag, because being priced out was disclosed before spend (P9). The remedies are opposite — "More time would recover these. More money would not." against "A higher spend limit would fund these. More time would not." — and a sender shown one merged "2 nodes incomplete" line raises whichever cap comes to mind, three-quarters of the time buying nothing.
- `BoundBy` is named explicitly with what raising it would buy, and **"no limit was reached" is printed rather than omitted**: quarry emits an empty `bound_by` as a measurement, so leaving it out would make a clean run indistinguishable from a footer that forgot to mention a limit. An unrecognised denomination gets a deliberately vague remedy — guessing one would send the sender to raise the wrong cap, the exact failure naming `BoundBy` prevents.
- **"Not measured" renders in words, never as 0%.** quarry omits the whole `provenance` object when a stability rate is unpublishable, which covers all three of its zero-cases (n=1; a rate of 0 reached with unassessed comparisons; a truncated comparison pass). All three would render as `0.0` and all three would read as "nothing replicated" — silence converted into a finding. A published rate is stated as "at least N%", never "exactly": `StabilityIsFloor`, `Unassessed` and `ComparedBy` are all `json:"-"`, so a floor and a real measurement reach a host as the *same bare float* and the weaker reading is the only one guaranteed true.
- Spend is read from quarry's integer `total_micros`, never summed from the receipt's float rows, and a run that ended without stating a total reports its spend as **unknown rather than zero** — the money still went out. A receipt whose itemised rows do not reconcile says so instead of rendering the numbers as if they agreed.
- The per-model breakdown is **disclosed, not reconciled**. quarry's reduce path assigns `Cost` but never `ModelVersion` (quarry#20), so per-model spend and the total legitimately disagree — on the `live-partition` fixture by 42042 of 80437 micro-units, over half the run. The footer names the residual with its upstream cause; upstream is explicit that a host must *"not close the gap by inventing an untagged row."*
- Every reply carries a citation (P8): quarry's content-hash run ID and its record URL, falling back to the gateway's own run directory when quarry did not make the record addressable.
- Dashboard decomposition tree: a `DashboardEvent::QuarryRun` variant (the WS union stays additive, so existing consumers are unaffected) carrying quarry's own nodes, edges, costs and flags. Every one is read from the `RunRecord` or the terminal event — **no locally-invented event type presented as the shared contract**, which is how two implementations of one integration drift apart while both look correct. Gaps render amber and unfunded blue for the same reason the footer separates them, and `verified: null` renders "not assessed" rather than "refuted": nobody looked is not a finding of wrong.
- `tests/quarry_receipt.rs` (27 tests) renders receipts from quarry's own captured runs and delivers them through the **real** formatter and chunker, including Teams' 1024-byte limit — the tightest of the five platform defaults — where `live-partition`'s 6798-rune answer spans many chunks and the footer is verified to survive whole rather than be dropped to fit.
- `tests/quarry_conformance.rs` and `tests/fixtures/quarry/runevents/` — 19 tests over quarry's own frozen event-stream corpus, 27 files vendored verbatim from its `testdata/runevents/` (source commit `d066040`, stream version 1, producer `quarry-go`). Upstream built the corpus for both twins, naming its consumers as *"bucktooth (Go) and rustynail (Rust)"*. Every other quarry test here asserts our reader against fixtures we wrote, which can only confirm we are self-consistent; these read bytes quarry produced and compare our reading against quarry's own restatement of the same bytes in integers — two independent readings of one wire capture. Vendored rather than regenerated because upstream is explicit that regenerating in CI would be *"worse than none"*: the budget-degraded case is unreachable under `--fake` (uniform per-call cost, so affordability either funds every child or declines the split) and the time-truncated cases are wall-clock races whose shape already changed once on the capturing machine. Determinism is claimed for the fold, which is pure, not for the runs. Closes #116.
- First-class `StreamEvent` and `OutcomeEvent` variants: quarry's stream carries **six** event kinds, not agate's four. `quarry_stream` opens the frame with the contract version; `quarry_outcome` closes it with quarry's own verdict — `outcome`, `bound_by`, `gaps`, `unfunded`, and the money in integer micro-units. Both were previously falling into `RunEvent::Unknown`, discarding the version (which exists so a host can *refuse* a stream) and the terminal event (whose *absence* is the only in-band kill signal).
- A stream declaring `version != 1` is **refused**, not folded (`stream_version_unsupported`). quarry's compatibility rule is that adding an event *kind* is minor and must be tolerated, while changing a field or what a kind means is major and bumps the version — so an unknown version is by definition a change that skipping cannot absorb, and the events we still recognise may no longer mean what we read them to mean.
- A frame that opens and never closes is reported as `stream_incomplete`, **never defaulted to complete**, including on exit 0. NDJSON yields complete lines whether or not the producer finished, so a stream cut off after the artifact event is byte-identical to a clean one except for the missing terminal event. Events read before the cut are kept: they were paid for.
- `usage_error` for exit 2 — quarry refused the invocation. A host defect rather than a run outcome: nothing was attempted, so there is nothing to retry and nothing to cite. Kept out of `crashed` because a crash invites a retry and this one would fail identically forever.
- `ReceiptRow::cost_micro_usd`, so each row converts with `round(× 1e6)` **before** summing. Two corpus cases carry `float_sum_equals_total: false` and exist to fail a host that adds the floats first.
- `src/quarry/approval.rs` and `src/quarry/gate.rs` — the plan gate. Being *allowed* to run under `quarry.policy` no longer starts a run: the gateway sends the sender the plan and **waits**, and only that sender's explicit approval spawns a quarry process. Closes #112.
- **Silence cancels, at zero spend.** There is no `default_approve` setting and no way to write one; a timeout settles as `Decision::Expired`, which `may_spend()` refuses on an exhaustive `match` so a future decision variant is a compile error rather than an accidental consent. Asserted by tests that check the *absence of the side effect* — no process spawned and a receipt reading zero — not that an error was returned, because an error return with the spend already incurred is the failure mode being prevented.
- The gate discloses **the cap, not an estimate**, and says which it is. Under quarry's P4 the cap is the contract — quarry fits its plan to the cap rather than discovering the cost partway through — so the granted spend limit is the ceiling on the bill. No estimate is shown because quarry has no plan-only mode: `main.go` dispatches only `run`/`show`/`replay`, `RunEvents()` folds an already-completed record, and `Probe()` is unwired and by quarry's own description "a real planner call", so producing an estimate would spend money to decide whether to spend money. The absence is printed with its reason rather than rendered as `$0.00`, which a sender would read as a measurement that was never taken.
- The plan states the **granted** caps and names every adjustment the policy made ("you asked for spend $50.0000, policy allows $5.0000 — proceeding with $5.0000"), so a silent clamp cannot be approved as if it were what was requested (P9: disclosure before spend).
- Replies are matched **whole-word**, case-insensitively, after trimming trailing punctuation: `yes`/`y`/`approve`/`approved`/`ok`/`okay`/`go`/`run` and `no`/`n`/`cancel`/`stop`/`abort`/`nope`/`nevermind`. A multi-word reply is never scanned for a keyword — "yes, but cheaper" and "no, wait — yes" each contain an approve word and neither is one — so both re-prompt while the run keeps waiting.
- **A bystander cannot approve.** Pending approvals are keyed `(channel, sender)`, so a `yes` from anyone but the sender who asked is an ordinary message, and two senders in one channel can each hold a plan without answering the other's. A second request from the same sender supersedes the first, cancelling it at zero spend with a message saying so.
- `quarry.approval_timeout_seconds` (`QUARRY_APPROVAL_TIMEOUT_SECONDS`, default `300`). Values below 15 are clamped up and **`0` does not disable the gate** — a zero-second window would cancel every run before its plan message finished sending, presenting as a broken integration rather than as a policy.
- Registration happens **before** the plan is sent, which is why `ApprovalRegistry::begin` returns an `ApprovalTicket` to wait on separately. Sending first would lose a reply arriving in the gap: on a fast channel that is a run which expires despite having been approved, with nothing in the log to explain it. Covered by a test whose responder answers from inside its own `reply` call.
- The plan takes the same formatter and chunker path as an agent reply (the send tail is now the shared `send_text`), because a plan sent around the chunker arrives truncated at Teams' 1024-byte limit — which is how a sender ends up approving a plan whose limits were cut off the bottom.
- Approval replies are exempt from message deduplication. A sender approving two runs in a session sends the *same* one-word reply twice and the dedup ring buffer would drop the second as a repeat, leaving that run to expire unapproved with no visible cause. The exemption is not a bypass: it applies only while that sender has a plan outstanding on that channel, and settling clears it.
- The reply interception runs **before** the rate limiter, so a `no` can never be the thing that gets throttled — a cancellation that fails is a run that proceeds.
- Audit event `quarry_plan_decision`, carrying the request id and the outcome (`approved`, `cancelled`, `expired`, `superseded`), correlating with `quarry_policy_decision` and `quarry_run_started` for the same run.
- `tests/quarry_plan_gate.rs` — nine integration tests driving the gate through the real gateway message pipeline, because an approval arrives as an ordinary inbound message and nothing in the unit tests would notice if that interception were removed. Both the interception and the dedup exemption were verified load-bearing by mutation: neutering the interception fails five of the nine, and removing the exemption fails the repeated-approval test with the second run expiring.
- `src/quarry/policy.rs` — the `CapsPolicy` trait and a config-backed implementation deciding what a sender is *allowed* to spend, separately from what they *asked* for. `src/quarry/caps.rs` reads a request; this grants one. Closes #113.
- `quarry.policy` config with `default`, per-`channels` and per-`senders` entries. **Default-deny: an absent or empty policy block means nobody may start a run.** The block is file-only with no env var, because a flat environment cannot express per-sender entries and a half-expressible permission model invites an operator to widen the default to compensate.
- Precedence is sender → channel → default, and the matching entry is taken **whole rather than merged** with a broader one. Merging cannot distinguish "this override did not mention `allow_unlimited`" from "this override sets it to false", so a channel's restriction would silently stop applying the moment a sender override set an unrelated field.
- Being allowed to set a cap is its own permission: `allowed_denominations` gates `spend`, `latency` and `due` independently, so a sender may be permitted a spend cap and refused a deadline. `Unlimited` (quarry's `-1`, not zero) is representable and **forbidden unless `allow_unlimited: true`**.
- An over-limit request is either reduced **with disclosure** or refused, per `on_over_limit`. An unrecognised value refuses — a typo in the policy file cannot land on the permissive branch. A policy that grants no cap at all is refused as a misconfiguration rather than passed to quarry, whose `Validate()` requires at least one cap (P9).
- The `Scope` is minted from **verified channel identity only** — the sender and channel the gateway itself resolved. Nothing in the message body can widen it, and a policy entry's own `scope_tags` cannot overwrite the identity tags. Canonicalised byte-identically to quarry's `Scope.Key()` (sorted `k=v;`) and pinned by `tests/fixtures/quarry/scope_keys_golden.json`, 16 vectors generated from real quarry, so a divergence turns every cache hit into a miss loudly instead of quietly.
- **A tag containing `=`, `;` or a control byte is refused, and one bad tag refuses the whole scope.** quarry's `Scope.Key()` escapes nothing, so `{tenant: "victim;user=alice"}` renders `tenant=victim;user=alice;` — byte-identical to the two-tag scope `{tenant: "victim", user: "alice"}`, verified against real quarry. Escaping here would make our keys disagree with quarry's; dropping the offending tag would yield a *wider* scope than requested. Refusal is the only response that is both byte-compatible and injective.
- `narrows_to` implements quarry's actual semantics — **subset of tags, not prefix matching**. `{scope: "chemistry"}` does *not* narrow to `{scope: "chemistry/chem-101"}`, and an empty scope narrows to everything, which is why an empty scope is refused outright.
- `tests/quarry_scope_isolation.rs` — the P6 property (two senders, one byte-identical problem, no shared answer) asserted as the **absence of a cache hit**, not as two scope strings differing. Includes a non-vacuity guard proving the cache really serves repeats, without which "no hit for Bob" would be indistinguishable from "no cache at all". Uses a caching fake binary because quarry's CLI builds its `MemCache` inside the run command, so with one process per run there is no cross-run cache to leak through and the literal test would pass regardless of scope.
- Audit event `quarry_policy_decision`, recording the matched entry, the requested and granted spend, every adjustment, and the minted scope key. Emitted for grants as well as refusals — "what was this run allowed to spend" is the first question asked after an unexpected bill.
- `HotConfig.quarry_policy` — a SIGHUP tightens or revokes a sender's caps without a restart, and revoking returns them to default-deny.
- The gateway's `/v1` bearer token is minted as the child's **only** credential (`QUARRY_PROVIDER_URL` + `QUARRY_PROVIDER_TOKEN`, exactly those two keys), and a run is refused when there is no token to mint rather than spawned with an unauthenticated provider URL.
- `src/quarry/caps.rs` — deterministic, offline parsing of a sender's prose into quarry's `Caps`. Pure function of `(text, now, timezone)`: no I/O, no gateway state, and **no LLM call** — a model call to read "$5" spends money to decide a money budget and makes the parser untestable offline. Closes #111.
- **An uncapped run is refused, never defaulted.** quarry's `Caps.Validate()` rejects one with *"at least one cap is required (P9)"* because planning is budget-conditioned, so a message with no cap gets a question quoting that reason rather than a cap the gateway invented.
- Spend parsing (`$5`, `up to $5.50`, `5 dollars`, `USD 5`, `3 bucks`) into int64 micro-dollars via `round(usd × 1e6)`, sharing one conversion with the receipt reader so a cap and the cost reconciled against it round identically. Truncation is not hypothetical at these amounts: `$2.01 × 1e6` is `2009999.9999999998` in binary floating point, so `int()` yields `2_009_999`. quarry's own `FromFloat` is a truncating Go conversion, i.e. it has the bug its integration doc warns about.
- Latency parsing (`within 20 minutes`, `in 2 hours`, `under 90s`) and deadline parsing (`by 5pm`, `by tonight`, `by tomorrow morning`, `by Friday`) resolved as an instant in the **sender's** timezone. A deadline is a price control in quarry's model — `Deferrable()` turns slack into cheaper inference — so "by tonight" resolved in UTC for a sender in New York buys four hours less compute than they asked for.
- `Disclosure::DueHasNoUpstreamFlag` — quarry's `cmd/quarry` sets `Caps{Spend, Latency}` and **nothing populates `Due`**, so a deadline can currently only be honoured as an equivalent `latency`, which forfeits `Deferrable()` and the batch/off-peak price with it. The substitution is reported to the sender before spend instead of performed silently, and disappears on its own when upstream `--due` lands.
- `Disclosure::DeadlineResolvedIn` — every resolved deadline echoes its instant, its zone, and **which step of the fallback chain supplied the zone**. "Resolved in your timezone" and "resolved in UTC because nothing was configured" are materially different claims and only the sender can tell whether the second is right.
- Per-sender timezone storage (`POST /users/:user_id/preferences` with `{"timezone": "America/New_York"}`) and a `quarry.default_timezone` operator setting (`QUARRY_DEFAULT_TIMEZONE`). The chain is sender preference → operator default → UTC. An unrecognised IANA name is rejected with a 400 rather than stored, because a bad zone falls back silently at resolve time and would move a sender's deadline without telling them.
- Ambiguity is surfaced, not resolved: `by 5` returns both readings to choose between, since twelve hours is the difference and the sender's whole budget rides on it. Two amounts in one message (`$5 or maybe $50`) is the same case.
- **"at most 30 agents" is recognised as not a cap.** quarry has no agent-count denomination — fanout is the planner's decision under the budget — and the nearest thing, recursion depth, is one quarry calls "a BACKSTOP, not the design (P2)", with a depth-bounded run being *under-verified rather than complete*. Inventing the cap would bound the run by something quarry cannot report as `BoundBy`; dropping it silently would return a run bounded by a constraint the sender never set. Node, step, level and token limits are handled the same way, with a token request redirected to the spend cap that actually covers it.
- `src/quarry/` — host support for [quarry](https://github.com/scttfrdmn/quarry) runs. quarry is spawned as a **subprocess** per run rather than linked as a library, matching the BuckTooth twin so both hosts exercise the same integration boundary. Closes #110.
- `src/quarry/event.rs` — the `RunEvent` wire types and a line-at-a-time NDJSON parser, mirrored by hand from quarry's `runevent.go` (there is no shared IDL). The union is **open**: an unrecognised `type` becomes `RunEvent::Unknown` with its raw JSON preserved, never an error — quarry states that adding an event kind cannot break an existing consumer, and rejecting one here would make that false on this side and break on quarry's next release. Costs convert with `round(usd × 1e6)`, never truncation, so a receipt summed here agrees to the micro-unit with what `/v1/chat/completions` metered.
- `src/quarry/supervisor.rs` — spawn, lifecycle and outcome classification. Reads stdout line-at-a-time so a caller can render a live tree, drains stdout and stderr concurrently with the wait (reading either to EOF first deadlocks on a child that fills the other pipe), and reads quarry's record file after exit.
- `quarry.*` config block with seven fields, all settable by env var (`QUARRY_ENABLED`, `QUARRY_BINARY_PATH`, `QUARRY_MAX_CONCURRENT_RUNS`, `QUARRY_RUN_RECORD_DIR`, `QUARRY_RETENTION_MAX_RUNS`, `QUARRY_RETENTION_MAX_AGE_SECONDS`, `QUARRY_RUN_TIMEOUT_SECONDS`). Disabled by default, because a quarry run spends real money.
- Scoped subprocess environment: a quarry child is spawned with `env_clear()` and given only what its caller explicitly passes. It cannot see `ANTHROPIC_API_KEY`, `DISCORD_BOT_TOKEN`, `AWS_*`, `DATABASE_URL` or any other provider or channel credential — a provider key would let it bypass the gateway's metering, a channel token would let it post as the bot. A known-sensitive-name list is stripped as a backstop against a caller that builds the wrong map, and a strip is logged by name.
- Audit events `quarry_run_started`, `quarry_run_ended` and `quarry_run_failed`. `quarry_run_started` carries `env_keys` — **keys only, never values** — which is the record that lets an operator confirm no credential reached the child. `quarry_run_ended` reports the classified outcome and `truncated_by`, which is **absent** when nothing was truncated rather than reported as an empty string.
- Outcome classification that keeps quarry's degradation semantics apart: `completed`, `truncated` (with the denomination that bit), `no_answer`, `timed_out`, `cancelled`, `crashed`, `killed_by_signal`, `stream_malformed`. A truncated run and a crash both produce fewer events than a full run, so the exit status plus the record — never the event count — separates them. Our own timeout and a cancellation report as **time truncation**, never as budget degradation: a caller told "priced out" would raise its spend cap and buy nothing when what ran out was time. A spend-truncated run is the reverse and is not a gap at all, matching quarry's two separate `ErrRecordedGap`/`ErrRecordedUnfunded` sentinels.
- `StreamStats.read_abandoned` — set when the supervisor stopped reading stdout before EOF, which happens when a killed child's **descendant** still holds the write end of the pipe. Reading to EOF regardless would hang the supervisor for as long as the grandchild lives, making the run timeout it had just enforced meaningless. Events parsed before the drain was abandoned are kept and reported: a killed run is a truncated run, not a discarded one, since the money was already spent.
- Concurrency limit enforced by **refusal, not queueing**, with a test that three concurrent requests at a limit of two produce two runs and one `AtCapacity`. A queue would convert a concurrency limit into unbounded undisclosed latency, and a caller's own deadline could expire before its run even started.
- Run-record retention: a background reaper (hourly, plus once at startup) bounded by count and/or age, both independently disableable. Only spawned when quarry is enabled and a limit is set.
- Shutdown now gives in-flight quarry runs up to `gateway.shutdown_timeout_seconds` to finish before aborting their tasks, so a run that already spent money gets a chance to write its record. Anything still running when the budget expires is killed and the loss is logged.
- A fake-quarry-binary test harness: a generated shell script emitting canned NDJSON, so the whole supervisor is tested with **no real quarry, no credentials, no network and no money**. A trait mock would have replaced exactly the subprocess boundary under test; the script also dumps the environment it received to a file, which is what makes "no credential reached the child" a checked assertion rather than an assumption.
- `stateless: true` on `POST /v1/chat/completions` — runs a completion with no conversation state: no history read, none written, no per-user agent created. Two calls with the same `user` are fully independent, which callers that fan a problem out into independent sub-requests need; sharing a history would let siblings see each other's answers and agree for the wrong reason. Honours the full `messages` array including a leading `system` role, applies no RustyNail persona or skills context, and never attaches tools. Part of #109.
- `usage` and `cost` on `/v1/chat/completions` responses, carrying **real provider token counts** and a USD cost. `cost.micro_usd` is integer micro-dollars computed as `round(amount_usd × 1_000_000)` — rounded to nearest, never truncated, since truncation desyncs a caller's ledger by up to a micro-unit per call. Both objects are **omitted when the underlying numbers are unavailable** rather than estimated, and an unpriced model yields no `cost` field rather than a zero one (a zero reads as a free call). Costs come from agenkit's `ModelPricing`, which returns an error for unknown models instead of a fabricated zero. Part of #109.
- `max_tokens` on `/v1/chat/completions` — previously absent from the request struct and silently dropped by serde. Applied to the primary provider and every fallback, so a failover cannot lift a ceiling the caller asked for. Not supported on Ollama, whose agenkit config has no equivalent knob.
- Machine-readable error bodies on `/v1/chat/completions`: `{"error": {"code", "message", "type"}}` with one `code` per cause — `no_messages`, `model_not_available`, `invalid_max_tokens` (400) and `upstream_provider_error` (502). No status is shared between a request-shape error and a transport fault, so callers classify on `code` without parsing a human-readable string. Follows the agate#265 lesson where an overloaded `402` covering four causes forced consumers to string-match a message and misclassify the rest.
- `AgentManager::complete_stateless`, returning a `CompletionOutcome` with provider-reported token counts, cost, resolved model name and finish reason. Accepts both usage key spellings (Anthropic's `input_tokens`/`output_tokens` and everything else's `prompt_tokens`/`completion_tokens`), since a mismatch would make cost silently vanish on half the providers.
- `tests/quarry_provider_contract.rs` — pins the `/v1` wire contract by serving the real router on a real socket and driving it with a real HTTP client against `llm_provider: stub`. No credentials, no network egress, runs in CI. Covers the round trip, full-messages-array survival, sub-request independence, error classification, and model-naming stability.
- The stub LLM provider now reports `model` and `usage` metadata in the same shape a real adapter does, so the metering path is exercisable without credentials.
- `scripts/check-release-consistency.sh` — release consistency gate. Verifies the version in `Cargo.toml` agrees with `Cargo.lock`, `CHANGELOG.md`, `README.md`, `CLAUDE.md` and the Helm chart (`version` and `appVersion`); that the CHANGELOG uses only the six Keep a Changelog headers and has a comparison link; that the Dockerfile builder image matches the declared MSRV; and that both workflows pin agenkit to the same release tag rather than a branch. With `--release` it additionally requires that the tag exists and is an ancestor of `origin/main`, that a GitHub release exists, that no `v*` tag lacks a release, and that the milestone exists and is closed with no open issues. Verified against each historical failure it is meant to catch.
- `msrv` CI job — builds with `cargo check --locked --all-targets` at the `rust-version` declared in `Cargo.toml` instead of `stable`. Closes #91. Every tag from v0.9.0 to v0.14.0 produced an unusable image because the builder MSRV was below what the lockfile required; `stable` in CI could not detect that, so only tag-time Docker builds failed and those logs went unread.
- `.github/workflows/release-consistency.yml` — runs the gate in `--release` mode on every `v*` tag, so tag/release/milestone drift surfaces as a failed check instead of going unnoticed for months.
- `Release consistency` step in the main CI job, gating version coherence on every push and pull request.
- `docker.yml` now also runs on pull requests touching `Dockerfile`, `Cargo.toml`, `Cargo.lock`, the dockerignore files, or the workflow itself. It builds without publishing (login and push are gated on tag pushes) and smoke-tests the result by running `version` in the built image and asserting it reports the version in `Cargo.toml`. Image builds are validated before a tag is cut rather than after.

### Changed
- **BREAKING:** a quarry spawn now requires a verified binary, and with `quarry.verification.enabled` defaulting to `true` while the cosign mechanism (#103) is unimplemented, **every quarry run is refused** until #103 lands or `quarry.verification.enabled: false` is set with a capability manifest beside the binary. Deployments running quarry today must make that choice explicitly. The two `enabled` defaults point in opposite directions on purpose: `quarry.enabled` is `false` because a run spends real money, and `verification.enabled` is `true` because once you have asked for runs you have asked for verified ones — defaulting it to `false` would ship silent unverified execution to anyone who never read this entry.
- `Supervisor::run` refuses **before** creating the run directory, so a refusal leaves no filesystem artifact. `SpawnError` gains `Unverified`, whose `code()` delegates to the underlying refusal rather than collapsing fifteen causes into one slug: a code that could not tell "no signature" from "no mechanism installed" would be the operator-hunting problem the gate exists to avoid. `SpawnError::sender_message()` is new, so the sender-facing text for a spawn failure is one function rather than a habit.
- `GET /users/:user_id/preferences` now returns `timezone` alongside `preferred_channel_id`, and `POST` accepts either field independently — **an absent field leaves the stored value alone** rather than clearing it. A store holding a single value per user would have lost the channel preference the first time a timezone was set, silently re-routing that user's replies. `docs/api.md` also documented the wrong field name (`preferred_channel`) and a POST response body that was never returned; both are corrected.
- **BREAKING:** `/v1/chat/completions` no longer echoes the requested `model` back. The response now names the model that **actually ran**, taken from the provider's own response where available. The request `model` is validated instead of ignored: the configured model name, a prefix of it, an empty string, or one of `rustynail`/`default`/`gateway` are accepted, and any other name is refused with `model_not_available` (400) rather than silently served by whatever `agents.llm_model` points at. Asking for `gpt-4` and being served Claude while the response claimed `gpt-4` made the field untrue and defeated replay. Clients sending an arbitrary placeholder model string must switch to one of the accepted aliases.
- **BREAKING:** `usage` on `/v1/chat/completions` is no longer always present, and no longer fabricated. It was computed as `content.len() / 4` from the request and response strings — a character estimate, not a token count, and never derived from the provider. It is now absent unless the provider reported real counts, which means the default stateful path reports no usage at all: it routes through `ConversationalAgent` (and `ReActAgent` when tools are enabled), which do not preserve provider usage metadata. Use `stateless: true` for metering. Clients that relied on the field always existing must handle its absence — the previous numbers were not measurements.
- `/v1/chat/completions` now respects the entire `messages` array. It previously read only the last `user` message (`.rev().find(...)`) and discarded system messages and all prior turns without saying so.
- `/v1/chat/completions` errors return `502` with `upstream_provider_error` for provider failures instead of `500` with a bare text body.
- Documented that the project stays on `0.x.x` until further notice: `1.0.0` is not a target and the `v1.0.0` milestone is a bucket for API-stability-gated work, not a timeline. `CLAUDE.md` versioning section and milestone table updated; `README.md` states the API and config format may change in any minor release.
- `CLAUDE.md` documents a 10-step release checklist and states that a version bump is not a release. Records the three drifts that motivated it: v0.14.0 bumped but never tagged (#94), v0.5.0–v0.7.0 tagged with no release, and the MSRV-broken images from v0.9.0 to v0.14.0. Adds a section on the agenkit path-dependency pin — both workflows must pin the same release tag, never a branch — and notes that the agenkit crate version does not track its repo tags. Also documents that the published image tag has no `v` prefix and is amd64-only.

### Fixed
- **No real quarry record could be deserialized — every one of the eight vendored records failed.** Go's `encoding/json` writes a nil slice as `null`, not `[]`, and `#[serde(default)]` does not cover an explicit null: it fires for an *absent* key only. So one nil list anywhere in a record — `Unverified` on a run where every node was verified, `Children` on any leaf — made the whole record unparseable. It went unnoticed because `read_record` treats a parse failure as an absent record and only `warn!`s, and because every hand-written fixture spelled its empty lists `[]`; the record type had therefore never once parsed something quarry produced. A `null_as_empty` deserializer now covers the three list fields, and four conformance tests read the real records rather than fixtures we authored.
- **`--events-json` was never passed to quarry, so no run produced any events.** quarry gates its entire `WriteRunEvents` call on the flag and writes a human-readable summary to stdout without it — so omitting it did not degrade the stream, it removed it, and every real run classified as `stream_malformed`. Verified by driving the real binary both ways through the real supervisor: without the flag, prose on stdout and a `usage_error`; with it, exactly six NDJSON lines and a `completed` run carrying its answer and a 71-micro-dollar receipt. Tests stayed green because `FakeBehavior` emitted its canned NDJSON unconditionally, ignoring argv — the same class of miss as the `[0.15.0]` empty-buffer harness, where the thing under test was replaced by the fake. The fake now **gates its stdout on the flag** and emits a human summary without it, so removing the flag fails a test rather than passing quietly. Closes #123.
- **Exit code 3 was classified as a crash.** quarry's vocabulary is five codes — `0` complete, `1` fault, `2` usage, `3` time-truncated, `4` no answer — not the two this integration assumed. A run whose deadline bites exits 3 and may still hold a perfectly good partial answer, which is precisely the result quarry promises to return; calling that a crash threw the answer away and invited a retry that would spend again for something already in hand. `EXIT_FAILURE` is replaced by the named set, and where the frame closed, quarry's own `outcome` field outranks the exit code.
- `quarry_stream` and `quarry_outcome` were being folded into `RunEvent::Unknown`, so a fully successful framed run reported `stream_malformed` — the terminal event was unreadable and therefore treated as missing. The two host-half events now parse, and the terminal one is found by scanning the stream **backwards**: quarry's union is open, so a future event kind appended after the outcome would make a reader taking `events.last()` report every complete run as cut off.
- `OutcomeEvent::cap_micros` deserialises to `-1` (unlimited) rather than `0` when absent, and `has_spend_cap()` compares against `-1` instead of testing the sign. Every other field on that event may honestly default to zero — for a count zero is a measurement, and an empty `bound_by` means no cap bound the run — but here `#[serde(default)]` turned a cap quarry never stated into the tightest cap expressible, so any run that spent anything read as having blown its budget. A cap of `0` is a real cap that funds nothing, and is now distinguishable from no cap at all.
- `.gitignore` now covers `quarry-run-*.json`. These are records of real runs and carry problem statements and answers, so a run pointed at the repo root rather than a configured `run_record_dir` cannot be committed by accident.
- `MessageChunker::chunk` panicked with "byte index N is not a char boundary" whenever a multi-byte character straddled the chunk limit. Slicing was by byte index with no boundary check, so `£` in a spend figure, an em dash, an ellipsis, an emoji or any non-Latin script could abort the message-handling task — the sender got no reply at all rather than a split one. Splits now land on the largest character boundary at or below the limit, the limit is documented as **bytes** (the conservative reading, since the direction that gets a message rejected is being over the byte count while under the character count), and two further hangs are closed: a limit of `0` is treated as "do not chunk" instead of looping on empty chunks, and a chunk beginning with whitespace no longer splits off an empty chunk forever.
- `CLAUDE.md`'s parity table claimed "Token/cost accounting ✅" for a feature that did not exist in this repo at all — no cost accounting anywhere, and tokens estimated as `len()/4`. The row is now ⚠️ with a note stating exactly what works. This is the third row in that table to have claimed parity for shipped-but-non-functional code, after the empty-buffer test harness and the un-buildable Docker image, which the table's own guidance warns about; the legend now defines ⚠️ and prefers it over an overstated ✅.
- Backfilled the missing GitHub releases for `v0.5.0`, `v0.6.0` and `v0.7.0`. All three were tagged on 2026-03-18 but never released; they are now published from their original CHANGELOG entries and marked as retroactive.
- Created the missing `v0.7.0` milestone and closed the `v0.14.0` milestone, which had no open issues but was still marked open.

## [0.15.0] - 2026-08-03

Includes the documentation work originally logged under `[0.14.0]`, which was
never tagged or released. See the note under that heading.

### Security
- The shell tool allowlist no longer prefix-matches the raw command string. `src/tools/shell.rs` checked `command.starts_with(prefix)` and then passed the whole string to `sh -c`, so an allowlist entry of `git` also permitted `git status; rm -rf ~` — the allowlist read as a boundary but did not bound execution. Entries are now tokenized and matched element-wise against the parsed argv, and an allowlisted command is exec'd directly instead of through a shell.

### Fixed
- Test channel (`POST /test/send`) now injects messages into the gateway pipeline instead of returning a bare `200` with no side effect; replies are captured and retrievable from `GET /test/responses`. The zero-credential harness was previously non-functional end to end.
- Gateway no longer constructs three separate `TestChannel` instances with independent capture buffers. It builds one channel and hands the HTTP layer a `CapturedMessages` handle to the same buffer.
- Docker builds now succeed. The builder image was `rust:1.75`, below the MSRV of several locked dependencies (`aws-credential-types` 1.3.0 requires 1.94.1, `wide` 1.6.0 requires 1.89, `base64ct` 1.8.3 is edition 2024). Builder image and the new `rust-version` field are both pinned to 1.94.
- `tests/harness/harness_test.rs` was never compiled because it lives outside `tests/` root and had no `[[test]]` target. It is now registered, serialized against the process-wide response buffer, and polls for responses rather than reading once.
- CI is green again. Every run since v0.9.0 failed at `cargo fmt --check`, so `cargo clippy -- -D warnings` and `cargo test` never executed upstream. Formatting is applied across the tree and all clippy findings are resolved: `derivable_impls` (`AuditConfig`), `let_and_return` (`create_router`), three `manual_div_ceil` sites, `type_complexity` (new `RingBuffer` alias), `await_holding_lock` (harness lock is now `tokio::sync::Mutex`), and unused imports / dead code in the test helpers.

### Added
- `rust-version = "1.94"` in `Cargo.toml`, documenting the MSRV and keeping it in sync with the Dockerfile builder image.
- `Dockerfile.dockerignore` — the build context is the parent directory (required for the agenkit path dependency), so an allowlist prevents sibling projects from being swept into the context.
- `CapturedMessages` type alias and `TestChannel::captured_handle()` / `drain_captured()` in `src/channels/testchan.rs`.

### Changed
- **BREAKING (shell tool allowlist):** a non-empty `tools.shell.allowed_commands` now disables shell syntax. Commands containing `;`, `|`, `&`, `$`, `` ` ``, `>`, `<`, or a newline are rejected, and the command is exec'd directly rather than via `sh -c` — so pipes, redirection, substitution, and globbing are unavailable. Quotes still group arguments. Matching is token-wise, so `git` permits `git log` but no longer `gitleaks`, and `git status` permits only that subcommand. Clear the allowlist to opt back into full shell semantics. Only affects deployments that configured an allowlist; the default is empty.
- Both CI workflows pin `ref: v0.87.0` when checking out agenkit instead of tracking the default branch. A path dependency carries no version constraint, so an upstream push previously changed what CI and release images built with no commit here.
- `.env.example` refreshed from 13 lines (frozen at v0.1.0) to a complete, sectioned reference covering every environment variable read by `src/config/mod.rs`.
- `README.md` prerequisites and badge updated to Rust 1.94+; added agenkit sibling-clone instructions, the pinned release tag, and directory layout.
- `CLAUDE.md` documents the agenkit sibling-clone prerequisite, the pin-to-a-tag rule, the required directory layout, and the 1.94 minimum toolchain; the parity table now states that ✅ requires verification, not just shipped code.
- `docs/configuration.md` documents how `shell.allowed_commands` is enforced, with a migration note for the behaviour change.

### Removed
- Dead `config: TeamsConfig` field from `TeamsChannel` (the only build warning); only the auth credentials are needed past construction.
- Unused `make_test_config` helper from `tests/common/mod.rs` and the unreachable `AlwaysFailAgent` from `src/agents/manager.rs` (it could not be injected into `AgentManager`).

## [0.14.0] - 2026-03-18

> Logged as released on 2026-03-18 but never tagged, and no image was published.
> The Docker build could not have succeeded at this point — the builder image
> was below the MSRV of the locked dependencies. Shipped as part of `0.15.0`.

### Added
- `docs/configuration.md`: complete field-by-field configuration reference for all 11 top-level config sections (`gateway`, `channels`, `agents`, `memory`, `tools`, `skills`, `audit`, `cron`, `mcp`, `otel`, `dashboard`); includes env var table, YAML examples, and per-backend subsections
- `docs/deployment.md`: operator runbook covering Docker pre-built image, Docker Compose (with agenkit build-context note), Kubernetes via Helm, full environment variable cheat sheet, health check + K8s probe YAML, Prometheus/Grafana integration, SIGHUP hot-reload procedure, and production checklist
- `docs/channels.md`: per-channel setup guide for all 12 channels (Discord, WhatsApp, Telegram webhook+longpoll, Slack webhook+socket, SMS/Twilio, Teams, Email, Webchat, Generic Webhook, Test Channel); each section includes prerequisites, credential steps, minimal config snippet, webhook URLs, and caveats
- `docs/cli.md`: man-page style reference for all 7 CLI subcommands (`start`, `status`, `version`, `config check`, `config validate`, `completions`, `mcp serve`); includes option tables, exit codes, and example output
- `docs/api.md`: complete HTTP API reference for all route groups (health/probes, dashboard, channel webhooks, webchat, admin, cron, user preferences, OpenAI-compat, test channel); includes request/response schemas and curl examples
- `docs/architecture.md`: architecture deep-dive with updated ASCII diagram (12 channels, 5 memory backends, tool registry, MCP); 8-stage message pipeline documentation; channel adapter pattern; agent manager internals; memory backend comparison; hot-reload mechanics; full observability coverage; source map table
- `docs/troubleshooting.md`: troubleshooting guide covering startup failures, per-channel issues, no-response debugging steps, memory backend errors, performance tuning, and useful debug commands

### Changed
- `README.md`: complete overhaul from v0.4.0 baseline; updated badges to v0.13.0 beta; rewrote Features section to reflect current reality (12 channels, 7 LLM providers, 5 memory backends, full tool registry); updated Quick Start, Channels, Deployment, HTTP API, CLI, and Architecture sections; replaced stale Roadmap section with Sister Projects table; fixed version references from "0.1.0 Alpha" to "0.13.0 Beta"

## [0.13.0] - 2026-03-18

### Added
- Rate limiter unit tests: `test_disabled_always_allows`, `test_within_window_allows`, `test_exceeds_limit_blocks`, `test_window_reset_allows_again`, `test_independent_users` (`src/gateway/rate_limiter.rs`)
- Agent fallback unit tests: `test_primary_success_no_fallback`, `test_capacity_error_uses_fallback`, `test_non_capacity_error_not_forwarded`, `test_all_fallbacks_fail_returns_last_error` (`src/agents/fallback.rs`)
- Agent manager unit tests: `test_process_message_stub_returns_echo`, `test_process_message_stream_emits_done`, `test_process_message_stream_emits_tokens`, `test_retry_disabled_calls_once`, `test_retry_enabled_returns_success_on_stub` (`src/agents/manager.rs`)
- HotConfig unit tests: `test_hotconfig_from_config`, `test_hotconfig_apply_detects_changes`, `test_hotconfig_apply_ignores_no_change` (`src/gateway/mod.rs`)
- Admin API integration tests: `test_admin_clear_memory_returns_200`, `test_admin_channels_health_returns_json`, `test_admin_skills_reload_returns_200`, `test_admin_requires_bearer_when_configured`, `test_admin_channels_health_structure` (`tests/admin_api.rs`)
- Teams webhook HMAC integration tests: `test_teams_no_hmac_secret_accepts_any_request`, `test_teams_valid_hmac_accepted`, `test_teams_invalid_hmac_rejected`, `test_teams_missing_auth_header_rejected`, `test_teams_malformed_json_rejected` (`tests/teams_webhook.rs`)
- Full pipeline integration tests: `test_pipeline_dedup_drops_duplicate`, `test_pipeline_multi_user_isolation`, `test_pipeline_chunking_splits_long_response`, `test_pipeline_formatting_slack_applied` (`tests/pipeline.rs`)
- `rustynail config validate` CLI subcommand: runs preflight checks (config load, API key presence, memory backend dependencies) and exits 0 (all pass) or 1 (any failure); prints `[✓]`/`[✗]` lines for each check
- `AuditEvent::AdminAction` variant: emitted by `admin_clear_memory_handler`, `admin_reload_skills_handler`, and `admin_channels_health_handler` with `endpoint`, optional `param`, and `success` fields
- `handle_message_for_test_full()` public test helper in `src/gateway/mod.rs` — exposes rate-limiter, deduplicator, chunker, and formatting controls for full-pipeline integration tests
- `make_test_state_stub()` helper in `tests/common/mod.rs` — `AppState` with stub LLM provider; no real API key required

### Changed
- `Cargo.toml` bumped to `0.13.0`

## [0.12.0] - 2026-03-18

### Added
- Teams HMAC-SHA256 activity validation: optional `TeamsAuthConfig.hmac_secret` field validates inbound Bot Framework activities using `Authorization: HMAC <hex>` header; env `TEAMS_HMAC_SECRET`; empty = skip validation (backward compatible)
- Temporal memory decay in vector store: `VectorMemoryStore` ring buffer now stores `(message, timestamp)` pairs; `get_history()` returns messages sorted descending by exponential recency weight (`half_life = VECTOR_DECAY_HALF_LIFE_SECONDS`, default 3600s); `recency_weight()` helper: at half-life age ≈ 0.5
- Token-based memory compaction: `SummarizationConfig.trigger_token_budget` (env `SUMMARIZATION_TRIGGER_TOKEN_BUDGET`; default 0 = disabled) triggers summarization when estimated token count (4 bytes ≈ 1 token) exceeds the budget, independently of the message-count threshold
- WebSocket token streaming for webchat: `AgentManager::process_message_stream()` splits responses into 5-byte chunks emitted via an mpsc channel with 10ms delay; webchat WS handler streams `{"type":"token","content":"…"}` frames followed by `{"type":"done"}`; widget JS updated to build up streaming messages in place
- OpenAI-compatible `/v1/chat/completions` endpoint (`src/gateway/openai_compat.rs`): supports non-streaming JSON response and SSE streaming (`"stream": true`) with `data: {…}` lines ending in `data: [DONE]`; `StreamEvent` enum re-used from `process_message_stream`

### Changed
- `Cargo.toml` bumped to `0.12.0`
- `TeamsAuthConfig` extended with `hmac_secret: String`; `teams_webhook_receive` handler switches from `Json<TeamsActivity>` to `Bytes` extractor for pre-parse HMAC verification
- `MemoryConfig` extended with `vector_decay_half_life_seconds: f64`; `VectorMemoryStore::new()` delegates to new `with_decay()` constructor; `Gateway::new()` passes configured half-life
- `SummarizationConfig` extended with `trigger_token_budget: usize`
- `AgentManager` gains `StreamEvent` enum and `process_message_stream()` method (requires `Arc<Self>`)
- `HttpServerConfig` and `AppState` extended with `teams_hmac_secret: String`; wired from `config.channels.teams.auth.hmac_secret`
- `gateway/mod.rs` adds `pub mod openai_compat`
- Widget JS in `src/channels/webchat.rs` handles `token`, `done` frame types for in-place streaming display

## [0.11.0] - 2026-03-18

### Added
- `Attachment` struct in `src/types.rs` (`url`, `media_type`, `filename`) — replaces the `Vec<String>` attachments field on `Message` with typed `Vec<Attachment>`; prerequisite for attachment auto-routing
- Message chunking: `MessageChunker` in `src/gateway/chunker.rs` splits long responses at whitespace boundaries; configured via `gateway.chunking_enabled` (env: `GATEWAY_CHUNKING_ENABLED`) and `gateway.chunking_limits` map (file-only); built-in per-platform defaults: discord → 2000, slack → 4000, teams → 1024, telegram/whatsapp → 4096
- Message deduplication: `MessageDeduplicator` in `src/gateway/deduplicator.rs` uses a SHA-256 ring buffer to drop duplicate `(user_id, content)` pairs within a sliding window; configured via `gateway.deduplication.enabled` (env: `GATEWAY_DEDUP_ENABLED`) and `gateway.deduplication.window_size` (env: `GATEWAY_DEDUP_WINDOW_SIZE`; default 256); deduplication runs at the very top of the pipeline before any processing
- Channel-aware response formatting: `ResponseFormatter` in `src/gateway/formatter.rs` converts standard markdown to platform-native syntax; enabled by `gateway.formatting_enabled` (env: `GATEWAY_FORMATTING_ENABLED`; default true); rules: Slack `**bold**` → `*bold*`, links → `<url|text>`; Telegram `**bold**` → `*bold*` + MarkdownV2 special-char escaping; WhatsApp `**bold**` → `*bold*`, links → `text (url)`; Discord/Teams: pass-through; code blocks are protected from inline substitution
- Attachment auto-routing: when `gateway.auto_route_attachments` (env: `GATEWAY_AUTO_ROUTE_ATTACHMENTS`) is enabled, `pdf` attachments prepend "Please analyze this PDF: {url}" to the agent prompt, and `image` attachments prepend "Please describe this image: {url}"
- LLM retry jitter: `agents.retry.jitter_enabled` (env: `AGENT_RETRY_JITTER_ENABLED`; default false) applies ±20% randomization to exponential backoff delays, reducing retry storms
- LLM provider fallback chain: `FallbackAgent` in `src/agents/fallback.rs` wraps the primary provider; on capacity/overload errors (HTTP 500, 503, "overloaded", "model not found") each configured fallback is tried in order; 429 rate-limit errors are not forwarded (handled by caller retry); fallbacks configured via `agents.fallback_providers` list in YAML (file-only)
- `DeduplicationConfig` struct with `enabled` and `window_size` fields; `FallbackProviderConfig` struct with `provider`, `model`, `api_key`, `api_base` fields

### Changed
- `Cargo.toml` bumped to `0.11.0`
- `Message.attachments` changed from `Vec<String>` to `Vec<Attachment>`; `Message::with_attachments()` updated accordingly
- `GatewayConfig` extended with `chunking_enabled`, `chunking_limits`, `formatting_enabled`, `auto_route_attachments`, `deduplication`
- `AgentRetryConfig` extended with `jitter_enabled: bool`
- `AgentsConfig` extended with `fallback_providers: Vec<FallbackProviderConfig>`
- `AgentManager::create_llm()` refactored into `create_llm()` (public pipeline entry) + `create_llm_from_config()` (parameterised builder); wraps primary with `FallbackAgent` when fallback providers are present
- `handle_message_inner()` pipeline order: deduplication → audit → rate limiting → attachment routing → agent call → response formatting → chunked send
- `gateway/mod.rs` extended with `pub mod chunker`, `pub mod deduplicator`, `pub mod formatter`
- `agents/mod.rs` extended with `pub mod fallback`
- `lib.rs` now re-exports `Attachment` alongside `Message`

## [0.10.0] - 2026-03-18

### Added
- PDF analysis tool (`pdf_analysis`): fetches or reads a PDF, base64-encodes it, and posts it to the Anthropic API as a `document` content block with the `pdfs-2024-09-25` beta header; configured via `tools.pdf_enabled` (env: `TOOLS_PDF_ENABLED`; default false); supports path or URL source, custom prompt, configurable `max_bytes` (default 32 MB)
- Image analysis tool (`image_analysis`): fetches or reads a jpeg/png/gif/webp image, detects media type from Content-Type header or file extension, base64-encodes it, and posts it to the Anthropic vision API; configured via `tools.image_enabled` (env: `TOOLS_IMAGE_ENABLED`; default false); supports path or URL source, custom prompt, configurable `max_bytes` (default 5 MB)
- Admin API endpoints under `/admin/*` (protected by bearer auth middleware): `DELETE /admin/memory/:user_id` clears a user's conversation history; `POST /admin/skills/reload` hot-reloads skills from disk without restart and returns `{"skills_loaded": N}`; `GET /admin/channels/health` returns per-channel health status with `health_detail` string for degraded/unhealthy channels
- Cron scheduler (`CronScheduler` in `src/cron/`): fires synthetic messages on configurable intervals (suffix: `s`, `m`, `h`, `d`); configured via `cron.jobs` YAML list (name, schedule, message, channel_id, user_id, enabled); invalid schedule strings log a warning and are skipped; `GET /cron/jobs` returns a snapshot of all job statuses
- WebSocket origin restriction: `gateway.allowed_ws_origins` config (env: `GATEWAY_ALLOWED_WS_ORIGINS`, comma-separated); when non-empty, both `/dashboard/ws` and `/channels/webchat/ws` upgrade handlers return `403 Forbidden` for unlisted origins; empty list allows all (backward compatible)
- Configurable shutdown timeout: `gateway.shutdown_timeout_seconds` (env: `GATEWAY_SHUTDOWN_TIMEOUT_SECONDS`; default 30); `gateway.stop()` is wrapped with `tokio::time::timeout`; logs a warning if exceeded; displayed in `rustynail config check` output
- `AgentManager::reload_skills_context()` async method to replace the active skills context without restart; `skills_context` field changed from `Option<String>` to `Arc<RwLock<Option<String>>>` to enable concurrent hot-reload

### Changed
- `Cargo.toml` bumped to `0.10.0`
- `ToolsConfig` extended with `pdf_enabled: bool` and `image_enabled: bool`
- `GatewayConfig` extended with `allowed_ws_origins: Vec<String>` and `shutdown_timeout_seconds: u64`
- `Config` extended with top-level `cron: CronConfig` (default empty)
- `AppState` and `HttpServerConfig` extended with `skills_config`, `cron_jobs`, and `allowed_ws_origins` fields
- `dashboard_ws_handler` and `webchat_ws_handler` return type changed to `Response` to support conditional 403 on origin mismatch
- Router gains `delete` import; new routes: `DELETE /admin/memory/:user_id`, `POST /admin/skills/reload`, `GET /admin/channels/health`, `GET /cron/jobs`
- `rustynail config check` now prints WS origins, shutdown timeout, cron job count, PDF tool state, and image tool state

## [0.9.0] - 2026-03-18

### Added
- Per-user sliding-window rate limiting: `RateLimiter` in `src/gateway/rate_limiter.rs` using `DashMap`; configured via `gateway.rate_limit.enabled`, `messages_per_window`, `window_seconds` (env: `RATE_LIMIT_ENABLED`, `RATE_LIMIT_MESSAGES`, `RATE_LIMIT_WINDOW_SECONDS`); off by default (backward compatible); users who exceed the limit receive a friendly warning message
- Structured NDJSON audit logging: `AuditLogger` + `AuditEvent` in `src/audit/mod.rs`; async background writer via `mpsc::UnboundedSender`; events: `auth_rejected`, `rate_limit_hit`, `message_received`, `tool_executed`, `config_reloaded`, `agent_created`, `llm_error`; configured via `audit.enabled`, `audit.path` (env: `AUDIT_ENABLED`, `AUDIT_PATH`); off by default
- Request body size limit: `DefaultBodyLimit::max()` applied globally (WebSocket upgrade routes exempt); configured via `gateway.max_body_bytes` (env: `GATEWAY_MAX_BODY_BYTES`; default 1 MB); returns `413 Payload Too Large` when exceeded
- Handler timeout: `TimeoutLayer` wraps all routes via `HandleErrorLayer`; configured via `gateway.request_timeout_seconds` (env: `GATEWAY_REQUEST_TIMEOUT_SECONDS`; default 30s); returns `408 Request Timeout`
- Security Prometheus counters: `rustynail_auth_failures_total`, `rustynail_rate_limit_hits_total`, `rustynail_llm_errors_total`, `rustynail_llm_retries_total`; exposed via `/metrics`; corresponding `record_*` methods on `MessageStats`
- Config hot-reload via SIGHUP: `HotConfig` struct wraps hot-reloadable config subset (`log_level`, `api_token`, `rate_limit.*`, `audit.*`); `Gateway::hot_config_handle()` returns an `Arc<RwLock<HotConfig>>`; bearer auth middleware reads token from `HotConfig` at request time; SIGHUP handler in `main.rs` reloads config and logs changed field names (Unix only)
- LLM retry with exponential backoff in `AgentManager::process_message()`: configurable via `agents.retry.enabled`, `max_attempts`, `base_delay_ms` (env: `AGENTS_RETRY_ENABLED`, `AGENTS_RETRY_MAX_ATTEMPTS`, `AGENTS_RETRY_BASE_DELAY_MS`; defaults: enabled, 3 attempts, 100 ms base); each retry increments `rustynail_llm_retries_total`; after all attempts exhausted, a friendly fallback message is returned to the user

### Changed
- `Cargo.toml` bumped to `0.9.0`
- `GatewayConfig` extended with `rate_limit: RateLimitConfig`, `max_body_bytes: usize`, `request_timeout_seconds: u64`
- `AgentsConfig` extended with `retry: AgentRetryConfig`
- `Config` extended with top-level `audit: AuditConfig`
- `AppState` and `HttpServerConfig` extended with `rate_limiter`, `audit`, `hot_config` fields
- `create_router()` now accepts `max_body_bytes` and `request_timeout_seconds` parameters
- `AgentManager` constructor chain extended with `with_tools_skills_and_stats()` accepting optional `Arc<MessageStats>` for retry metrics
- Bearer auth middleware reads expected token from `HotConfig` (hot-reloadable) instead of static `AppState.api_token`

## [0.8.0] - 2026-03-18

### Added
- Gateway API bearer token authentication: `GatewayConfig.api_token` (env: `GATEWAY_API_TOKEN`); Axum middleware uses `subtle::ConstantTimeEq` for timing-safe comparison; `/live` and `/ready` always exempt (K8s probes); disabled when token is absent (backward compatible)
- Token/cost accounting: `tokens_in` and `tokens_out` atomic counters on `MessageStats`; `record_tokens(input, output)` called after each LLM completion; exposed via `/metrics` as `rustynail_tokens_in_total` / `rustynail_tokens_out_total` and `/dashboard/data` JSON
- Web fetch tool (`web_fetch`): fetches a URL with 15s timeout via `reqwest`; HTML stripped via `scraper` crate (skips `<script>`, `<style>`, `<noscript>`); respects `max_bytes` parameter (default 512 KB); registered when `tools.enabled = true`
- Shell execution tool (`shell`): executes commands via `sh -c` with `tokio::process::Command`; two-step approval gate (returns `"Pending approval: ..."` unless `approved=true`); optional `allowed_commands` prefix allowlist; configurable via `tools.shell.enabled`, `tools.shell.require_approval`, `tools.shell.allowed_commands`
- Agent skills system: `SkillRegistry` in `src/skills/mod.rs` discovers `SKILL.md` files from configured paths; selected skills injected into agent system prompts (up to `skills.max_active`); configured via `skills.enabled`, `skills.paths`, `skills.max_active` (env: `SKILLS_ENABLED`, `SKILLS_PATHS`, `SKILLS_MAX_ACTIVE`)
- Bundled skills: `skills/rustynail-assistant/SKILL.md` (surfaces available channels and tools) and `skills/formatting/SKILL.md` (channel-aware output formatting guidance)
- Microsoft Teams channel (`TeamsChannel`): Bot Framework Activity webhook at `POST /channels/teams/messages`; OAuth2 client credentials token cache with 60s pre-refresh; outbound send to `{serviceUrl}/v3/conversations/{id}/activities/{activityId}`; configured via `TEAMS_APP_ID`, `TEAMS_APP_PASSWORD`
- Zero-credential test harness: `StubAgent` (echo mode or fixed response, selected by `llm_provider = "stub"`); `TestChannel` with `POST /test/send` inject and `GET /test/responses` drain endpoints; `configs/harness.yaml` minimal config; integration tests in `tests/harness/` (skip unless `HARNESS_URL` is set)
- Helm chart at `deploy/helm/rustynail/`: Chart.yaml, values.yaml, deployment, service, configmap, secret, ingress, HPA, service account, helpers, NOTES.txt; liveness → `/live`, readiness → `/ready`; optional Redis subchart
- Criterion benchmark suite in `benches/gateway_benchmarks.rs`: `bench_inmemory_store_add`, `bench_inmemory_store_get`, `bench_config_load`, `bench_message_stats_record`
- `config check` prints `Gateway auth: enabled/disabled` and `Skills: enabled (N paths, M skills loaded) / disabled`
- `stub` provider option for `agents.llm_provider` in `AgentManager`

### Changed
- `Cargo.toml` bumped to `0.8.0`; added `scraper = "0.19"` (HTML stripping), `subtle = "2"` (constant-time comparison), `criterion = "0.5"` (dev dep for benchmarks)
- `GatewayConfig` extended with `api_token: Option<String>`
- `ChannelsConfig` extended with `teams: Option<TeamsConfig>` and `test_channel: bool`
- `ToolsConfig` extended with `shell: ShellToolConfig`
- `Config` extended with `skills: SkillsConfig`
- `AgentManager` gains `skills_context: Option<String>` field; new constructor `with_tools_and_skills()`; skills context appended to system prompt when set
- `HttpServerConfig` and `AppState` extended with `teams_tx`, `api_token`, `test_channel` fields
- `DashboardData` extended with `tokens_in` and `tokens_out` fields
- Gateway `start()` registers Teams channel, test channel, web fetch tool, and shell tool

## [0.7.0] - 2026-03-18

### Added
- MCP (Model Context Protocol) support via agenkit 0.82.0
- `rustynail mcp serve` subcommand: exposes RustyNail's registered tools (calculator, formatter, filesystem, web search, calendar) as an MCP server over stdio, compatible with Claude Code, Cursor, and any MCP client
- MCP client connectivity in gateway `start()`: configure `mcp.servers` in YAML to connect to external MCP servers at startup and register their tools into the agent tool registry; supports both `stdio` (subprocess) and `http` transports; gracefully skips misconfigured or unreachable servers with an error log
- `McpConfig` and `McpServerEntry` structs in config: `mcp.servers` list with `name`, `transport`, `command`, `args`, `env` (stdio), and `url` (http) fields

## [0.6.0] - 2026-03-18

### Added
- Shell completion generation via `clap_complete`; `rustynail completions <bash|zsh|fish|powershell|elvish>` prints the completion script for the requested shell
- Grafana dashboard (`deploy/grafana/dashboard.json`) with panels for messages_in/out rate, active_users, healthy_channels, p50/p95/p99 latency histogram, and active users over time; auto-load provisioning config at `deploy/grafana/provisioning/dashboards/rustynail.yml`
- Prometheus alert rules (`deploy/prometheus/alerts.yaml`): `RustyNailDown`, `HighMessageLatency`, `ChannelUnhealthy`, `HighErrorRate`, `NoActiveUsers`
- SQLite memory backend (`SqliteStore`) implementing `MemoryStore`; configured via `SQLITE_PATH` env var or `memory.sqlite_path` YAML key; uses a dedicated single-threaded tokio runtime to bridge the sync trait; history trimmed to `max_history` on every insert
- PostgreSQL memory backend (`PostgresStore`) implementing `MemoryStore`; configured via `DATABASE_URL` env var or `memory.postgres_url` YAML key; creates `rustynail_messages` table with `(user_id, ts)` index automatically
- Vector memory backend (`VectorMemoryStore`) implementing `MemoryStore`; wraps agenkit's `VectorMemory` with an in-process `SimpleEmbeddingProvider` (64-dim character bigram); maintains a secondary ring buffer for ordered `get_history()` access; semantic search available via underlying VectorMemory
- Memory summarization (`MemorySummarizer`); fires asynchronously when history exceeds `summarization.trigger_at` (default 40); replaces oldest `(len - keep_recent)` messages with a `[Summary: ...]` entry via the configured LLM; configured via `SUMMARIZATION_ENABLED`, `SUMMARIZATION_TRIGGER_AT`, `SUMMARIZATION_KEEP_RECENT`, `SUMMARIZATION_MODEL` env vars
- Multi-LLM provider support in `AgentManager::create_agent()`; `llm_provider` can now be `"anthropic"` (default), `"openai"`, `"ollama"`, `"gemini"`, `"bedrock"`, `"litellm"`, or `"openai-compat"`; configured via `LLM_PROVIDER` env var or `agents.llm_provider` YAML key
- AWS Bedrock support: `aws_region` field on `AgentsConfig` (env: `AWS_REGION`; default `us-east-1`)
- SMS channel (`SmsChannel`) via Twilio: webhook receive at `POST /webhooks/sms` (TwiML response), outbound send via Twilio Messages REST API; configured via `TWILIO_ACCOUNT_SID`, `TWILIO_AUTH_TOKEN`, `TWILIO_FROM_NUMBER`
- Generic inbound webhook channel (`WebhookChannel`): `POST /webhooks/:name` matches against `channels.webhook.endpoints` config; HMAC-SHA256 verification if `secret` set; JSONPath text extraction via `jsonpath-rust`; configured via YAML `channels.webhook.endpoints`
- Web chat widget channel (`WebchatChannel`): WebSocket endpoint at `GET /channels/webchat/ws?session_id=<uuid>`; static auto-reconnecting vanilla JS widget at `GET /channels/webchat/widget.js` (~3KB, no dependencies); per-session routing via `DashMap`; configured via `WEBCHAT_ENABLED`, `WEBCHAT_ALLOWED_ORIGINS`, `WEBCHAT_WELCOME_MESSAGE`
- Email channel (`EmailChannel`): IMAP polling receive (sync `imap` crate via `spawn_blocking`, 30-second poll interval, `~` home dir expansion, HTML/quoted-text stripping); SMTP send via `lettre` with tokio1; configured via `EMAIL_IMAP_HOST`, `EMAIL_SMTP_HOST`, `EMAIL_USERNAME`, `EMAIL_PASSWORD`, and optional `EMAIL_IMAP_PORT`, `EMAIL_SMTP_PORT`, `EMAIL_INBOX`, `EMAIL_FROM_ADDRESS`
- Slack Socket Mode channel (`SlackSocketModeChannel`): self-connecting WebSocket via `tokio-tungstenite`; calls `apps.connections.open` to get WSS URL; handles `hello`, `events_api` (with envelope ack), and `disconnect` frames with exponential backoff reconnection; configured via `SLACK_APP_TOKEN` + `SLACK_MODE=socket`
- `config check` now prints `llm_provider` and summarization status

### Changed
- `ChannelsConfig` extended with `sms`, `webhook`, `webchat`, and `email` optional fields
- `SlackConfig` extended with `app_token: Option<String>` and `mode: String` (default `"webhook"`)
- `MemoryConfig` extended with `sqlite_path`, `postgres_url`, `vector_store`, `vector_store_url`, `embedding_provider`, `embedding_model`, and `summarization` fields
- `AgentsConfig` extended with `aws_region: Option<String>`
- Gateway `start()` now wires SMS, webhook, webchat, email, and Slack Socket Mode channels
- HTTP `AppState` and `HttpServerConfig` extended with `sms_tx`, `sms_auth_token`, `webhook_endpoints`, `webhook_tx`, `webchat_sessions`, `webchat_tx`
- `handle_message_inner` now calls `MemorySummarizer::maybe_summarize` (fire-and-forget) after adding the user message

## [0.5.0] - 2026-03-18

### Added
- Real Prometheus `/metrics` endpoint replacing the hand-rolled JSON response; exposes `rustynail_messages_in_total`, `rustynail_messages_out_total`, `rustynail_active_users`, `rustynail_healthy_channels`, and `rustynail_message_duration_seconds` (histogram with default buckets); content-type `text/plain; version=0.0.4`
- Redis memory store (`RedisStore`) implementing `MemoryStore` via `redis` blocking client; configured via `REDIS_URL` + `REDIS_TTL_SECONDS` env vars or `memory.redis_url` + `memory.redis_ttl_seconds` YAML keys; graceful fallback to in-memory on connection failure
- `MemoryConfig` section in config (`memory.backend`, `memory.redis_url`, `memory.redis_ttl_seconds`); `MEMORY_BACKEND=redis` switches backends at runtime
- Telegram long-poll mode (`TELEGRAM_MODE=longpoll` or `channels.telegram.mode: longpoll`); `TelegramLongPollChannel` spawns a `getUpdates?timeout=30` loop with automatic offset tracking and 5-second backoff on error
- Calendar tool (`CalendarTool`): `create`, `list`, `get`, `delete`, `upcoming` operations backed by a local JSON file in `RUSTYNAIL_DATA_DIR` (default `~/.rustynail/calendar.json`)
- Message formatter tool (`FormatterTool`): pure-Rust `to_markdown`, `to_plain`, `truncate`, `wrap`, `summarize_header` operations; useful for adapting content across Discord/WhatsApp/Slack formatting conventions
- Dashboard live WebSocket at `GET /dashboard/ws`; streams `stats_update` (every 5 s) and `message_event` (on each inbound/outbound message) JSON payloads; dashboard HTML updated with auto-reconnecting WebSocket JS block
- CLI subcommands via `clap` derive macros: `rustynail start` (default), `rustynail status [--port N]`, `rustynail version`, `rustynail config check`
- Message processing duration fed into the `rustynail_message_duration_seconds` Prometheus histogram in `handle_message_inner`

### Changed
- `Dockerfile` runtime stage switched from `debian:bookworm-slim` to `gcr.io/distroless/cc-debian12` (no shell, no package manager); CA certificates copied explicitly from builder stage; image now runs as distroless `nonroot` user (uid 65532)
- `axum` dependency updated to enable the `ws` feature for WebSocket support
- `MessageStats` extended with Prometheus metric handles and a `broadcast::Sender<DashboardEvent>`; `record_inbound_async` and `record_outbound_async` now also increment Prometheus counters and broadcast `MessageEvent`

## [0.4.5] - 2026-03-18

### Added
- Integration test suite: 25 new tests covering all HTTP endpoints (`/health`, `/status`, `/metrics`, `/ready`, `/live`), all three webhook paths (WhatsApp, Telegram, Slack), dashboard endpoints with auth, user preferences, and a full inbound→mock-Anthropic→outbound message-flow pipeline
- `tests/common/mod.rs`: shared test helpers (`make_test_state`, `make_test_state_with_webhooks`, `RecordingChannel`) and `tests/fixtures/test_config.yaml`
- `rustynail::gateway::handle_message_for_test` public entry point for integration tests
- `api_base: Option<String>` field on `AgentsConfig`; loaded from `ANTHROPIC_API_BASE` env var; overrides production Anthropic URL in both `ConversationalAgent` and `PlanningAgent` constructions

### Changed
- `DISCORD_BOT_TOKEN` is now optional in `Config::load()` — absent means no Discord channel; all other channels remain optional; only `ANTHROPIC_API_KEY` is required for a production start
- `config.yaml.example`: added commented `api_base` field under `agents`
- CI workflow: `cargo test` step now passes `ANTHROPIC_API_KEY=test_unused` so Tier 1 integration tests run in CI without secrets

## [0.4.1] - 2026-03-17

### Fixed
- `DiscordChannel::health()` always returned `ChannelHealth::Healthy`; it now reads the actual stored health state via `blocking_read()`

### Changed
- `DiscordChannel::message()` handler no longer clones the `Message` before passing it to `mpsc::send()` (needless allocation)
- `parse_update()` in `telegram.rs` simplifies `text.as_ref()?.clone()` to the idiomatic `text.clone()?`
- `ToolRegistry` now derives `Default`; `new()` delegates to `Self::default()`
- `MessageStats` fields (`messages_in`, `messages_out`, `start_instant`, `start_time`) are now private; public accessor methods `messages_in()`, `messages_out()`, and `start_time()` replace direct field access
- Duplicate `ChannelStatus` struct in `dashboard.rs` removed; the module now re-uses `http::ChannelStatus`
- `WebSearchTool::execute()` no longer allocates a `String` for the `search_depth` parameter (kept as `&str`)
- `test_success_response` in `web_search.rs` replaced with `test_response_parsing` that directly tests the JSON-parsing logic without a dead mockito server
- Channel adapters (discord, whatsapp, telegram, slack) use named `const NAME` rather than inline string literals in `name()` implementations
- WhatsApp Graph API version extracted to `const GRAPH_API_VERSION` in `whatsapp.rs`
- Agent system prompt extracted to `const SYSTEM_PROMPT` in `agents/manager.rs`
- Doc comments added to `HttpServerConfig`, `AppState`, `MessageStats`, `DashboardData`, `RecentMessage`, `ToolRegistry`, `CalculatorTool`, `FileSystemTool`, `WebSearchTool`

## [0.4.0] - 2026-03-17

### Added
- Multi-stage `Dockerfile` (`rust:1.82-slim-bookworm` builder → `debian:bookworm-slim` runtime) for producing a minimal production image
- `docker-compose.yml` for local development; build context is the parent directory to resolve the local `agenkit` path dependency
- `.dockerignore` excluding `target/`, `.env`, `.git/`, and `diary/` from the build context
- GitHub Actions CI workflow (`.github/workflows/ci.yml`): runs `cargo fmt --check`, `cargo clippy -D warnings`, and `cargo test` on push/PR to `main`; dual checkout of `rustynail` + `agenkit` sibling repos
- GitHub Actions Docker workflow (`.github/workflows/docker.yml`): builds and pushes to `ghcr.io/scttfrdmn/rustynail` on `v*` tag push
- Web monitoring dashboard at `GET /dashboard` (embedded HTML/CSS/JS, no CDN) with 30-second auto-refresh
- `GET /dashboard/data` JSON endpoint returning version, uptime, message counters, active users, channel health, and a recent-messages ring buffer (last 50)
- `MessageStats` struct with atomic counters (`messages_in`, `messages_out`) and a `RwLock<VecDeque<RecentMessage>>` ring buffer; threaded through the gateway message loop and `handle_message_inner`
- Optional HTTP basic auth on dashboard endpoints via `DASHBOARD_AUTH_PASSWORD` env var; credentials are `rustynail:<password>`
- `DashboardConfig` struct in the configuration system with `DASHBOARD_AUTH_PASSWORD` env var support
- `HttpServerConfig` struct replacing the 10-positional-argument `start_http_server` signature

## [0.3.0] - 2026-03-17

### Added
- Telegram Bot API channel adapter: webhook receive (POST with `X-Telegram-Bot-Api-Secret-Token` auth) and `sendMessage` REST send
- Slack Events API channel adapter: webhook receive (HMAC-SHA256 signature verification, `url_verification` challenge handling) and `chat.postMessage` REST send
- Web search tool via Tavily API (`web_search`): `query`, `max_results`, and `search_depth` parameters; registered when `TAVILY_API_KEY` is set
- Planning agent (`/plan <task>` prefix routes to `PlanningAgent`); activated by `agents.planning_enabled = true` / `AGENTS_PLANNING_ENABLED=true`
- Optional OpenTelemetry distributed tracing via OTLP exporter; enabled by `OTEL_EXPORTER_OTLP_ENDPOINT`; `gateway.handle_message` and `agent.process` spans emitted

## [0.2.0] - 2026-03-17

### Added
- Tool registry system backed by agenkit `Tool` trait; agents upgraded to `ReActAgent` when tools are registered and `tools.enabled = true`
- Calculator tool: add, sub, mul, div, pow, sqrt, abs, floor, ceil, round operations
- FileSystem tool: read, write, list, exists — sandboxed to a configured root path (rejects path traversal)
- WhatsApp Cloud API channel adapter: webhook receive (GET verify + POST events) and REST send via Graph API v18.0
- Cross-channel message routing: per-user preferred response channel via `POST /users/:id/preferences`; query via `GET /users/:id/preferences`
- Gateway now owns its internal message channel; `message_sender()` method for external channel adapters (Discord, etc.)
- `ToolsConfig` and `WhatsAppConfig` added to the configuration system (env vars + YAML)

## [0.1.0] - 2026-02-01

### Added
- Multi-channel architecture with pluggable `Channel` trait
- Discord integration using Serenity library (message receive/send, attachments, health monitoring)
- Agenkit-Rust integration with Claude 3.5 Sonnet via `AnthropicAgent` adapter
- `ConversationalAgent` wrapper for per-user conversation history management
- `AgentManager` for per-user agent lifecycle (automatic creation, thread-safe, isolated history)
- HTTP server with 5 production endpoints via Axum:
  - `GET /health` — basic health check for load balancers
  - `GET /status` — detailed system status with channel health and active user count
  - `GET /metrics` — operational metrics (Prometheus-compatible)
  - `GET /ready` — Kubernetes readiness probe (503 when not ready)
  - `GET /live` — Kubernetes liveness probe
- `MemoryStore` trait with `InMemoryStore` implementation (per-user history, auto-trimming)
- Configuration system with YAML file and environment variable support, sensible defaults
- Gateway core: channel registry, message router, event bus (tokio broadcast), graceful shutdown
- Structured logging with `tracing` and `tracing-subscriber`
- README with architecture diagrams, quick start, and HTTP endpoint documentation

[Unreleased]: https://github.com/scttfrdmn/rustynail/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/scttfrdmn/rustynail/compare/v0.13.0...v0.15.0
[0.14.0]: https://github.com/scttfrdmn/rustynail/compare/v0.13.0...v0.15.0
[0.13.0]: https://github.com/scttfrdmn/rustynail/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/scttfrdmn/rustynail/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/scttfrdmn/rustynail/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/scttfrdmn/rustynail/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/scttfrdmn/rustynail/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/scttfrdmn/rustynail/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/scttfrdmn/rustynail/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/scttfrdmn/rustynail/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/scttfrdmn/rustynail/compare/v0.4.5...v0.5.0
[0.4.5]: https://github.com/scttfrdmn/rustynail/compare/v0.4.1...v0.4.5
[0.4.1]: https://github.com/scttfrdmn/rustynail/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/scttfrdmn/rustynail/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/scttfrdmn/rustynail/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/scttfrdmn/rustynail/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/scttfrdmn/rustynail/releases/tag/v0.1.0
