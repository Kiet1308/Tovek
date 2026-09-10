local value=setmetatable({}, {
 __mul=function(_, index) print("multiply",index); return index * 3 end,
})
print("sum",f(value))
