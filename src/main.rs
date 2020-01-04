use std::io::{self, Write};

mod buffer;
mod decoder;

use decoder::{DecodeError, LzDecoder};

pub fn main() -> io::Result<()> {
    let mut dec = LzDecoder::new();

    let res = dec.decode(&mut io::stdin(), &mut io::stdout());
    if let Err(e) = res {
        let msg = format!("{:?}", e);
        io::stderr().write_all(msg.as_ref())?;

        let main_err = match e {
            DecodeError::WriteIoError(e) => e,
            DecodeError::ReadIoError(e) => e,
            _ => io::Error::new(io::ErrorKind::InvalidData, e),
        };
        return Err(main_err);
    }
    Ok(())
}
