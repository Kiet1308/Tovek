use nom::{bytes::complete::take, IResult};
use nom_leb128::leb128_usize;

pub mod bytecode;
pub mod chunk;
pub mod constant;
pub mod function;
mod list;

fn parse_string(input: &[u8]) -> IResult<&[u8], &[u8]> {
    let (input, length) = leb128_usize(input)?;
    let (input, bytes) = take(length)(input)?;
    Ok((input, bytes))
}

pub fn deserialize(bytecode: &[u8], encode_key: u8) -> Result<bytecode::Bytecode<'_>, String> {
    match bytecode::Bytecode::parse(bytecode, encode_key) {
        Ok((_, deserialized_bytecode)) => Ok(deserialized_bytecode),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn length(output: &mut Vec<u8>, mut value: usize) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            output.push(byte | if value == 0 { 0 } else { 0x80 });
            if value == 0 { break; }
        }
    }

    #[test]
    fn borrowed_strings_match_owned_parser_for_arbitrary_bytes_and_truncation() {
        // Keep the previous parser as an independent oracle for consumption
        // and nom errors, including malformed lengths and empty payloads.
        fn owned(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
            let (input, length) = leb128_usize(input)?;
            let (input, bytes) = take(length)(input)?;
            Ok((input, bytes.to_owned()))
        }
        for size in [0, 1, 2, 127, 128, 255, 256, 1024] {
            let mut encoded = Vec::new();
            length(&mut encoded, size);
            let prefix = encoded.len();
            encoded.extend((0..size).map(|i| i as u8));
            for end in 0..=encoded.len() {
                let input = &encoded[..end];
                assert_eq!(parse_string(input).map(|(rest, value)| (rest, value.to_vec())), owned(input));
            }
            encoded.extend_from_slice(b"trailing");
            let (rest, value) = parse_string(&encoded).unwrap();
            assert_eq!(rest, b"trailing");
            assert_eq!(value, &encoded[prefix..prefix + size]);
            assert_eq!(value.as_ptr(), encoded[prefix..].as_ptr());
        }
        for input in [vec![0xff; 32], vec![0x80; 32], vec![0xff, 0x7f], vec![0x80, 0]] {
            assert_eq!(parse_string(&input).map(|(rest, value)| (rest, value.to_vec())), owned(&input));
        }
    }

    #[test]
    fn chunk_strings_and_userdata_names_borrow_the_encoded_payload() {
        let strings: [&[u8]; 3] = [b"", b"CFrame", b"\0\xff\x80name"];
        let mut encoded = vec![4, 3, strings.len() as u8];
        let mut offsets = Vec::new();
        for bytes in strings {
            length(&mut encoded, bytes.len());
            offsets.push(encoded.len());
            encoded.extend_from_slice(bytes);
        }
        // Preserve duplicate tag order and skip invalid string indices exactly
        // as before; consumers intentionally select the first matching tag.
        encoded.extend_from_slice(&[1, 2, 1, 3, 2, 0, 3, 9, 0, 0, 0]);
        let bytecode::Bytecode::Chunk(chunk) = deserialize(&encoded, 1).unwrap() else { panic!() };
        assert_eq!(chunk.string_table, strings);
        for (string, offset) in chunk.string_table.iter().zip(offsets) {
            assert_eq!(string.as_ptr(), encoded[offset..].as_ptr());
        }
        assert_eq!(chunk.userdata_type_names, vec![(0, strings[1]), (0, strings[2])]);
        for (tag, name) in &chunk.userdata_type_names {
            assert_eq!(*tag, 0);
            assert!(chunk.string_table.iter().any(|string| std::ptr::eq(*name, *string)));
        }
    }

    #[test]
    fn compiler_error_messages_remain_owned_and_lossy() {
        let message = {
            let input = vec![0, b'e', 0xff, 0];
            let bytecode::Bytecode::Error(message) = deserialize(&input, 1).unwrap() else { panic!() };
            message
        };
        assert_eq!(message, "e\u{fffd}\0");
    }
}

/*#[test]
fn main() -> anyhow::Result<()> {
    let compiler = Compiler::new()
        .set_debug_level(1).set_optimization_level(2);
    let bytecode = compiler.compile("asd = test");
    println!("{:#?}", bytecode);
    let deserialized = deserialize(&bytecode);
    println!("{:#?}", deserialized);
    Ok(())
}*/
