return function(choose, value)
    if choose then
        return Vector3.new(value, value, value)
    else
        return (Vector3.new)(value, value, value)
    end
end
