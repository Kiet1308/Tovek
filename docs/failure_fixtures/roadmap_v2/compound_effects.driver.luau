local first, second = {value = 10}, {value = 100}
local trace = {}
local record = setmetatable({}, {__index = function()
    table.insert(trace, "read")
    return if #trace == 1 then first else second
end})
f("base", record, 0)
print("base", #trace, first.value, second.value)
for _, mode in {"key", "unary"} do
    local target, count = {10, 20}, 0
    local function step()
        count += 1
        return count
    end
    local key = setmetatable({}, {__add = step, __unm = step})
    f(mode, target, key)
    print(mode, count, target[1], target[2])
end
local calls = 0
local key = setmetatable({}, {__add = function()
    calls += 1
    if calls == 2 then error("second-key") end
    return 1
end})
local ok, err = pcall(f, "key", {10, 20}, key)
print("error", calls, ok, string.find(tostring(err), "second-key", 1, true) ~= nil)
