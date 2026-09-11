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
    }
}
