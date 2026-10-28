local trash
require = function(value)
    for i = 1, 15000 do
        trash = { i, tostring(i) }
    end
    return value
end
script = { Ref = 1, mount = 2, update = 3, reify = 4, teardown = 5, reconcile = 6, setGlobalConfig = 7, UNSTABLE = 8 }
local object = {
    Ref = require(script.Ref),
    mount = require(script.mount),
    update = require(script.update),
    reify = require(script.reify),
    teardown = require(script.teardown),
    reconcile = require(script.reconcile),
    setGlobalConfig = require(script.setGlobalConfig),
    UNSTABLE = require(script.UNSTABLE),
}
local parts = {}
for key, value in pairs(object) do
    parts[#parts + 1] = key .. "=" .. tostring(value)
end
print(table.concat(parts, ","))
