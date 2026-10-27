-- Observers before the first store must not see invented nil keys.
local function put(t,k,v) t[k]=v end
local function visit(t) for k,v in pairs(t) do print(k,v) end end
local t={}
print(next(t))
print(pcall(function() return next(t,"ZUvxIjpsT") end))
local ok=pcall(next,t,{}); print(ok)
t.ZUvxIjpsT=0
print(next(t,"ZUvxIjpsT"))
put(t,"kRiMzFtsJ",1)
put(t,"NGCuccUFW",2)
put(t,"YVUFXYsXD",3)
put(t,"DYxXyxmof",4)
put(t,"lxXyKTHoR",5)
put(t,"WxrdAWctj",6)
put(t,"ELDxjLUAA",7)
visit(t)
put(t,"ZUvxIjpsT",nil)
visit(t)
