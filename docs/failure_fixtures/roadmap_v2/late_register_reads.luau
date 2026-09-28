-- A register local read by an arithmetic, comparison or index operation is
-- read when the operation runs, after the other operand; a concatenation
-- copies it first, and a closure fetches its upvalue before the call.
return function(step)
    local total = 1
    local items = { "first" }
    local function bump()
        total = total * 10
        items = { "a", "b", "c", "d", "e" }
    end
    total = total + step(bump)
    local larger = total < step(bump)
    local item = items[step(bump)]
    local label = total .. step(bump)
    return total, larger, item, label, function()
        return total + step(bump)
    end
end
