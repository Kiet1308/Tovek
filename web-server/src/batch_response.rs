//! Serialize each completed quantum once, with space reserved for every later
//! item's error envelope. A large source cannot sink good neighbours or leave
//! a partial JSON response, and results never accumulate for the entire batch.
use crate::{BatchResultItem, Error, ParsedItem, MAX_RESPONSE_LEN, MAX_RESPONSE_METADATA,
    MAX_SOURCE_LEN, RESPONSE_BUDGET_ERROR};
use axum::body::Bytes;
use serde::Serialize;
use std::io::{self, Write};

#[derive(Default)]
struct Count(usize);
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.checked_add(bytes.len()).ok_or_else(|| io::Error::other("response size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn fallback(index: usize, id: Option<String>, script_name: Option<String>) -> BatchResultItem {
    BatchResultItem { index, id, script_name, ok: false, decompilation: None,
        error: Some(RESPONSE_BUDGET_ERROR.into()) }
}

fn metadata_sizes(items: &[ParsedItem]) -> Result<Vec<usize>, Error> {
    let mut total = 128usize;
    let mut sizes = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let (id, name) = match item {
            ParsedItem::Ready { id, script_name, .. } | ParsedItem::Failed { id, script_name, .. } => (id, script_name),
        };
        let row = fallback(index, id.clone(), name.clone());
        let mut count = Count::default();
        row.serialize(&mut serde_json::Serializer::new(&mut count)).map_err(|error| Error::Io(io::Error::other(error)))?;
        let bytes = count.0 + usize::from(index != 0);
        total = total.saturating_add(bytes);
        if total > MAX_RESPONSE_METADATA {
            return Err(Error::TooLarge(format!("batch response metadata exceeds {MAX_RESPONSE_METADATA} bytes")));
        }
        sizes.push(bytes);
    }
    Ok(sizes)
}

pub(super) fn validate_metadata(items: &[ParsedItem]) -> Result<(), Error> { metadata_sizes(items).map(|_| ()) }

struct Buffer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let required = self.bytes.len().checked_add(bytes.len())
            .filter(|length| *length <= self.limit).ok_or_else(|| io::Error::other("response byte budget"))?;
        if required > self.bytes.capacity() {
            let capacity = self.bytes.capacity().saturating_mul(2).max(required).min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

pub(super) struct Writer {
    buffer: Buffer,
    maximum: usize,
    reserve: usize,
    sizes: Vec<usize>,
    emitted: usize,
    ok_count: usize,
}

impl Writer {
    pub fn new(items: &[ParsedItem]) -> Result<Self, Error> {
        Self::with_limit(metadata_sizes(items)?, MAX_RESPONSE_LEN)
    }

    fn with_limit(sizes: Vec<usize>, maximum: usize) -> Result<Self, Error> {
        let reserve = sizes.iter().sum::<usize>().saturating_add(128);
        if reserve > maximum { return Err(Error::TooLarge("batch response metadata exceeds response budget".into())); }
        Ok(Self { buffer: Buffer { bytes: b"{\"results\":[".to_vec(), limit: maximum }, maximum,
            reserve, sizes, emitted: 0, ok_count: 0 })
    }

    pub fn append(&mut self, rows: Vec<BatchResultItem>) -> Result<(), Error> {
        for row in rows {
            let size = *self.sizes.get(self.emitted).ok_or_else(|| Error::Io(io::Error::other("too many batch results")))?;
            self.reserve -= size;
            self.buffer.limit = self.maximum - self.reserve;
            if self.emitted != 0 { self.buffer.write_all(b",")?; }
            let mark = self.buffer.bytes.len();
            let oversized = row.decompilation.as_ref().is_some_and(|source| source.len() > MAX_SOURCE_LEN);
            if !oversized && serde_json::to_writer(&mut self.buffer, &row).is_ok() {
                self.ok_count += usize::from(row.ok);
            } else {
                self.buffer.bytes.truncate(mark);
                let row = fallback(row.index, row.id, row.script_name);
                serde_json::to_writer(&mut self.buffer, &row).map_err(|error| Error::Io(io::Error::other(error)))?;
            }
            self.emitted += 1;
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Bytes, Error> {
        if self.emitted != self.sizes.len() { return Err(Error::Io(io::Error::other("missing batch results"))); }
        self.buffer.limit = self.maximum;
        write!(&mut self.buffer, "],\"count\":{},\"ok_count\":{}}}", self.emitted, self.ok_count)?;
        Ok(Bytes::from(self.buffer.bytes.into_boxed_slice()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_budget_preserves_order_and_reserves_later_errors_after_json_escaping() {
        let items = (0..3).map(|index| ParsedItem::Failed {
            id: Some(index.to_string()), script_name: Some("Widget\n\"".into()), error: "invalid".into(),
        }).collect::<Vec<_>>();
        let sizes = metadata_sizes(&items).unwrap();
        let limit = sizes.iter().sum::<usize>() + 128 + 16;
        let mut writer = Writer::with_limit(sizes, limit).unwrap();
        writer.append((0..3).map(|index| BatchResultItem {
            index, id: Some(index.to_string()), script_name: Some("Widget\n\"".into()), ok: true,
            decompilation: Some(if index == 1 { Bytes::from("\n".repeat(1000)) } else { Bytes::from_static(b"return 7") }), error: None,
        }).collect()).unwrap();
        let bytes = writer.finish().unwrap();
        assert!(bytes.len() <= limit);
        let output: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(output["count"], 3);
        assert_eq!(output["ok_count"], 2);
        assert_eq!(output["results"][0]["decompilation"], "return 7");
        assert_eq!(output["results"][1]["error"], RESPONSE_BUDGET_ERROR);
        assert_eq!(output["results"][2]["decompilation"], "return 7");
    }

    #[test]
    fn bounded_buffer_does_not_retain_an_overgrown_capacity() {
        let mut buffer = Buffer { bytes: Vec::new(), limit: 100 };
        for _ in 0..100 { buffer.write_all(b"x").unwrap(); }
        assert!(buffer.bytes.capacity() <= 100);
        assert!(buffer.write_all(b"x").is_err());
        assert_eq!(buffer.bytes.len(), 100);
    }
}
