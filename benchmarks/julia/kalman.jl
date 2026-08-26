include("lib/kalman.jl")

result = run_kalman(200000)
println(result[1])
println(result[2])
