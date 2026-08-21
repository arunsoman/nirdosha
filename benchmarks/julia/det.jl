using LinearAlgebra

function run_det(n_max::Int64)
    t = 0.0
    checksum = 0.0
    for n in 1:n_max
        m = [t+4.0 t+1.0 t+2.0 t+0.5;
             t+1.0 t+5.0 t+0.3 t+1.2;
             t+2.0 t+0.3 t+6.0 t+0.7;
             t+0.5 t+1.2 t+0.7 t+7.0]
        checksum += det(m)
        t += 0.0001
    end
    return checksum
end

println(run_det(200000))
