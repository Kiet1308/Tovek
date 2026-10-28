type = function(value) return "replacement-type", 101 end
typeof = function(value) return "replacement-typeof", 102 end
math = {abs = function(value) return "replacement-abs", 103 end}
string = {len = function(value) return "replacement-len", 104 end}
bit32 = {band = function(value, mask) return "replacement-band", 105 end}
