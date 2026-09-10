local children = {}
local scope = {}
function scope:New(class)
    print("new",class)
    return function(props) print("construct", props.Name); return props end
end
local function label(props) print("label",props.Text); return props end
local root=f(scope,children,label,"Caption",true)
print(root.Name,root.Visible,#root[children],root[children][1].Text,root[children][2].Name)
