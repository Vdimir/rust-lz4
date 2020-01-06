use std::error::Error;
use std::fmt;
use std::io::{self, Read, Write};
use std::result::Result;
use std::u32;

use crate::buffer::{read_buf::Lz4ReadBuf, write_buf::Lz4WriteBuf};

#[derive(Debug)]
pub enum DecodeError {
    /// Wrong LZ4 magic number
    WrongMagic,
    /// Wrong LZ4 version
    WrongVersion,
    /// Error occured while data reading
    ReadIoError(io::Error),
    /// Error occured while data writing
    WriteIoError(io::Error),
    /// Usnuppoted LZ4 feature
    UnsuppotedFeature(String),
    /// Invalud block size (possibly, larger than internal buffer)
    InvalidBlockSize(usize),
    /// Data stream is corrupted
    CorruptedData,
    /// All data decopressed but reader contains unrecognized data at end
    UnknownDataAtEnd,
}

use DecodeError::*;

type DecodeResult<T> = Result<T, DecodeError>;

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", "Decode Error")
    }
}

impl Error for DecodeError {}

/// Decoder for LZ4 compressed data
#[derive(Debug)]
pub struct LzDecoder {
    input_buffer: Lz4ReadBuf,
}

#[derive(Debug)]
struct FrameHeaderInfo {
    block_indep_flag: bool,
    block_checksum_flag: bool,
    content_size_flag: bool,
    content_checksum_flag: bool,
    dict_id_flag: bool,
    block_max_size: u8,
    header_size: usize,
}

#[inline]
fn is_bit_set(n: u8, i: u8) -> bool {
    return n & (1 << i) != 0;
}

impl LzDecoder {
    const INPUT_BUFFER_SIZE: usize = 1 << 22;
    const WINDOW_SIZE: usize = 1 << 16;
    const FRAME_MAGIC: u32 = 0x184D2204;
    const U32_LEN: usize = std::mem::size_of::<u32>();
    const U16_LEN: usize = std::mem::size_of::<u16>();

    const BASE_MATCH_LEN: usize = 4;

    /// Create new decoder
    pub fn new() -> Self {
        LzDecoder {
            input_buffer: Lz4ReadBuf::with_capacity(Self::INPUT_BUFFER_SIZE),
        }
    }

    /// Read compressed data from `input` and write decopressed  to `output`
    pub fn decode<R, W>(&mut self, input: &mut R, output: &mut W) -> DecodeResult<()>
    where
        R: Read,
        W: Write,
    {
        let mut output = Lz4WriteBuf::with_capacity(output, Self::WINDOW_SIZE);

        let frame_header = self.parse_header(input)?;

        let FrameHeaderInfo { dict_id_flag, .. } = frame_header;

        if dict_id_flag {
            return Err(DecodeError::UnsuppotedFeature("DictID".to_string()));
        }

        loop {
            self.input_buffer.compact();

            let bs_data = self.read_u32(input)?;

            let mask = 1 << 31;
            let is_raw = bs_data & mask != 0;

            let block_size = (bs_data & (mask - 1)) as usize;

            if bs_data == 0 {
                break;
            }

            if block_size >= self.input_buffer.capacity() {
                return Err(DecodeError::InvalidBlockSize(block_size));
            }

            self.input_buffer
                .extend_read(input, block_size)
                .map_err(ReadIoError)?;

            if is_raw {
                let n = self.input_buffer.len();
                output
                    .write_all(&self.input_buffer[..n])
                    .map_err(WriteIoError)?;
                self.input_buffer.consume(n);
                continue;
            }

            loop {
                let block_completed = self.process_sequence(&mut output)?;
                if block_completed {
                    break;
                }
            }
        }

        if frame_header.content_checksum_flag {
            // TODO do not ignore content checksum
            let _ = self.read_u32(input)?;
        }

        let mut dummy_buf = [0u8; 4];
        let n = input.read(&mut dummy_buf).map_err(ReadIoError)?;
        if n != 0 {
            return Err(UnknownDataAtEnd);
        }
        Ok(())
    }

