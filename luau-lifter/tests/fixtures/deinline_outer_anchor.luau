local particles = require(script.Parent.Particles)

-- Its only fixed name is the method; `particles` pins it down.
local function emit(instance)
	if instance then
		particles:AbsoluteEmit(instance)
	end
end

return function(scope, state, fx)
	local function purchase(amount)
		scope:doCleanup()
		if state.OnPurchase(amount) ~= false then
			scope:PlaySound("Buy", 0.1)
		end
	end

	-- Also matches the first statement of every `purchase` copy.
	local function cancel()
		scope:doCleanup()
	end

	state.OnAccept = purchase
	state.OnCancel = cancel
	emit(fx:FindFirstChild("Beams"))
	emit(fx.Smoke)
	if state.Quick then
		purchase(nil)
	end
end
