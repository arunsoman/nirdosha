include("lib/matmul.jl")

# Steady-state / "compiled" measurement: warm up with a FULL-SIZE call
# (200000, not a token n=2) -- a small warmup triggers JIT specialization
# but not the GC heap-growth cost the real workload pays on its first big
# run. Confirmed empirically: a `n=2` warmup left ~60% GC time on the next
# full call; a full-size warmup brings that under ~1.5%. See
# benchmarks/RESULTS.md's caveats section for why the main suite doesn't
# do this by default (it measures one-shot `julia script.jl` cost, which
# is what a user actually experiences running it once).
N = 200000
WARMUP_CALLS = 1
TIMED_CALLS = 5

for _ in 1:WARMUP_CALLS
    run_matmul(N)
end

result = 0.0
for i in 1:TIMED_CALLS
    elapsed = @elapsed (global result = run_matmul(N))
    println("timed_call_$(i)_elapsed_s=", elapsed)
end
println("result=", result)
