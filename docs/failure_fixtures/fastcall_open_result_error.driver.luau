local library = table
local reads = 0
getfenv(f.method).table = setmetatable({}, {__index = function(_, key)
    reads += 1
    return library[key]
end})
local ok = pcall(f.method, {}, {})
print("method", ok, reads)
for mode = 0, 4 do
    reads = 0
    local out = {}
    local ok = pcall(f.open, out, function()
        if mode == 0 then return end
        if mode == 1 then return 7 end
        if mode == 2 then return 1, 8 end
        if mode == 3 then return nil end
        error("argument failed", 0)
    end)
    print(mode, ok, reads, #out, out[1])
end
