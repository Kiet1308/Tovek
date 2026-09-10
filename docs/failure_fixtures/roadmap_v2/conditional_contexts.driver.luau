local function emit(label, value)
    print(label, value)
    return value, "extra"
end
for _, flag in {true, false} do
    local results = table.pack(f(flag, emit))
    print("results", results.n, results[1], results[2], results[3])
end
