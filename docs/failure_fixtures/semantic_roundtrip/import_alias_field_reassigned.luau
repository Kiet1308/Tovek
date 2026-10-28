-- An alias of a script table's field keeps the old function after the field
-- is reassigned; only fixed library members fold into their calls.
-- The companion prelude initializes Shared in a separate chunk.
local f = Shared.fn
Shared.fn = function() return "new" end
print(f())
local g = Shared.fn
local function swap() Shared.fn = function() return "newer" end end
swap()
print(g(), Shared.fn())
