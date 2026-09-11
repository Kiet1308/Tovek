for _, mutable in { false, true } do
    for _, mutate in { false, true } do
        for _, input in { 7, false, "nil" } do
            local call = f(function(replace)
                if mutate then replace() end
                if input == "nil" then return nil end
                return input
            end, mutable)
            local result = table.pack(call())
            print(mutable, mutate, input, result.n, result[1], result[2])
        end
    end
end
