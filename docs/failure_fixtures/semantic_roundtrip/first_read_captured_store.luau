-- A helper storing into a cell (`n = 5`) the moved argument reads
-- (`T.read()`): the store came first in the inlined copy.
local n = 0
local T = {}
function T.read() return n end
local function mark(p) n = 5; print("mark", p, n) end
mark(T.read())
n = 1
n = 5
print("mark", T.read(), n)
