for _,value in {-2,0,4,math.huge,0/0} do print(f(value)) end
local mt={}
local function box(x) return setmetatable({n=x},mt) end
function mt.__lt(a,b) print("lt",a.n,b); return a.n<b end
function mt.__mul(a,b) print("mul",a.n,b); return a.n*b end
function mt.__add(a,b) print("add",a.n,b); return box(a.n+b) end
local success, result = pcall(f,box(4))
print("boxed_error",success,if success then result else result:match("attempt to compare table < number") ~= nil)
