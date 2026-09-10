-- Exercise loop exits and header effects that survive SSA simplification.
local trace = {}
local function visit(value)
    table.insert(trace, tostring(value))
    return value
end

local function first(limit, stop)
    for i = 1, limit do
        visit(i)
        if i == stop then
            return i, "found"
        end
    end
    return 0, "missing"
end

local function immediate(limit)
    for i = 1, limit do
        return i, i + 1
    end
    return -1
end

print(first(0, 2))
print(first(4, 2))
print(first(3, 5))
print(immediate(0))
print(immediate(3))

local function invert(limit)
    local i = 0
    while not (i >= limit) do
        i += 1
        visit(i)
        if i == 2 then
            break
        end
    end
    return i
end
print(invert(0), invert(1), invert(5))

local function headerEffects(limit)
    local i = 0
    while visit(i) < limit do
        i += 1
        if i % 2 == 0 then
            continue
        end
        visit(-i)
    end
    return i
end
print(headerEffects(0), headerEffects(4))

local function cycle(limit)
    local i = 0
    while true do
        i += 1
        if i == limit then
            break
        end
        visit(i)
    end
    return i
end
print(cycle(1), cycle(4))

local thread = coroutine.create(function()
    while true do
        coroutine.yield("tick")
    end
end)
for _ = 1, 3 do
    print(coroutine.resume(thread))
end
print(table.concat(trace, ","))

-- The iterator exit is a join even though the outer cycle never returns.
local genericThread = coroutine.create(function()
    local total = 0
    while true do
        coroutine.yield("before", total)
        for _, value in { 2, 5, 7 } do
            total += value
        end
        coroutine.yield("after", total)
    end
end)
for _ = 1, 5 do
    print(coroutine.resume(genericThread))
end

local numericThread = coroutine.create(function()
    while true do
        coroutine.yield("numeric-before")
        for i = 3, 1, -1 do
            coroutine.yield("numeric-body", i)
        end
        coroutine.yield("numeric-after")
    end
end)
for _ = 1, 7 do
    print(coroutine.resume(numericThread))
end

-- The re-entry tail may return or retry, without an exhaustion sentinel.
local function retryFields(fields)
    local attempts = 0
    while true do
        local key = ""
        for _, name in { "a", "b" } do
            key ..= fields[name] or ""
        end
        attempts += 1
        if key == "" then
            return attempts
        elseif fields.a then
            fields.a = nil
        elseif fields.b then
            fields.b = nil
        else
            return -1
        end
    end
end
print(retryFields({}), retryFields({ a = "A" }), retryFields({ a = "A", b = "B" }))

-- A cycle has two terminal cleanup paths, one of which joins a bypass arm.
local function scan(useFast, hits)
    local ignored = { "base" }
    if useFast then
        return hits[1] and hits[1].id or "miss", hits[1] ~= nil, #ignored
    end
    local initialCount = #ignored
    local cursor = 0
    repeat
        cursor += 1
        local hit = hits[cursor]
        if hit then
            if hit.solid then
                for i = #ignored, initialCount + 1, -1 do
                    ignored[i] = nil
                end
                return hit.id, true, #ignored
            end
            ignored[#ignored + 1] = hit.id
        end
    until not hit
    for i = #ignored, initialCount + 1, -1 do
        ignored[i] = nil
    end
    return "miss", false, #ignored
end
print(scan(true, {}))
print(scan(true, { { id = "fast" } }))
print(scan(false, {}))
print(scan(false, { { id = "soft" }, { id = "solid", solid = true } }))
print(scan(false, { { id = "soft1" }, { id = "soft2" } }))

-- A header branch continues the outer cycle; the other branch may return.
local function idleCycle()
    local ticks = 0
    local lastActive = 0
    while true do
        ticks += 1
        visit(ticks)
        if ticks <= 2 then
            lastActive = ticks
        else
            local idle = ticks - lastActive
            if idle > 3 then
                return ticks, idle
            end
            if idle > 1 then
                visit(-idle)
            end
        end
    end
end
print(idleCycle())
