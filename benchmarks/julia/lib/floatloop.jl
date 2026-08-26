function run_floatloop(n_max::Int64)
    acc = 0.0
    i = 0
    while i < n_max
        acc = acc + 1.5
        acc = acc * 0.999999
        i += 1
    end
    return acc
end
