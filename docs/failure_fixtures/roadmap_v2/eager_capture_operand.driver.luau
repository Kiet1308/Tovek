local function invoke(callback) return callback() end
print(f(10, 20, invoke))
print(f(0, 20, invoke))
print(f(-7, 99, invoke))
