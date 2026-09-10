return function(self, _, v1, p1)
    local function callback(_)
        return self + _ + v1 + p1
    end
    return callback(_)
end
