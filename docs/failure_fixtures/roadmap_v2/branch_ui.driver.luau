local children = {}
local scope = {}
function scope:New(class)
    print("new",class)
    return function(props) print("construct",props.Name); return props end
end
local function label(props) print("label",props.Text); return props end
for _,enabled in {true,false} do
 local root=f(scope,children,label,"Caption",enabled)
 print(root.Name,root.BackgroundTransparency,root[children][1].Text)
end
