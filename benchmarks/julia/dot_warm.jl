include("lib/dot.jl")

# dot's GC time is unstable even fully warmed (observed swinging between
# ~2% and ~87% across otherwise-identical calls, run to run) -- more
# timed samples than the other benchmarks specifically to make that
# instability visible rather than accidentally picking a lucky call.
N = 200000
WARMUP_CALLS = 1
TIMED_CALLS = 8

for _ in 1:WARMUP_CALLS
    run_dot(N)
end

result = 0.0
for i in 1:TIMED_CALLS
    elapsed = @elapsed (global result = run_dot(N))
    println("timed_call_$(i)_elapsed_s=", elapsed)
end
println("result=", result)
