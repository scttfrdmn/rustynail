# Vendored quarry host event-stream corpus

**Do not edit these files.** They are copied verbatim from quarry's
`testdata/runevents/`, which is the upstream contract both twins are asserted
against — the corpus README there names its consumers as *"**bucktooth** (Go) and
**rustynail** (Rust)"*.

| | |
|---|---|
| Source | `github.com/scttfrdmn/quarry`, `testdata/runevents/` |
| Producing commit | **`d066040`** (`71682ab` froze the corpus; `d066040` corrected the README's own provenance line) |
| Stream version | **1** |
| Producer | **`quarry-go`** |
| Vendored | 2026-08-04 |

Consumed by `tests/quarry_conformance.rs`.

## Why vendored rather than generated

Upstream is explicit that regenerating the corpus in CI would be *"worse than
none"*, and names the two reasons its own request for a generator script could not
be met:

- **The budget-degraded case is unreachable under `--fake`.** The fake's per-call
  cost is uniform, so affordability either funds every child or declines the split;
  a tree with *some* children priced out does not exist in that mode. Hence
  `live-partition` is a real Bedrock run, and it is also the only case carrying the
  float-sum and model-residual properties.
- **The time-truncated cases are wall-clock races**, and the shape already changed
  once on the capturing machine: the pair was first captured at 620ms/500ms giving 3
  and 4 gaps, and after `BudgetedSolver` began wrapping the leaf prompt — which
  changes the prompt hash, from which the fake derives its per-call latency — 620ms
  produced **zero** gaps and the whole band moved to ~185–195ms.

So determinism is claimed for the **fold**, which is pure, not for the runs. A
corpus we authored ourselves could not fail us in the ways a captured one does.

## Three files per case

| file | what it is |
|---|---|
| `<case>.json` | the run record in quarry's canonical (hashed) encoding — **captured**, once, by hand |
| `<case>.ndjson` | the framed stream `--events-json` emits for that record — **derived** |
| `<case>.expected.json` | the same stream restated in **integers**, for a host to check itself against — **derived** |

Every figure in `.expected.json` came off the wire, not out of the record, so
comparing our reading against it compares two readings of the same bytes.

## The cases

Ten, not the nine the upstream README's table lists — `deadline-only` is described
in that file's capture section and in its own note, but is missing from the table.

| case | what it pins |
|---|---|
| `complete` | the baseline. Its 3 rows already fail to sum in float64 |
| `deadline-only` | the **only** case with `cap_micros: -1`. Added upstream because the README stated that rule with no fixture behind it |
| `live-partition` | real Bedrock, 25 rows. Rows do not sum in float64 (1.4e-17); **42042 micro-units of model-spend residual, over half the run** |
| `time-truncated` | exit 3, 2 gaps, 0 unfunded, **and a partial answer worth showing** |
| `no-answer-time` | 4 gaps, `total_micros: 0`, **empty receipt**. Classified `no-answer`, not `time-truncated`, though `bound_by` is `latency` |
| `no-answer-spend` | the spend counterpart: 0 gaps, 1 unfunded. `"rows":[]` — **never `null`** |
| `unicode` | HTML escaping is off — `&` and `<` stay literal |
| `unicode-long` | the **60-rune** label boundary, with one label of 47 runes / 139 bytes that must **not** be cut |
| `unknown-kind` | synthetic: a `quarry_future_kind` event *between* answer and receipt. Also the only **round-dollar** costs, which serialize as `"cost":1` with no decimal point |
| `crashed` | synthetic: cut **mid-line**, inside the artifact event's JSON. **No `.expected.json`, deliberately** — the bytes do not parse into a complete set, so there is no expectation to state |

`unknown-kind` and `crashed` have no `.json`: they are facts about the framing, not
about a run.

## The rules these exist to enforce

1. **Money is integers.** Convert each row with `round(× 1e6)` *before* summing —
   never sum the floats. `complete` and `live-partition` both carry
   `float_sum_equals_total: false`.
2. **Absence is not zero**, in three places: a missing `provenance` object means
   quarry *declined to publish an estimate*, never that stability was 0;
   `cap_micros: -1` means no spend cap, not a cap of nothing; `bound_by: ""` means
   no cap bound the run and is emitted because it is a measurement.
3. **Gaps and unfunded are different denominations and must never be summed.** Only
   time produces a gap. A host that added them would offer more time where money was
   needed.

Lengths are **runes**, not bytes — both `answer_runes` and the 60-rune label limit.

## Staleness

Upstream warns that a commit hash in a corpus README goes stale silently. If a case
here disagrees with what quarry produces, trust
`git log -- testdata/runevents/<case>.json` upstream over the table above. If a
*derived* file changes upstream, that is either a wire change needing a version bump
or an accident — and either way this directory needs re-vendoring.
