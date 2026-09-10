-- Optional search: the incoming parameter survives a bypass, exhaustion
-- clears it, and a body-side break exports the selected iteration value.
local function selectValue(items, requested, fallback)
    if requested == nil then
        return fallback
    end
    local selected = nil
    if items then
        for _, value in items do
            if value == requested then
                selected = value
                break
            end
        end
    end
    fallback = selected
    return fallback
end
print(selectValue({ 2, 4, 6 }, nil, 99))
print(selectValue(nil, 4, 99))
print(selectValue({}, 4, 99))
print(selectValue({ 2, 4, 6 }, 4, 99))
print(selectValue({ 2, 4, 6 }, 8, 99))

-- Closures created after the loop retain a shared outer binding. Both reads
-- and later writes must refer to that cell, including nested closure bodies.
local function captureSelected(items, requested)
    local selected = nil
    for _, value in items do
        if value.id == requested then
            selected = value
            break
        end
    end
    local function read()
        return selected and selected.id or "missing"
    end
    local function nested()
        return function() return selected and selected.id or "missing" end
    end
    local function replace(value)
        selected = value
    end
    return read, nested(), replace
end
for _, requested in { "A", "B", "absent" } do
    local read, nested, replace = captureSelected({ { id = "A" }, { id = "B" } }, requested)
    print(read(), nested())
    replace({ id = "changed" })
    print(read(), nested())
    replace(nil)
    print(read(), nested())
end

-- A closure created before the loop can observe every assignment, rather
-- than only the final search result.
local function earlierObserver(items)
    local selected = "initial"
    local function read() return selected end
    local seen = {}
    for _, value in items do
        selected = value
        table.insert(seen, read())
    end
    return table.concat(seen, ","), read()
end
print(earlierObserver({ "x", "y" }))
print(earlierObserver({}))
