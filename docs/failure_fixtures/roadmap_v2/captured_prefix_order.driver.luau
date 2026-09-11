for _, mode in { "base", "key", "tuple", "argument" } do
    for _, mutate in { false, true } do
        for _, input in { 7, false, "nil" } do
            local result = table.pack(f(function(replace)
                if mutate then replace() end
                if input == "nil" then return nil end
                return input
            end, function(...)
                return ...
            end, mode))
            print(mode, mutate, input, result.n, result[1], result[2])
        end
    end
end
