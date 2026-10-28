local kind = type
local alternateKind = typeof

local function observe(label, ok, value)
    print(label, ok, value)
end

local function probe(object)
    local value = object.value
    print("index-value", value)
    return (kind(value))
end

local function capturedProbe(object)
    local value = object.value
    local read = function() return value end
    local observed = alternateKind(value)
    print("captured-kind", observed)
    return read
end

local function varargProbe(...)
    local value = ...
    print("vararg-value", value)
    return (kind(value))
end

local hits = 0
local object = setmetatable({}, {
    __index = function(_, key)
        hits += 1
        print("getter", key, hits)
        return 17
    end,
})
observe("index", pcall(probe, object))
local first = capturedProbe(object)
local second = capturedProbe(object)
print("closure-identity", first == second)
observe("capture-first", pcall(first))
observe("capture-second", pcall(second))
observe("varargs-zero", pcall(varargProbe))
observe("varargs-one", pcall(varargProbe, false))
observe("varargs-many", pcall(varargProbe, 17, "ignored", nil))

local throwing = setmetatable({}, {
    __index = function(_, key)
        print("throwing-getter", key)
        error("getter-error", 0)
    end,
})
observe("throwing", pcall(probe, throwing))

local entered = false
local recursive = setmetatable({}, {
    __index = function(_, key)
        print("reentrant-getter", key, entered)
        print("reentrant-frame", debug.info(2, "f") == probe)
        if not entered then
            entered = true
            observe("nested", pcall(probe, {value = false}))
            entered = false
        end
        return 23
    end,
})
observe("reentrant", pcall(probe, recursive))
print("getter-count", hits)

local function dynamicKey(object, key)
    local value = object[key]
    print("dynamic-key-value", value)
    return (kind(value))
end
observe("dynamic-key", pcall(dynamicKey, object, "selected"))

local function fromCall(callback)
    local value = callback()
    print("callback-value", value)
    return (kind(value))
end
observe("callback", pcall(fromCall, function() print("callback-effect"); return 37, "unused" end))

local mutable = 17
local function fromLocal(callback)
    callback()
    local value = mutable
    print("local-value", value)
    return (kind(value))
end
observe("local-copy", pcall(fromLocal, function() mutable = false end))

local function crowded(...)
    local v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16, v17, v18, v19, v20, v21, v22, v23, v24, v25, v26, v27, v28, v29, v30, v31, v32, v33, v34, v35, v36, v37, v38, v39, v40, v41, v42, v43, v44, v45, v46, v47, v48, v49, v50, v51, v52, v53, v54, v55, v56, v57, v58, v59, v60, v61, v62, v63, v64, v65, v66, v67, v68, v69, v70, v71, v72, v73, v74, v75, v76, v77, v78, v79, v80, v81, v82, v83, v84, v85, v86, v87, v88, v89, v90, v91, v92, v93, v94, v95, v96, v97, v98, v99, v100, v101, v102, v103, v104, v105, v106, v107, v108, v109, v110, v111, v112, v113, v114, v115, v116, v117, v118, v119, v120, v121, v122, v123, v124, v125, v126, v127, v128, v129, v130, v131, v132, v133, v134, v135, v136, v137, v138, v139, v140, v141, v142, v143, v144, v145, v146, v147, v148, v149, v150, v151, v152, v153, v154, v155, v156, v157, v158, v159, v160, v161, v162, v163, v164, v165, v166, v167, v168, v169, v170, v171, v172, v173, v174, v175, v176, v177, v178, v179, v180, v181, v182, v183, v184, v185, v186, v187, v188, v189 = ...
    local value = v0
    local observed = (type)(value)
    print("crowded-kind", observed)
    return {v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12, v13, v14, v15, v16, v17, v18, v19, v20, v21, v22, v23, v24, v25, v26, v27, v28, v29, v30, v31, v32, v33, v34, v35, v36, v37, v38, v39, v40, v41, v42, v43, v44, v45, v46, v47, v48, v49, v50, v51, v52, v53, v54, v55, v56, v57, v58, v59, v60, v61, v62, v63, v64, v65, v66, v67, v68, v69, v70, v71, v72, v73, v74, v75, v76, v77, v78, v79, v80, v81, v82, v83, v84, v85, v86, v87, v88, v89, v90, v91, v92, v93, v94, v95, v96, v97, v98, v99, v100, v101, v102, v103, v104, v105, v106, v107, v108, v109, v110, v111, v112, v113, v114, v115, v116, v117, v118, v119, v120, v121, v122, v123, v124, v125, v126, v127, v128, v129, v130, v131, v132, v133, v134, v135, v136, v137, v138, v139, v140, v141, v142, v143, v144, v145, v146, v147, v148, v149, v150, v151, v152, v153, v154, v155, v156, v157, v158, v159, v160, v161, v162, v163, v164, v165, v166, v167, v168, v169, v170, v171, v172, v173, v174, v175, v176, v177, v178, v179, v180, v181, v182, v183, v184, v185, v186, v187, v188, v189}
end
local crowdedValues = crowded(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160, 161, 162, 163, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 176, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189)
print("crowded-values", #crowdedValues, crowdedValues[1], crowdedValues[190])
