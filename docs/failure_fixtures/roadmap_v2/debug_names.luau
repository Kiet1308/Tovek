local function invoiceTotal(unitPrice: number, quantity: number, discountRate: number)
    local subtotal = unitPrice * quantity
    local discountAmount = subtotal * discountRate
    local totalCost = subtotal - discountAmount
    return totalCost, function() return discountAmount, totalCost end
end
return invoiceTotal
