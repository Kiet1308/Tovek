use nom::{multi::count, number::complete::le_u32, IResult};

#[derive(Debug)]
pub struct Position {
    pub instruction: usize,
    pub source: u32,
}

impl Position {
    pub fn parse(input: &[u8]) -> IResult<&[u8], Vec<Self>> {
        let (input, positions_length) = le_u32(input)?;
        let (input, source_positions) = count(le_u32, positions_length as usize)(input)?;

        Ok((
            input,
            source_positions
                .iter()
                .enumerate()
                .map(|(instruction, &source)| Self {
                    instruction,
                    source,
                })
                .collect(),
        ))
    }
}
