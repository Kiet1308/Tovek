local function run()
	local cache = {}

	local function rebuild(floors)
		cache = {}

		if not floors then
			return
		end

		for floorIndex, floor in floors do
			if floor and floor.items then
				for key, item in floor.items do
					if item and item.active and item.data then
						table.insert(cache, {
							floorIndex = floorIndex,
							key = key,
						})
					end
				end
			end
		end
	end

	while task.wait(1) do
		rebuild(getFloors())

		for i = 1, #cache do
			print(cache[i])
		end
	end
end

task.spawn(run)
