-- Events.lua: SSA reuses the result register for a helper-local default,
-- then overwrites it only on the successful branch. Rebuilding a call must
-- keep the time/cache/offset/validation order and truncate a call leaf once.
local trace = {}
local tick = 1000
local offset = 0
local accept = false
local failure = ""
local function note(label)
    trace[#trace + 1] = label
    if failure == label then error("fault:" .. label, 0) end
end
-- Keep the service lookup opaque, as workspace:GetServerTimeNow is in Events.
local clock = table.clone({now = function()
    note("time")
    tick += 1
    return tick, "extra"
end})
local data = setmetatable({}, {__index = function(_, key)
    assert(key == "Offset")
    note("offset")
    return offset
end})
local Store = {get = function(_, player)
    note("cache")
    trace[#trace + 1] = tostring(player)
    return data
end}
local Validation = {check = function(_, value)
    note("validate")
    trace[#trace + 1] = tostring(value)
    return accept
end}

local function adjusted(player)
    if not player then return clock.now() end
    local cached = Store:get(player)
    local now = clock.now()
    local candidate = now + cached.Offset
    if Validation:check(candidate) then return candidate end
    return now
end

local function stamp(player)
    local result = adjusted(player)
    return {value = result, count = select("#", result)}
end

-- A result observed during its computation must remain its original cell.
-- In particular, moving its first store to the end would make check() see
-- nil rather than the just-read clock value, including when check throws.
local function observed(player)
    local result
    local original = Validation.check
    Validation.check = function(self, value)
        trace[#trace + 1] = "observed:" .. tostring(result)
        return original(self, value)
    end
    if player then
        local cached = Store:get(player)
        result = clock.now()
        local candidate = result + cached.Offset
        if Validation:check(candidate) then result = candidate end
    else
        result = clock.now()
    end
    Validation.check = original
    return {value = result, count = select("#", result)}
end

local normalCheck = Validation.check
local players = {false, 42, "player"} -- index 4 supplies nil
for i = 1, 4 do
    for _, delta in {0, -500, 500, math.huge, 0 / 0} do
        for _, valid in {false, true} do
            for _, fault in {"", "cache", "time", "offset", "validate"} do
                for _, run in {stamp, observed} do
                    trace, tick, offset, accept, failure = {}, 1000, delta, valid, fault
                    Validation.check = normalCheck
                    local ok, result = pcall(run, players[i])
                    if ok then
                        print(i, tostring(delta), valid, fault, result.value, result.count, table.concat(trace, ","))
                    else
                        print(i, tostring(delta), valid, fault, "error", result, table.concat(trace, ","))
                    end
                end
            end
        end
    end
end

-- The helper's untruncated call still returns two values on its false path.
failure = ""
print("arity", select("#", adjusted(false)), select("#", adjusted(42)))
