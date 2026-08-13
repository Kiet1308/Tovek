-- Metatables & OOP: class via __index, __add/__call/__tostring/__eq, plus a
-- __index/__newindex logging proxy table.
local Vec = {}
Vec.__index = Vec
function Vec.new(x, y)
	local self = setmetatable({}, Vec)
	self.x, self.y = x, y
	return self
end
function Vec:add(o) return Vec.new(self.x + o.x, self.y + o.y) end
function Vec:__add(o) return self:add(o) end
function Vec:__tostring() return `({self.x},{self.y})` end
function Vec:__eq(o) return self.x == o.x and self.y == o.y end
function Vec:__call(n) return self.x * n + self.y end
local a = Vec.new(1, 2)
local b = Vec.new(3, 4)
local c = a + b
print(tostring(c), a:add(b).x)
print(c(2))
print(a == Vec.new(1, 2), a == b)
local storage = { public = 1 }
local events = {}
local mt = {
	__index = function(_, k) events[#events + 1] = "get:" .. tostring(k); return storage[k] end,
	__newindex = function(_, k, v) events[#events + 1] = "set:" .. tostring(k) .. "=" .. tostring(v); storage[k] = v end,
}
local proxy = setmetatable({}, mt)
print(proxy.public, proxy.missing)
proxy.public = 5
proxy.extra = 9
print(storage.public, storage.extra, table.concat(events, "|"))