#!/usr/bin/env bash
# Live, no-cherry-picking Nirdosha-vs-Julia-vs-C benchmark runner.
#
# What this does differently from just reading benchmarks/RESULTS.md:
#   - Prints every individual timing sample (not a silently-picked "best of
#     N") plus min/median/max, so nothing is hidden.
#   - Verifies every language's output numerically (within a tolerance, to
#     allow for float-printing/summation-order differences) BEFORE trusting
#     any timing for that benchmark -- a mismatch is a hard failure, not a
#     footnote.
#   - Times Nirdosha's `nirdosha build` step separately from running the
#     compiled binary, and reports it alongside the run times -- Julia's
#     number bundles JIT compile + run in one process because that's how
#     `julia script.jl` is actually used; Nirdosha's AOT compile is a
#     separate, one-time step because that's how `nirdosha build` is
#     actually used. Both are printed so nobody has to trust a single
#     framing of what's "fair" -- read the breakdown and decide.
#   - Prints the machine/toolchain versions actually used for this run, not
#     copied from a prior write-up.
#
#   - Optionally (WARM=1) also reports Julia's steady-state number: a
#     full-size warmup call, discarded, then several timed calls in the
#     SAME process. This isn't cosmetic -- an earlier version of this
#     measurement warmed up with a tiny call (n=2) and still showed ~50-60%
#     GC time on the first full-size call afterward (heap growth the small
#     warmup never triggered), making "warm" look 3x worse than steady
#     state actually is. Warming with the real problem size fixes that;
#     see the comment in benchmarks/julia/*_warm.jl.
#
# Usage: RUNS=5 WARM=1 ./benchmarks/run_head_to_head.sh [benchmark ...]
#   Default: 5 runs per language per benchmark, all six benchmarks, warm
#   mode off (cold-only, matches benchmarks/RESULTS.md's main table).

set -euo pipefail

RUNS="${RUNS:-5}"
WARM="${WARM:-0}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [ "$#" -gt 0 ]; then
    BENCHMARKS=("$@")
else
    BENCHMARKS=(matmul det dot kalman fib floatloop)
fi

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

echo "================================================================"
echo "Environment (this run, not copied from any prior write-up)"
echo "================================================================"
echo "Date (UTC):  $(date -u)"
echo "CPU:         $(grep 'model name' /proc/cpuinfo | head -1 | cut -d: -f2 | sed 's/^ //')"
echo "Cores:       $(nproc)"
echo "Cur. clock:  $(( $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq 2>/dev/null || echo 0) / 1000 )) MHz (cpu0, may throttle under load)"
echo "Julia:       $(julia --version)"
echo "gcc:         $(gcc --version | head -1)"
echo "Nirdosha:    $(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown) ($(git -C "$REPO_ROOT" log -1 --format=%cd --date=short 2>/dev/null || echo unknown))"
echo

echo "================================================================"
echo "Build"
echo "================================================================"
if [ ! -x "$REPO_ROOT/compiler/target/release/nirdosha" ]; then
    echo "Building nirdosha --release (not found)..."
    (cd "$REPO_ROOT/compiler" && cargo build --release)
fi
NIRDOSHA_BIN="$REPO_ROOT/compiler/target/release/nirdosha"
echo "nirdosha binary: $NIRDOSHA_BIN"
echo

# --- helpers ---------------------------------------------------------

# Numeric-tolerant compare of two float-ish strings, LINE BY LINE (kalman
# prints two lines -- x[0] and x[1] -- comparing the whole blob as one
# float silently fails to parse and falls back to exact-string comparison,
# which then "mismatches" on nothing but print-precision differences; that
# was a real bug caught while writing this script, not a hypothetical).
# Returns 0 (all lines match within tolerance) or 1 (mismatch), tolerance
# 1e-4 relative (loose enough for summation-order differences across three
# independently-written implementations, tight enough to catch an actual
# bug).
numeric_match() {
    python3 -c "
import sys
a_lines = sys.argv[1].strip().splitlines()
b_lines = sys.argv[2].strip().splitlines()
if len(a_lines) != len(b_lines):
    sys.exit(1)
for a, b in zip(a_lines, b_lines):
    try:
        fa, fb = float(a), float(b)
    except ValueError:
        if a.strip() != b.strip():
            sys.exit(1)
        continue
    tol = 1e-4 * max(1.0, abs(fa), abs(fb))
    if abs(fa - fb) > tol:
        sys.exit(1)
sys.exit(0)
" "$1" "$2"
}

