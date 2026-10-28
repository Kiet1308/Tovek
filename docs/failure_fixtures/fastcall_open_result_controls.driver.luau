local library = table
local reads = 0
getfenv(f.varargs).table = setmetatable({}, {__index = function(_, key)
    reads += 1
    return library[key]
end})
local function values(mode)
    if mode == 0 then return end
    if mode == 1 then return 7 end
    if mode == 2 then return 1, 8 end
    if mode == 3 then return nil end
    error("argument failed", 0)
end
local function insert(out, ...)
    print("arguments", select("#", ...), ...)
    library.insert(out, ...)
end
for mode = 0, 3 do
    reads = 0
    local out = {}
    local ok = pcall(f.varargs, out, values(mode))
    print("varargs", mode, ok, reads, #out, out[1])
end
for mode = 0, 4 do
    local out = {}
    local ok = pcall(f.ordinary, insert, out, function() return values(mode) end)
    print("ordinary", mode, ok, #out, out[1])
end
