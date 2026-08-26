using LinearAlgebra

function run_kalman(n_max::Int64)
    t = 0.0
    x = [0.0, 0.0, 0.0, 0.0]
    P = Matrix{Float64}(I, 4, 4)
    F = [1.0 0.0 1.0 0.0;
         0.0 1.0 0.0 1.0;
         0.0 0.0 1.0 0.0;
         0.0 0.0 0.0 1.0]
    Q = Matrix{Float64}(0.01I, 4, 4)
    H = [1.0 0.0 0.0 0.0;
         0.0 1.0 0.0 0.0]
    R = [0.25 0.0; 0.0 0.25]

    for n in 1:n_max
        x1 = F * x
        P1 = F * P * F' + Q
        z = [t, t * 0.5]
        y = z - H * x1
        S = H * P1 * H' + R
        K = P1 * H' * inv(S)
        x2 = x1 + K * y
        P2 = (I - K * H) * P1
        x = x2
        P = P2
        t += 0.01
    end
    return x
end
