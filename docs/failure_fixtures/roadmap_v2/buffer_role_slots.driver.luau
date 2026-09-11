local value, bytes = f(buffer.create(4), 0, 8, 7, buffer.fromstring("ABCD"), 1, 0, 2)
print(value, string.byte(bytes, 1, 4))
local ok, err = pcall(f, buffer.create(4), 0, 33, 7, buffer.fromstring("ABCD"), 1, 0, 2)
print("error", ok, string.find(tostring(err), "bit count", 1, true) ~= nil)
