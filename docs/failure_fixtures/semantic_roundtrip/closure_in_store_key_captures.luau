-- A closure in a store key (`handlers[function() x = 5 end] = true`) writes
-- `x`; the capture census skipped store addresses, so `x = nil` looked
-- redundant and was deleted.
local function f(handlers)
    local x
    handlers[function()
        x = 5
    end] = true
    for callback in handlers do
        callback()
    end
    x = nil
    return x
end
print(f({}))
