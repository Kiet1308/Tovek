//! Versioned naming hints, deliberately incapable of expressing effect facts.
//! A syntactic library name can be rebound. These roles never resolve runtime
//! callees, authorize inlining, establish types, or promise that a call is total.

pub const VERSION: &str = "luau-buffer-roles-v1";
pub const COMPILER_COMMIT: &str = "c2ec0d4e5ca50796ba174a7565298f59aa572268";
pub const SOURCE_PATH: &str = "VM/src/lbuflib.cpp";

pub fn buffer_arguments(member: &str) -> &'static [&'static str] {
    match member {
        "create" => &["size"],
        "fromstring" => &["str"],
        "len" | "tostring" => &["buf"],
        "readi8" | "readu8" | "readi16" | "readu16" | "readi32" | "readu32"
        | "readf32" | "readf64" | "readinteger" => &["buf", "offset"],
        "writei8" | "writeu8" | "writei16" | "writeu16" | "writei32" | "writeu32"
        | "writef32" | "writef64" | "writeinteger" => &["buf", "offset", "value"],
        "readstring" => &["buf", "offset", "count"],
        "writestring" => &["buf", "offset", "str", "count"],
        "readbits" => &["buf", "bitOffset", "bitCount"],
        "writebits" => &["buf", "bitOffset", "bitCount", "value"],
        "fill" => &["buf", "offset", "value", "count"],
        "copy" => &["target", "targetOffset", "source", "sourceOffset", "count"],
        _ => &[],
    }
}

/// Version of the curated Roblox role table, not an engine/API release number.
pub const ROBLOX_VERSION: &str = "roblox-event-roles-v1";
pub const ROBLOX_SOURCE: &str = "https://create.roblox.com/docs/reference/engine/classes";

/// Ordered parameter names for a Roblox event's `:Connect` callback. A `None`
/// slot keeps the param's default name. Only conservative, well-known signatures
/// are listed; overloaded/arbitrary ones (`Changed`, `OnClientEvent`, ...) are
/// intentionally absent so we never invent a misleading name.
pub fn roblox_event_parameters(event: &str) -> Option<&'static [Option<&'static str>]> {
    Some(match event {
        "Heartbeat" | "RenderStepped" | "PreSimulation" | "PostSimulation" | "PreRender"
        | "PreAnimation" => &[Some("dt")],
        "Stepped" => &[Some("time"), Some("dt")],
        "InputBegan" | "InputEnded" | "InputChanged" => &[Some("input"), Some("gameProcessed")],
        "ChildAdded" | "ChildRemoved" => &[Some("child")],
        "DescendantAdded" | "DescendantRemoving" => &[Some("descendant")],
        "AncestryChanged" => &[None, Some("parent")],
        "CharacterAdded" | "CharacterRemoving" | "CharacterAppearanceLoaded" => {
            &[Some("character")]
        }
        "PlayerAdded" | "PlayerRemoving" => &[Some("player")],
        "Triggered" => &[Some("player")],
        "PromptTriggered" => &[Some("prompt"), Some("player")],
        "Touched" | "TouchEnded" => &[Some("otherPart")],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_api_members_and_bit_positions() {
        assert_eq!(buffer_arguments("readbits"), ["buf", "bitOffset", "bitCount"]);
        assert_eq!(buffer_arguments("writebits")[3], "value");
        assert_eq!(buffer_arguments("copy")[2], "source");
        assert!(buffer_arguments("readCustom").is_empty());
        assert!(buffer_arguments("Readu32").is_empty());
        assert_eq!(roblox_event_parameters("CharacterAdded"), Some([Some("character")].as_slice()));
        assert_eq!(roblox_event_parameters("AncestryChanged"), Some([None, Some("parent")].as_slice()));
        assert!(roblox_event_parameters("OnClientEvent").is_none());
        assert!(roblox_event_parameters("Changed").is_none());
        assert!(roblox_event_parameters("characterAdded").is_none());
    }
}
