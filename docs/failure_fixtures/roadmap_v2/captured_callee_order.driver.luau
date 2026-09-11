for _, mutate in { false, true } do
    for _, value in { 7, false, "nil" } do
        local result = table.pack(f(function(replace)
            if mutate then replace() end
            if value == "nil" then return nil end
            return value
        end))
        print(mutate, value, result.n, result[1], result[2])
    end
end
