-- Reduced from a real VoidShop.luau (Sky dump): at -O2 Luau inlines `ensure`
-- into `wire`, so the local `scale` is assigned twice, but both writes come
-- before any closure copies it. Every copy holds the final value, so the four
-- inlined `tweenTo` bodies are rebuilt as calls. A write after a copy
-- (deinline_copy_capture_rebound) still refuses them.
local function ensure(holder)
	local scale = holder.scale
	if not scale then
		scale = { Scale = 1 }
		holder.scale = scale
	end
	return scale
end

local function wire(button, log)
	local scale = ensure(button)
	local hovering = false
	local function tweenTo(target, speed)
		log[#log + 1] = { scale, target, speed }
		scale.Scale = target
	end
	button.enter = function()
		hovering = true
		tweenTo(1.08, 1)
	end
	button.leave = function()
		hovering = false
		tweenTo(1, 1)
	end
	button.down = function()
		tweenTo(0.92, 2)
	end
	button.up = function()
		tweenTo(hovering and 1.08 or 1, 1)
	end
end

local button, log = {}, {}
wire(button, log)
button.enter()
button.down()
button.up()
button.leave()
for _, entry in log do
	print(entry[1] == button.scale, entry[2], entry[3])
end
print(button.scale.Scale)
