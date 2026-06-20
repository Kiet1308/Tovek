local x, y = 0, 1
for _ = 1, 6 do
	x, y = y, x + y      -- simultaneous (Fibonacci)
end
print(x, y)              -- 8  13
