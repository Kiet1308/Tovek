-- Explicit array keys still consume constructor hash syntax when list items
-- follow. Folding the later fields must not grow the original hash allocation.
local function observe(tag, t)
    print(tag, #t)
    for key, value in next, t do print(type(key), key, value) end
end

local mixed = {[1] = 11, [true] = 22, 33}
mixed.k = nil
mixed.extra = 44
observe("mixed", mixed)

local duplicate = {[-0] = 11, [1] = 22, [-0] = 33, [2] = nil, 44, 55, 66}
duplicate.x = 77
observe("duplicate", duplicate)

local record = {[1] = 11, [2] = 22, label = 33, 44}
record.extra = 55
record.more = 66
observe("record", record)
