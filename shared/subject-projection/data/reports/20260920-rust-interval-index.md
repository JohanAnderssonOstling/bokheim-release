# Rust LCC interval index

Replaced subclass-filtered point scans and full-vector interval scans with a
static interval tree. The taxonomy and matching precedence are unchanged.

The index is built with the immutable matcher during compilation, and when
building a downloaded overlay. Nodes are sorted by lower class letters and
number; each slice's midpoint is its implicit balanced-tree root. Each node
contains only two range IDs: its own range and the range with the greatest
upper bound in its subtree. Endpoints and Cutter components remain in the
original range vector, not duplicated in the index.

A query prunes subtrees whose lowest start is later than the incoming start,
or whose greatest end is earlier than the incoming end. A point uses identical
start/end bounds. Numeric containment deliberately returns a conservative
candidate set: the existing matcher still checks Cutter families and applies
ancestry, exact-selector, specificity, and ambiguity rules. Query traversal
uses the call stack and a visitor; it allocates no candidate iterator or list.
It replaces the old subclass-to-range-position maps rather than retaining both.

This is a search optimization, not a new taxonomy model. Matching descendants
can still beat narrower ancestor ranges. Whole input intervals require complete
containment. Existing parser/recovery behavior remains in place. The private
bundled binary representation is rebuilt with the application; the semantic
matcher version does not change.

## Validation

- Differential tests compare tree candidates with linear containment for
  overlapping, nested, equal, cross-subclass, empty, single-node, and infinite
  bounds. They also round-trip serialization.
- The existing bundled-matcher test verifies deterministic compilation and
  byte-for-byte encoding.
- Production release binaries before and after the change were compared on
  **64,678 inputs**, using one frozen taxonomy: exact LCC/BISAC/DDC selectors,
  stored range endpoints, whole intervals, and earlier review probes. Candidates
  and resolved IDs were identical. Three alternating runs also produced identical
  complete CSV hashes.
- A pre-existing test expectation for `Creative Thinking` was updated to the
  already-curated name `Creativity`; the frozen baseline had that name too.

`cargo test --release -p subject-projection` passed all **280 tests** (261 unit
and 19 integration). Two manual audit/benchmark tests are ignored in that normal
run; the candidate benchmark was invoked separately and passed.

## Measurement

The mixed production-probe workload had median process times of **3.888 seconds
before** and **1.106 seconds after** across three runs. Those times include
taxonomy loading, runtime construction, parsing, matching, and CSV output. The
workload deliberately contains many full-range inputs, so this is **not a
prediction of ordinary library scan speed**. Other workspace builds were active;
the baseline's slow first run was 7.0 seconds.

The ignored `benchmark_curated_candidate_lookup` test isolates candidate search
on every stored range start and every whole stored range. It alternates old and
new search order, discards one warmup, reports six-run medians, and verifies equal
candidate counts and ID checksums. It also reports serialized index sizes.

On 13,644 queries, point-search medians were **16.782 ms → 9.911 ms**
(about 41% less time). Whole-range medians were **1.285 s → 10.204 ms**
(about 126× faster). These are candidate-search costs, excluding parsing, Cutter
checks and ranking. The serialized index grew from **44,296 to 80,735 bytes**
(+36,439 bytes); its native node array occupies 218,304 bytes. The parsed range
vector already existed and is shared by either search method.

Reproduce with:

```sh
cargo test --release -p subject-projection --lib benchmark_curated_candidate_lookup -- --ignored --nocapture
```

Local artifacts: [frozen taxonomy](../../../../target/lcc-interval-index/taxonomy.sqlite3),
[comparison inputs](../../../../target/lcc-interval-index/probes.json),
[comparison script](../../../../target/lcc-interval-index/compare.py), and
[timings and output hash](../../../../target/lcc-interval-index/comparison.json).
The script uses the separately saved baseline and interval release executables
in that same directory.
