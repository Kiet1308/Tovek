-- Events.lua regression: an early loop exit shares a RETURN with an outer
-- branch, while normal exhaustion has a separate return/continuation.
-- Compare actual effects, return tuples and captured iteration cells at O0/O1/O2.
local trace = {}
local function record(value)
    trace[#trace + 1] = tostring(value)
end

local function generic(mode, items, limit)
    if mode == 1 then
        for _, value in ipairs(items) do
            if value > limit then break end
            record(value)
        end
    elseif mode == 2 then
        record("other")
        return
    else
        record("fallback")
    end
end

local function numeric(mode, items, limit)
    if mode == 1 then
        for i = 1, #items do
            if items[i] > limit then break end
            record(items[i])
        end
    elseif mode == 2 then
        record("other")
        return
    else
        record("fallback")
    end
end

local function whileLoop(mode, items, limit)
    if mode == 1 then
        local i = 1
        while i <= #items do
            if items[i] > limit then break end
            record(items[i])
            i += 1
        end
    elseif mode == 2 then
        record("other")
        return
    else
        record("fallback")
    end
end

local function repeatLoop(mode, items, limit)
    if mode == 1 then
        local i = 0
        repeat
            i += 1
            if i > #items or items[i] > limit then break end
            record(items[i])
        until i >= #items
    elseif mode == 2 then
        record("other")
        return
    else
        record("fallback")
    end
end

local function finish(value)
    record("finish")
    return value, nil, "last"
end

local function sequential(mode, items, limit)
    local result = "outer"
    if mode == 1 then
        for _, value in ipairs(items) do
            result = value
            if value > limit then return finish(result) end
            record(value)
        end
        record("between")
        for i = 1, #items do
            result = items[i] * 2
            if result > limit then return finish(result) end
            record(result)
        end
        record("exhausted")
        return "normal"
    end
    return finish(result)
end

local function nested(mode, items, limit)
    local readers = {}
    if mode == 1 then
        for i, value in ipairs(items) do
            readers[#readers + 1] = function() return i, value end
            for j = 1, 2 do
                if value * j > limit then
                    record("early")
                    return readers
                end
                record(value * j)
            end
        end
        record("exhausted")
        return readers
    end
    record("outer")
    return readers
end

for _, items in ipairs({{}, {1}, {1, 2, 3}}) do
    for mode = 0, 2 do
        for _, limit in ipairs({0, 2, 10}) do
            for _, run in ipairs({generic, numeric, whileLoop, repeatLoop, sequential}) do
                trace = {}
                local a, b, c = run(mode, items, limit)
                print(#items, mode, limit, table.concat(trace, ","), a, b, c)
            end
            trace = {}
            local readers = nested(mode, items, limit)
            for _, read in ipairs(readers) do
                local i, value = read()
                record(i .. ":" .. value)
            end
            print(#items, mode, limit, table.concat(trace, ","))
        end
    end
end