# Run a command RUNS times, print every sample, then min/median/max.
# Sets globals: LAST_OUT (stdout of the final run), SAMPLES (bash array).
run_n_times() {
    SAMPLES=()
    LAST_OUT=""
    local i t0 t1 dt
    for ((i = 1; i <= RUNS; i++)); do
        t0=$(date +%s.%N)
        LAST_OUT="$("$@")"
        t1=$(date +%s.%N)
        dt=$(echo "$t1 - $t0" | bc)
        SAMPLES+=("$dt")
        printf "    run %d: %ss\n" "$i" "$dt"
    done
    local sorted
    IFS=$'\n' sorted=($(sort -n <<<"${SAMPLES[*]}")); unset IFS
    local n=${#sorted[@]}
    local mid=$((n / 2))
    printf "    -> min=%ss  median=%ss  max=%ss\n" "${sorted[0]}" "${sorted[$mid]}" "${sorted[$((n - 1))]}"
}

# --- per-benchmark run -------------------------------------------------

for f in "${BENCHMARKS[@]}"; do
    echo "================================================================"
    echo "$f"
    echo "================================================================"

    c_bin="$WORKDIR/${f}_c"
    nir_bin="$WORKDIR/${f}_nir"

    gcc -O2 -o "$c_bin" "benchmarks/c/${f}.c" -lm

    echo "  compiling with nirdosha build..."
    t0=$(date +%s.%N)
    "$NIRDOSHA_BIN" build "benchmarks/nirdosha/${f}.nir" -o "$nir_bin" >/dev/null
    t1=$(date +%s.%N)
    nir_compile_s=$(echo "$t1 - $t0" | bc)
    echo "  nirdosha build time: ${nir_compile_s}s (one-time, not part of per-run timing below)"
    echo

    echo "  -- C (gcc -O2), $RUNS runs --"
    run_n_times "$c_bin"
    c_out="$LAST_OUT"

    echo "  -- Nirdosha (already-compiled binary), $RUNS runs --"
    run_n_times "$nir_bin"
    nir_out="$LAST_OUT"

    echo "  -- Julia (cold: 'julia ${f}.jl' -- JIT compile + run + process startup, all included), $RUNS runs --"
    run_n_times julia "benchmarks/julia/${f}.jl"
    julia_out="$LAST_OUT"
    IFS=$'\n' julia_cold_sorted=($(sort -n <<<"${SAMPLES[*]}")); unset IFS
    julia_cold_median="${julia_cold_sorted[$((${#julia_cold_sorted[@]} / 2))]}"

    echo
    echo "  -- correctness (must agree within 1e-4 relative tolerance before any timing above is trusted) --"
    echo "    C:        $c_out"
    echo "    Nirdosha: $nir_out"
    echo "    Julia:    $julia_out"

    ok=1
    if ! numeric_match "$c_out" "$nir_out"; then
        echo "    !! MISMATCH: C vs Nirdosha differ beyond tolerance"
        ok=0
    fi
    if ! numeric_match "$c_out" "$julia_out"; then
        echo "    !! MISMATCH: C vs Julia differ beyond tolerance"
        ok=0
    fi
    if [ "$ok" -eq 1 ]; then
        echo "    OK -- all three agree, timings above are trustworthy for this benchmark."
    else
        echo "    !! DO NOT TRUST THE TIMINGS ABOVE for $f -- outputs disagree, the programs aren't computing the same thing."
    fi
    echo

    if [ "$WARM" = "1" ] && [ -f "benchmarks/julia/${f}_warm.jl" ]; then
        echo "  -- Julia (WARM/steady-state: 1 full-size warmup call, discarded, then timed calls in the same process) --"
        warm_raw="$(julia "benchmarks/julia/${f}_warm.jl")"
        echo "$warm_raw" | grep '^timed_call_' | sed 's/^timed_call_\([0-9]*\)_elapsed_s=/    call \1: /' | sed 's/$/s/'
        mapfile -t warm_samples < <(echo "$warm_raw" | sed -n 's/^timed_call_[0-9]*_elapsed_s=//p')
        IFS=$'\n' warm_sorted=($(sort -n <<<"${warm_samples[*]}")); unset IFS
        wn=${#warm_sorted[@]}
        wmid=$((wn / 2))
        printf "    -> min=%ss  median=%ss  max=%ss  (%d timed calls, after 1 full-size warmup)\n" \
            "${warm_sorted[0]}" "${warm_sorted[$wmid]}" "${warm_sorted[$((wn - 1))]}" "$wn"

        warm_out="$(echo "$warm_raw" | sed -n 's/^result[^=]*=//p')"
        echo "    warm result: $warm_out"
        if numeric_match "$c_out" "$warm_out"; then
            echo "    OK -- warm-mode result agrees with C/Nirdosha within tolerance."
        else
            echo "    !! MISMATCH -- warm-mode result disagrees with C/Nirdosha; do not trust the warm timing for $f."
        fi
        echo "    Julia cold median: ${julia_cold_median}s  |  warm/steady-state median: ${warm_sorted[$wmid]}s  |  speedup: $(python3 -c "print(f'{${julia_cold_median} / ${warm_sorted[$wmid]}:.1f}x')")"
        echo
    fi
done
