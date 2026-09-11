local function measures(record)
    local width = record.Width
    local height = record.Height
    return width, height
end
return function(leftRecord, rightRecord)
    local leftWidth, leftHeight = measures(leftRecord)
    local rightWidth, rightHeight = measures(rightRecord)
    return leftWidth, leftHeight, rightWidth, rightHeight
end
