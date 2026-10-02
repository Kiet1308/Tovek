-- As first_read_captured_operand, in a statement helper.
local x = 1
local obj = {}
function obj.bump() x = x + 10; return 5 end
local function show(t)
  print("show", x, t, "end")
end
show(2)
print("show", x, obj.bump(), "end")
print(x)
