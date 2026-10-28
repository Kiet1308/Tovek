local trash
local function work(v)
 for i=1,15000 do trash={i,tostring(i)} end
 return v
end
local function observe(t)
 local parts={}
 for k,v in pairs(t) do parts[#parts+1]=k.."="..tostring(v) end
 return table.concat(parts,",")
end
local function test_3_5(condition)
 local t={Ref = 0,mount = 0,update = 0,reify = nil,teardown = work(7),reconcile = 0,setGlobalConfig = 0,UNSTABLE = 0}
 if condition then t.reify=1 else t.reify=2 end
 t.reconcile=3
 return observe(t)
end
print(3,5,test_3_5(true))
