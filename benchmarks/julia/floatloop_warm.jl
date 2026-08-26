include("lib/floatloop.jl")

N = 200000000
WARMUP_CALLS = 1
TIMED_CALLS = 5

for _ in 1:WARMUP_CALLS
    run_floatloop(N)
end

result = 0.0
for i in 1:TIMED_CALLS
    elapsed = @elapsed (global result = run_floatloop(N))
    println("timed_call_$(i)_elapsed_s=", elapsed)
end
println("result=", result)
