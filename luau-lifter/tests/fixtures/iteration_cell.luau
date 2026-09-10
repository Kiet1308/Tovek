local callbacks = {}
for _, value in ipairs({ 10, 20, 30 }) do
    value += 1
    callbacks[#callbacks + 1] = function() return value end
end
return callbacks
