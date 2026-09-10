for index, value in f() do
    local bytes = {}
    for position = 1, #value do bytes[position] = string.byte(value, position) end
    print(index, #value, table.concat(bytes, ","))
end
