local calls = 0
local function multiple()
    calls += 1
    return "tail", nil, 17
end
local function forward(...) return ... end
local spread, adjusted, spreadArray, adjustedArray, returned = f(forward, multiple)
for _, result in {spread, adjusted} do
    print(result.n, #result[1], #result[2], result[3], result[4], result[5])
end
print(spreadArray[3], spreadArray[4], spreadArray[5], adjustedArray[3], adjustedArray[4], adjustedArray[5])
local result = table.pack(returned())
print(result.n, result[3], result[4], calls)
