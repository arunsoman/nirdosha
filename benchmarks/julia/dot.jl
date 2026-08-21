using LinearAlgebra

function run_dot(n_max::Int64)
    t = 0.0
    checksum = 0.0
    for n in 1:n_max
        a = [t, t + 1.0, t + 2.0, t + 3.0, t + 4.0, t + 5.0, t + 6.0, t + 7.0]
        b = [t + 1.0, t, t + 3.0, t + 2.0, t + 5.0, t + 4.0, t + 7.0, t + 6.0]
        checksum += dot(a, b)
        t += 0.0001
    end
    return checksum
end

println(run_dot(200000))
