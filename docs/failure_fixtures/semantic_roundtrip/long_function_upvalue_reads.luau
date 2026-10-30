-- A long function reading an upvalue in every branch: each read is a value
-- defined at entry, so liveness precomputed for every local was quadratic
-- (49 s and 210 MiB at 5000 branches); it is now found per local on demand.
local f = function(v)
	return v % 13
end
local function g(x, y)
	local t = {}
	do
		local v = (x + 0) * y
		if v > 0 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 1) * y
		if v > 3 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 2) * y
		if v > 6 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 3) * y
		if v > 9 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 4) * y
		if v > 12 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 5) * y
		if v > 15 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 6) * y
		if v > 18 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 7) * y
		if v > 21 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 8) * y
		if v > 24 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 9) * y
		if v > 27 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 10) * y
		if v > 30 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 11) * y
		if v > 33 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 12) * y
		if v > 36 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 13) * y
		if v > 39 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 14) * y
		if v > 42 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 15) * y
		if v > 45 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 16) * y
		if v > 48 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 17) * y
		if v > 51 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 18) * y
		if v > 54 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 19) * y
		if v > 57 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 20) * y
		if v > 60 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 21) * y
		if v > 63 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 22) * y
		if v > 66 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 23) * y
		if v > 69 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 24) * y
		if v > 72 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 25) * y
		if v > 75 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 26) * y
		if v > 78 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 27) * y
		if v > 81 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 28) * y
		if v > 84 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 29) * y
		if v > 87 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 30) * y
		if v > 90 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 31) * y
		if v > 93 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 32) * y
		if v > 96 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 33) * y
		if v > 99 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 34) * y
		if v > 102 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 35) * y
		if v > 105 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 36) * y
		if v > 108 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 37) * y
		if v > 111 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 38) * y
		if v > 114 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 39) * y
		if v > 117 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 40) * y
		if v > 120 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 41) * y
		if v > 123 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 42) * y
		if v > 126 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 43) * y
		if v > 129 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 44) * y
		if v > 132 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 45) * y
		if v > 135 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 46) * y
		if v > 138 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 47) * y
		if v > 141 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 48) * y
		if v > 144 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 49) * y
		if v > 147 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 50) * y
		if v > 150 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 51) * y
		if v > 153 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 52) * y
		if v > 156 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 53) * y
		if v > 159 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 54) * y
		if v > 162 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 55) * y
		if v > 165 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 56) * y
		if v > 168 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 57) * y
		if v > 171 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 58) * y
		if v > 174 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 59) * y
		if v > 177 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 60) * y
		if v > 180 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 61) * y
		if v > 183 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 62) * y
		if v > 186 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 63) * y
		if v > 189 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 64) * y
		if v > 192 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 65) * y
		if v > 195 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 66) * y
		if v > 198 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 67) * y
		if v > 201 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 68) * y
		if v > 204 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 69) * y
		if v > 207 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 70) * y
		if v > 210 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 71) * y
		if v > 213 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 72) * y
		if v > 216 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 73) * y
		if v > 219 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 74) * y
		if v > 222 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 75) * y
		if v > 225 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 76) * y
		if v > 228 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 77) * y
		if v > 231 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 78) * y
		if v > 234 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 79) * y
		if v > 237 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 80) * y
		if v > 240 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 81) * y
		if v > 243 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 82) * y
		if v > 246 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 83) * y
		if v > 249 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 84) * y
		if v > 252 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 85) * y
		if v > 255 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 86) * y
		if v > 258 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 87) * y
		if v > 261 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 88) * y
		if v > 264 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 89) * y
		if v > 267 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 90) * y
		if v > 270 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 91) * y
		if v > 273 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 92) * y
		if v > 276 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 93) * y
		if v > 279 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 94) * y
		if v > 282 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 95) * y
		if v > 285 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 96) * y
		if v > 288 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 97) * y
		if v > 291 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 98) * y
		if v > 294 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 99) * y
		if v > 297 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 100) * y
		if v > 300 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 101) * y
		if v > 303 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 102) * y
		if v > 306 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 103) * y
		if v > 309 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 104) * y
		if v > 312 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 105) * y
		if v > 315 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 106) * y
		if v > 318 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 107) * y
		if v > 321 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 108) * y
		if v > 324 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 109) * y
		if v > 327 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 110) * y
		if v > 330 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 111) * y
		if v > 333 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 112) * y
		if v > 336 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 113) * y
		if v > 339 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 114) * y
		if v > 342 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 115) * y
		if v > 345 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 116) * y
		if v > 348 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 117) * y
		if v > 351 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 118) * y
		if v > 354 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 119) * y
		if v > 357 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 120) * y
		if v > 360 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 121) * y
		if v > 363 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 122) * y
		if v > 366 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 123) * y
		if v > 369 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 124) * y
		if v > 372 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 125) * y
		if v > 375 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 126) * y
		if v > 378 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 127) * y
		if v > 381 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 128) * y
		if v > 384 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 129) * y
		if v > 387 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 130) * y
		if v > 390 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 131) * y
		if v > 393 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 132) * y
		if v > 396 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 133) * y
		if v > 399 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 134) * y
		if v > 402 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 135) * y
		if v > 405 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 136) * y
		if v > 408 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 137) * y
		if v > 411 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 138) * y
		if v > 414 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 139) * y
		if v > 417 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 140) * y
		if v > 420 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 141) * y
		if v > 423 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 142) * y
		if v > 426 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 143) * y
		if v > 429 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 144) * y
		if v > 432 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 145) * y
		if v > 435 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 146) * y
		if v > 438 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 147) * y
		if v > 441 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 148) * y
		if v > 444 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 149) * y
		if v > 447 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 150) * y
		if v > 450 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 151) * y
		if v > 453 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 152) * y
		if v > 456 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 153) * y
		if v > 459 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 154) * y
		if v > 462 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 155) * y
		if v > 465 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 156) * y
		if v > 468 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 157) * y
		if v > 471 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 158) * y
		if v > 474 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 159) * y
		if v > 477 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 160) * y
		if v > 480 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 161) * y
		if v > 483 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 162) * y
		if v > 486 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 163) * y
		if v > 489 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 164) * y
		if v > 492 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 165) * y
		if v > 495 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 166) * y
		if v > 498 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 167) * y
		if v > 501 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 168) * y
		if v > 504 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 169) * y
		if v > 507 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 170) * y
		if v > 510 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 171) * y
		if v > 513 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 172) * y
		if v > 516 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 173) * y
		if v > 519 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 174) * y
		if v > 522 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 175) * y
		if v > 525 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 176) * y
		if v > 528 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 177) * y
		if v > 531 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 178) * y
		if v > 534 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 179) * y
		if v > 537 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 180) * y
		if v > 540 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 181) * y
		if v > 543 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 182) * y
		if v > 546 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 183) * y
		if v > 549 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 184) * y
		if v > 552 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 185) * y
		if v > 555 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 186) * y
		if v > 558 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 187) * y
		if v > 561 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 188) * y
		if v > 564 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 189) * y
		if v > 567 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 190) * y
		if v > 570 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 191) * y
		if v > 573 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 192) * y
		if v > 576 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 193) * y
		if v > 579 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 194) * y
		if v > 582 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 195) * y
		if v > 585 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 196) * y
		if v > 588 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 197) * y
		if v > 591 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 198) * y
		if v > 594 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 199) * y
		if v > 597 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 200) * y
		if v > 600 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 201) * y
		if v > 603 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 202) * y
		if v > 606 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 203) * y
		if v > 609 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 204) * y
		if v > 612 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 205) * y
		if v > 615 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 206) * y
		if v > 618 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 207) * y
		if v > 621 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 208) * y
		if v > 624 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 209) * y
		if v > 627 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 210) * y
		if v > 630 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 211) * y
		if v > 633 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 212) * y
		if v > 636 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 213) * y
		if v > 639 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 214) * y
		if v > 642 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 215) * y
		if v > 645 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 216) * y
		if v > 648 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 217) * y
		if v > 651 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 218) * y
		if v > 654 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 219) * y
		if v > 657 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 220) * y
		if v > 660 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 221) * y
		if v > 663 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 222) * y
		if v > 666 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 223) * y
		if v > 669 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 224) * y
		if v > 672 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 225) * y
		if v > 675 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 226) * y
		if v > 678 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 227) * y
		if v > 681 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 228) * y
		if v > 684 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 229) * y
		if v > 687 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 230) * y
		if v > 690 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 231) * y
		if v > 693 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 232) * y
		if v > 696 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 233) * y
		if v > 699 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 234) * y
		if v > 702 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 235) * y
		if v > 705 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 236) * y
		if v > 708 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 237) * y
		if v > 711 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 238) * y
		if v > 714 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 239) * y
		if v > 717 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 240) * y
		if v > 720 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 241) * y
		if v > 723 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 242) * y
		if v > 726 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 243) * y
		if v > 729 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 244) * y
		if v > 732 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 245) * y
		if v > 735 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 246) * y
		if v > 738 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 247) * y
		if v > 741 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 248) * y
		if v > 744 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 249) * y
		if v > 747 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 250) * y
		if v > 750 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 251) * y
		if v > 753 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 252) * y
		if v > 756 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 253) * y
		if v > 759 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 254) * y
		if v > 762 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 255) * y
		if v > 765 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 256) * y
		if v > 768 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 257) * y
		if v > 771 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 258) * y
		if v > 774 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 259) * y
		if v > 777 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 260) * y
		if v > 780 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 261) * y
		if v > 783 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 262) * y
		if v > 786 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 263) * y
		if v > 789 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 264) * y
		if v > 792 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 265) * y
		if v > 795 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 266) * y
		if v > 798 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 267) * y
		if v > 801 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 268) * y
		if v > 804 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 269) * y
		if v > 807 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 270) * y
		if v > 810 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 271) * y
		if v > 813 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 272) * y
		if v > 816 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 273) * y
		if v > 819 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 274) * y
		if v > 822 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 275) * y
		if v > 825 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 276) * y
		if v > 828 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 277) * y
		if v > 831 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 278) * y
		if v > 834 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 279) * y
		if v > 837 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 280) * y
		if v > 840 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 281) * y
		if v > 843 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 282) * y
		if v > 846 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 283) * y
		if v > 849 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 284) * y
		if v > 852 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 285) * y
		if v > 855 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 286) * y
		if v > 858 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 287) * y
		if v > 861 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 288) * y
		if v > 864 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 289) * y
		if v > 867 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 290) * y
		if v > 870 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 291) * y
		if v > 873 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 292) * y
		if v > 876 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 293) * y
		if v > 879 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 294) * y
		if v > 882 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 295) * y
		if v > 885 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 296) * y
		if v > 888 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 297) * y
		if v > 891 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 298) * y
		if v > 894 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 299) * y
		if v > 897 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 300) * y
		if v > 900 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 301) * y
		if v > 903 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 302) * y
		if v > 906 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 303) * y
		if v > 909 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 304) * y
		if v > 912 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 305) * y
		if v > 915 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 306) * y
		if v > 918 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 307) * y
		if v > 921 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 308) * y
		if v > 924 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 309) * y
		if v > 927 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 310) * y
		if v > 930 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 311) * y
		if v > 933 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 312) * y
		if v > 936 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 313) * y
		if v > 939 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 314) * y
		if v > 942 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 315) * y
		if v > 945 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 316) * y
		if v > 948 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 317) * y
		if v > 951 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 318) * y
		if v > 954 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 319) * y
		if v > 957 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 320) * y
		if v > 960 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 321) * y
		if v > 963 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 322) * y
		if v > 966 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 323) * y
		if v > 969 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 324) * y
		if v > 972 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 325) * y
		if v > 975 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 326) * y
		if v > 978 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 327) * y
		if v > 981 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 328) * y
		if v > 984 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 329) * y
		if v > 987 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 330) * y
		if v > 990 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 331) * y
		if v > 993 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 332) * y
		if v > 996 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 333) * y
		if v > 999 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 334) * y
		if v > 1002 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 335) * y
		if v > 1005 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 336) * y
		if v > 1008 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 337) * y
		if v > 1011 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 338) * y
		if v > 1014 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 339) * y
		if v > 1017 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 340) * y
		if v > 1020 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 341) * y
		if v > 1023 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 342) * y
		if v > 1026 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 343) * y
		if v > 1029 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 344) * y
		if v > 1032 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 345) * y
		if v > 1035 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 346) * y
		if v > 1038 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 347) * y
		if v > 1041 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 348) * y
		if v > 1044 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 349) * y
		if v > 1047 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 350) * y
		if v > 1050 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 351) * y
		if v > 1053 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 352) * y
		if v > 1056 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 353) * y
		if v > 1059 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 354) * y
		if v > 1062 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 355) * y
		if v > 1065 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 356) * y
		if v > 1068 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 357) * y
		if v > 1071 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 358) * y
		if v > 1074 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 359) * y
		if v > 1077 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 360) * y
		if v > 1080 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 361) * y
		if v > 1083 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 362) * y
		if v > 1086 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 363) * y
		if v > 1089 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 364) * y
		if v > 1092 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 365) * y
		if v > 1095 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 366) * y
		if v > 1098 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 367) * y
		if v > 1101 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 368) * y
		if v > 1104 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 369) * y
		if v > 1107 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 370) * y
		if v > 1110 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 371) * y
		if v > 1113 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 372) * y
		if v > 1116 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 373) * y
		if v > 1119 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 374) * y
		if v > 1122 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 375) * y
		if v > 1125 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 376) * y
		if v > 1128 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 377) * y
		if v > 1131 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 378) * y
		if v > 1134 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 379) * y
		if v > 1137 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 380) * y
		if v > 1140 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 381) * y
		if v > 1143 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 382) * y
		if v > 1146 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 383) * y
		if v > 1149 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 384) * y
		if v > 1152 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 385) * y
		if v > 1155 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 386) * y
		if v > 1158 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 387) * y
		if v > 1161 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 388) * y
		if v > 1164 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 389) * y
		if v > 1167 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 390) * y
		if v > 1170 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 391) * y
		if v > 1173 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 392) * y
		if v > 1176 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 393) * y
		if v > 1179 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 394) * y
		if v > 1182 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 395) * y
		if v > 1185 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 396) * y
		if v > 1188 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 397) * y
		if v > 1191 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 398) * y
		if v > 1194 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 399) * y
		if v > 1197 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 400) * y
		if v > 1200 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 401) * y
		if v > 1203 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 402) * y
		if v > 1206 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 403) * y
		if v > 1209 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 404) * y
		if v > 1212 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 405) * y
		if v > 1215 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 406) * y
		if v > 1218 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 407) * y
		if v > 1221 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 408) * y
		if v > 1224 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 409) * y
		if v > 1227 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 410) * y
		if v > 1230 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 411) * y
		if v > 1233 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 412) * y
		if v > 1236 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 413) * y
		if v > 1239 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 414) * y
		if v > 1242 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 415) * y
		if v > 1245 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 416) * y
		if v > 1248 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 417) * y
		if v > 1251 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 418) * y
		if v > 1254 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 419) * y
		if v > 1257 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 420) * y
		if v > 1260 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 421) * y
		if v > 1263 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 422) * y
		if v > 1266 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 423) * y
		if v > 1269 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 424) * y
		if v > 1272 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 425) * y
		if v > 1275 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 426) * y
		if v > 1278 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 427) * y
		if v > 1281 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 428) * y
		if v > 1284 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 429) * y
		if v > 1287 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 430) * y
		if v > 1290 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 431) * y
		if v > 1293 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 432) * y
		if v > 1296 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 433) * y
		if v > 1299 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 434) * y
		if v > 1302 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 435) * y
		if v > 1305 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 436) * y
		if v > 1308 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 437) * y
		if v > 1311 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 438) * y
		if v > 1314 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 439) * y
		if v > 1317 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 440) * y
		if v > 1320 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 441) * y
		if v > 1323 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 442) * y
		if v > 1326 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 443) * y
		if v > 1329 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 444) * y
		if v > 1332 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 445) * y
		if v > 1335 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 446) * y
		if v > 1338 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 447) * y
		if v > 1341 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 448) * y
		if v > 1344 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 449) * y
		if v > 1347 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 450) * y
		if v > 1350 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 451) * y
		if v > 1353 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 452) * y
		if v > 1356 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 453) * y
		if v > 1359 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 454) * y
		if v > 1362 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 455) * y
		if v > 1365 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 456) * y
		if v > 1368 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 457) * y
		if v > 1371 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 458) * y
		if v > 1374 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 459) * y
		if v > 1377 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 460) * y
		if v > 1380 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 461) * y
		if v > 1383 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 462) * y
		if v > 1386 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 463) * y
		if v > 1389 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 464) * y
		if v > 1392 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 465) * y
		if v > 1395 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 466) * y
		if v > 1398 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 467) * y
		if v > 1401 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 468) * y
		if v > 1404 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 469) * y
		if v > 1407 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 470) * y
		if v > 1410 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 471) * y
		if v > 1413 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 472) * y
		if v > 1416 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 473) * y
		if v > 1419 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 474) * y
		if v > 1422 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 475) * y
		if v > 1425 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 476) * y
		if v > 1428 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 477) * y
		if v > 1431 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 478) * y
		if v > 1434 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 479) * y
		if v > 1437 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 480) * y
		if v > 1440 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 481) * y
		if v > 1443 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 482) * y
		if v > 1446 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 483) * y
		if v > 1449 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 484) * y
		if v > 1452 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 485) * y
		if v > 1455 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 486) * y
		if v > 1458 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 487) * y
		if v > 1461 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 488) * y
		if v > 1464 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 489) * y
		if v > 1467 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 490) * y
		if v > 1470 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 491) * y
		if v > 1473 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 492) * y
		if v > 1476 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 493) * y
		if v > 1479 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 494) * y
		if v > 1482 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 495) * y
		if v > 1485 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 496) * y
		if v > 1488 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 497) * y
		if v > 1491 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 498) * y
		if v > 1494 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 499) * y
		if v > 1497 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 500) * y
		if v > 1500 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 501) * y
		if v > 1503 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 502) * y
		if v > 1506 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 503) * y
		if v > 1509 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 504) * y
		if v > 1512 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 505) * y
		if v > 1515 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 506) * y
		if v > 1518 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 507) * y
		if v > 1521 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 508) * y
		if v > 1524 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 509) * y
		if v > 1527 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 510) * y
		if v > 1530 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 511) * y
		if v > 1533 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 512) * y
		if v > 1536 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 513) * y
		if v > 1539 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 514) * y
		if v > 1542 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 515) * y
		if v > 1545 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 516) * y
		if v > 1548 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 517) * y
		if v > 1551 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 518) * y
		if v > 1554 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 519) * y
		if v > 1557 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 520) * y
		if v > 1560 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 521) * y
		if v > 1563 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 522) * y
		if v > 1566 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 523) * y
		if v > 1569 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 524) * y
		if v > 1572 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 525) * y
		if v > 1575 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 526) * y
		if v > 1578 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 527) * y
		if v > 1581 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 528) * y
		if v > 1584 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 529) * y
		if v > 1587 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 530) * y
		if v > 1590 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 531) * y
		if v > 1593 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 532) * y
		if v > 1596 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 533) * y
		if v > 1599 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 534) * y
		if v > 1602 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 535) * y
		if v > 1605 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 536) * y
		if v > 1608 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 537) * y
		if v > 1611 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 538) * y
		if v > 1614 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 539) * y
		if v > 1617 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 540) * y
		if v > 1620 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 541) * y
		if v > 1623 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 542) * y
		if v > 1626 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 543) * y
		if v > 1629 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 544) * y
		if v > 1632 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 545) * y
		if v > 1635 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 546) * y
		if v > 1638 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 547) * y
		if v > 1641 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 548) * y
		if v > 1644 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 549) * y
		if v > 1647 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 550) * y
		if v > 1650 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 551) * y
		if v > 1653 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 552) * y
		if v > 1656 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 553) * y
		if v > 1659 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 554) * y
		if v > 1662 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 555) * y
		if v > 1665 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 556) * y
		if v > 1668 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 557) * y
		if v > 1671 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 558) * y
		if v > 1674 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 559) * y
		if v > 1677 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 560) * y
		if v > 1680 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 561) * y
		if v > 1683 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 562) * y
		if v > 1686 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 563) * y
		if v > 1689 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 564) * y
		if v > 1692 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 565) * y
		if v > 1695 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 566) * y
		if v > 1698 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 567) * y
		if v > 1701 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 568) * y
		if v > 1704 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 569) * y
		if v > 1707 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 570) * y
		if v > 1710 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 571) * y
		if v > 1713 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 572) * y
		if v > 1716 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 573) * y
		if v > 1719 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 574) * y
		if v > 1722 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 575) * y
		if v > 1725 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 576) * y
		if v > 1728 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 577) * y
		if v > 1731 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 578) * y
		if v > 1734 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 579) * y
		if v > 1737 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 580) * y
		if v > 1740 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 581) * y
		if v > 1743 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 582) * y
		if v > 1746 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 583) * y
		if v > 1749 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 584) * y
		if v > 1752 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 585) * y
		if v > 1755 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 586) * y
		if v > 1758 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 587) * y
		if v > 1761 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 588) * y
		if v > 1764 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 589) * y
		if v > 1767 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 590) * y
		if v > 1770 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 591) * y
		if v > 1773 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 592) * y
		if v > 1776 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 593) * y
		if v > 1779 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 594) * y
		if v > 1782 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 595) * y
		if v > 1785 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 596) * y
		if v > 1788 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 597) * y
		if v > 1791 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 598) * y
		if v > 1794 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	do
		local v = (x + 599) * y
		if v > 1797 then
			t[#t + 1] = v
		else
			t[#t + 1] = f(v)
		end
	end
	return t
end
local r = g(2, 3)
local sum = 0
for _, v in r do
	sum += v
end
print(#r, sum, r[1], r[600])
