-- `("value=%*"):format(42)` as a statement must stay a call: an
-- interpolated string is no statement.
local function f()
    ("value=%*"):format(42)
    return 7
end
print(f())
