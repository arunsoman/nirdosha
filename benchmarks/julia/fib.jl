function fib(n::Int64)::Int64
    if n < 2
        return n
    end
    return fib(n - 1) + fib(n - 2)
end

println(fib(35))
