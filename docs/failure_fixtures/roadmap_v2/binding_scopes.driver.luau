local first, second, initial = f(4, function(label, value) print(label, value) end)
print(first(), second(), initial)
