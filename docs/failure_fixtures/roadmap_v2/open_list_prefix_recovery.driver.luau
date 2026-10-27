local function run(name, key, flag)
    print(name, flag)
    local ok, result = pcall(f[name], key, flag, 5, nil, 7)
    if ok then
        print("result", result[1], result[2], result[3], result[4], result[5], result.extra, result.second)
    else
        print("error", string.match(result, ":%d+: (.*)") or result)
    end
end

for _, name in {"keyed", "positional"} do
    run(name, "extra", true)
    run(name, 2, false)
    run(name, nil, true)
    run(name, 0 / 0, false)
end
