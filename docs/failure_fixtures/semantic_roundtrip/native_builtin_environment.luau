-- Explicit scalar calls preserve the original FASTCALL and fallback arity.
local probes = {
    function(value) return (type(value)) end,
    function(value) return (typeof(value)) end,
    function(value) return (math.abs(value)) end,
    function(value) return (string.len(value)) end,
    function(value) return (bit32.band(value, 7)) end,
}
local values = {false, false, -17, "hello", 19}
for index, probe in ipairs(probes) do
    local ok, first = pcall(probe, values[index])
    print(index, ok, first)
end
