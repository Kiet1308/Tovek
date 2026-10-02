-- `local new = Foo.new` raises where it stands when `Foo` is nil; folding the
-- alias into its call would print first.
-- alias of a global path whose root is nil: error must happen before print
local ok, err = pcall(function()
  local new = Foo.new
  print("between")
  return new(1)
end)
print(ok, err ~= nil)
