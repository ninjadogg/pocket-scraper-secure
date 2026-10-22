//! Chrome native messaging framing: a 32-bit length in native byte order, then UTF-8
//! JSON. Both directions are capped at 1 MiB (Chrome's limit for host messages).

use pocket_scraper_core::limits::HOST_MESSAGE_MAX;
use std::io::{self, Read, Write};

/// Reads one message. `Ok(None)` means Chrome closed the connection.
pub fn read_message(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > HOST_MESSAGE_MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message larger than 1 MiB"));
    }
    let mut body = vec![0u8; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

pub fn write_message(output: &mut impl Write, body: &[u8]) -> io::Result<()> {
    if body.len() > HOST_MESSAGE_MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message larger than 1 MiB"));
    }
    output.write_all(&(body.len() as u32).to_ne_bytes())?;
    output.write_all(body)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_refuses_oversized_frames() {
        let mut buf = Vec::new();
        write_message(&mut buf, br#"{"type":"hello","version":1}"#).unwrap();
        let mut cursor = io::Cursor::new(buf);
        assert_eq!(read_message(&mut cursor).unwrap().unwrap(), br#"{"type":"hello","version":1}"#);
        assert!(read_message(&mut cursor).unwrap().is_none(), "EOF ends the session");

        let mut big = ((HOST_MESSAGE_MAX + 1) as u32).to_ne_bytes().to_vec();
        big.extend_from_slice(b"{}");
        assert!(read_message(&mut io::Cursor::new(big)).is_err());
        assert!(write_message(&mut Vec::new(), &vec![b' '; HOST_MESSAGE_MAX + 1]).is_err());
        // A truncated body is an error, not a short message.
        let mut short = 10u32.to_ne_bytes().to_vec();
        short.extend_from_slice(b"{}");
        assert!(read_message(&mut io::Cursor::new(short)).is_err());
    }
}
