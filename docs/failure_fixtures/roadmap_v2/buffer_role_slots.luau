return function(buf, bitOffset, bitCount, value, source, targetOffset, sourceOffset, count)
    buffer.writebits(buf, bitOffset, bitCount, value)
    buffer.copy(buf, targetOffset, source, sourceOffset, count)
    return buffer.readbits(buf, bitOffset, bitCount), buffer.tostring(buf)
end