    fn process_sequence<W: Write>(&mut self, output: &mut Lz4WriteBuf<W>) -> DecodeResult<bool> {
        let tok = self
            .input_buffer
            .pop_byte()
            .ok_or(DecodeError::CorruptedData)?;

        let lit_len = self.get_var_int_from_buf((tok & 0xF0) >> 4)?;
        if lit_len > self.input_buffer.len() {
            return Err(DecodeError::CorruptedData);
        }

        output
            .write_all(&self.input_buffer[..lit_len])
            .map_err(WriteIoError)?;
        self.input_buffer.consume(lit_len);

        if self.input_buffer.len() == 0 {
            return Ok(true);
        }

        if self.input_buffer.len() < Self::U16_LEN {
            return Err(DecodeError::CorruptedData);
        }
        let offset = u16::from_le_bytes([self.input_buffer[0], self.input_buffer[1]]) as usize;
        if offset == 0 {
            return Err(DecodeError::CorruptedData);
        }

        self.input_buffer.consume(Self::U16_LEN);
        let match_len = self.get_var_int_from_buf(tok & 0x0F)? + Self::BASE_MATCH_LEN;

        output
            .copy_from_offset(offset, match_len)
            .map_err(WriteIoError)?;

        Ok(false)
    }

    fn read_u32<R: Read>(&self, input: &mut R) -> DecodeResult<u32> {
        let mut int_data = [0u8; 4];
        input.read_exact(&mut int_data).map_err(ReadIoError)?;
        Ok(u32::from_le_bytes(int_data))
    }

    #[inline]
    fn get_var_int_from_buf(&mut self, base: u8) -> DecodeResult<usize> {
        let mut n = base as usize;

        if base != 15 {
            return Ok(n);
        }
        loop {
            let b = self
                .input_buffer
                .pop_byte()
                .ok_or(DecodeError::CorruptedData)?;
            n += b as usize;
            if b != 255 {
                break;
            }
        }
        return Ok(n);
    }

    fn parse_header<R: Read>(&mut self, input: &mut R) -> DecodeResult<FrameHeaderInfo> {
        let min_header_size = Self::U32_LEN + 3;

        self.input_buffer
            .extend_read(input, min_header_size)
            .map_err(ReadIoError)?;

        let frame_magic = self.input_buffer.get_u32(0);

        if frame_magic != Self::FRAME_MAGIC {
            return Err(DecodeError::WrongMagic);
        }

        /*
        |  BitNb  |  7-6  |   5   |    4     |  3   |    2     |    1     |   0  |
        | ------- |-------|-------|----------|------|----------|----------|------|
        |FieldName|Version|B.Indep|B.Checksum|C.Size|C.Checksum|*Reserved*|DictID|
        */
        let flg_byte = self.input_buffer[Self::U32_LEN];
        if flg_byte >> 6 != 0b1 {
            return Err(DecodeError::WrongVersion);
        }

        let mut header_size = min_header_size;

        let content_size_flag = is_bit_set(flg_byte, 3);
        if content_size_flag {
            let content_size_num_size = 4;
            // TODO: don't skip content size
            self.input_buffer
                .extend_read(input, content_size_num_size)
                .map_err(ReadIoError)?;
            header_size += content_size_num_size;
        }

        let dict_id_flag = is_bit_set(flg_byte, 0);
        if dict_id_flag {
            let dict_size = 4;
            // TODO: don't skip dict id
            self.input_buffer
                .extend_read(input, dict_size)
                .map_err(ReadIoError)?;
            header_size += dict_size;
        }

        /*
        |  BitNb  |     7    |     6-5-4     |  3-2-1-0 |
        | ------- | -------- | ------------- | -------- |
        |FieldName|*Reserved*| Block MaxSize |*Reserved*|
        */
        let bd_byte = self.input_buffer[Self::U32_LEN + 1];

        // TODO: check header checksum
        let _ = self.input_buffer[header_size];

        self.input_buffer.consume(header_size);
        self.input_buffer.compact();

        let header_info = FrameHeaderInfo {
            block_indep_flag: is_bit_set(flg_byte, 5),
            block_checksum_flag: is_bit_set(flg_byte, 4),
            content_size_flag: content_size_flag,
            content_checksum_flag: is_bit_set(flg_byte, 2),
            dict_id_flag: dict_id_flag,
            block_max_size: (bd_byte & 0b01110000) >> 4,
            header_size: header_size,
        };

        return Ok(header_info);
    }
}
