local trace = {}
local function record(label, width, height, throws)
    return setmetatable({}, {__index = function(_, key)
        table.insert(trace, label .. ":" .. key)
        if throws and key == "Height" then error("height-read") end
        if key == "Width" then return width end
        return height
    end})
end
local a, b, c, d = f(record("left", false, nil), record("right", 7, 9))
print(a, b, c, d, table.concat(trace, ","))
trace = {}
print("arity", select("#", f(record("left", nil, false), record("right", nil, nil))))
trace = {}
local ok, err = pcall(f, record("left", 1, 2), record("right", 3, 4, true))
print("error", ok, string.find(tostring(err), "height-read", 1, true) ~= nil, table.concat(trace, ","))
