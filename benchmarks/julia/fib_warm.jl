include("lib/fib.jl")

N = 35
WARMUP_CALLS = 1
TIMED_CALLS = 5

for _ in 1:WARMUP_CALLS
    fib(N)
end

result = 0
for i in 1:TIMED_CALLS
    elapsed = @elapsed (global result = fib(N))
    println("timed_call_$(i)_elapsed_s=", elapsed)
end
println("result=", result)
