function run_matmul(n_max::Int64)
    t = 0.0
    checksum = 0.0
    for n in 1:n_max
        a = [t t+1.0 t+2.0 t+3.0;
             t+4.0 t+5.0 t+6.0 t+7.0;
             t+8.0 t+9.0 t+10.0 t+11.0;
             t+12.0 t+13.0 t+14.0 t+15.0]
        b = [t+1.0 t t+3.0 t+2.0;
             t+5.0 t+4.0 t+7.0 t+6.0;
             t+9.0 t+8.0 t+11.0 t+10.0;
             t+13.0 t+12.0 t+15.0 t+14.0]
        c = a * b
        checksum += c[1, 1]
        t += 0.0001
    end
    return checksum
end
