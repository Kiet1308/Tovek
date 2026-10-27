local function run(key, replacement, flag)
    local ok, result = pcall(f, key, replacement, flag, 2, nil, 3)
    print("ok", ok)
    if ok then
        print(result.before, result.after, result[false], result[1], result[2], result[3], result[4])
    else
        print(string.match(result, ":%d+: (.*)") or result)
    end
end
run("before", "after", true)
run(nil, "after", false)
run(0 / 0, "after", true)
run("before", nil, true)
run("before", 0 / 0, false)
run("before", false, false)
