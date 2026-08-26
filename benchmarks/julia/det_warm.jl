include("lib/det.jl")

N = 200000
WARMUP_CALLS = 1
TIMED_CALLS = 5

for _ in 1:WARMUP_CALLS
    run_det(N)
end

result = 0.0
for i in 1:TIMED_CALLS
    elapsed = @elapsed (global result = run_det(N))
    println("timed_call_$(i)_elapsed_s=", elapsed)
end
println("result=", result)
