use anyhow::Result;
use bytes::{BufMut, BytesMut};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Write};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Register {
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct PowRequest {
    pub id: String,
    pub challenge: [u8; 32],
    pub start_nonce: u32,
    pub end_nonce: u32,
    pub difficulty: u64,
    pub pow_difficulty: [u8; 32],
    pub node_id: [u8; 32],
    pub pow_flags: String,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct PowResponse {
    pub id: String,
    pub ciphers_pows: HashMap<u32, u64>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub enum PowMessage {
    Register(Register),
    Request(PowRequest),
    Response(PowResponse),
}

#[derive(Default)]
pub struct PoWMessageCodec {
    cursor: usize,
}

impl Encoder<PowMessage> for PoWMessageCodec {
    type Error = anyhow::Error;
    fn encode(&mut self, message: PowMessage, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json_string = serde_json::to_string(&message).unwrap();
        dst.writer().write_all(json_string.as_bytes())?;
        dst.writer().write_all("\n".as_bytes())?;
        Ok(())
    }
}

impl Decoder for PoWMessageCodec {
    type Error = anyhow::Error;
    type Item = PowMessage;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let mut i = self.cursor;
        while i < src.len() {
            if src[i] == 10u8 {
                self.cursor = 0;
                let mut data = src.split_to(i + 1);
                unsafe {
                    data.set_len(i);
                }
                src.reserve(100);
                let message = serde_json::from_slice(&data[..])?;
                return Ok(Some(message));
            }
            i += 1;
        }
        self.cursor = i;
        Ok(None)
    }
}
