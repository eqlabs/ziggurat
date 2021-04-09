use crate::protocol::payload::{read_n_bytes, Hash, VarInt};

use std::io::{self, Cursor, Write};

#[derive(Debug)]
pub struct Inv {
    count: VarInt,
    inventory: Vec<InvHash>,
}

impl Inv {
    pub fn encode(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        self.count.encode(buffer)?;

        for hash in &self.inventory {
            hash.encode(buffer)?;
        }

        Ok(())
    }

    pub fn decode(bytes: &mut Cursor<&[u8]>) -> io::Result<Self> {
        let count = VarInt::decode(bytes)?;
        let mut inventory = Vec::with_capacity(count.0);

        for _ in 0..count.0 {
            let hash = InvHash::decode(bytes)?;
            inventory.push(hash);
        }

        Ok(Self { count, inventory })
    }
}

#[derive(Debug)]
struct InvHash {
    kind: ObjectKind,
    hash: Hash,
}

impl InvHash {
    fn encode(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        self.kind.encode(buffer)?;
        self.hash.encode(buffer)?;

        Ok(())
    }

    fn decode(bytes: &mut Cursor<&[u8]>) -> io::Result<Self> {
        let kind = ObjectKind::decode(bytes)?;
        let hash = Hash::decode(bytes)?;

        Ok(Self { kind, hash })
    }
}

#[derive(Debug)]
enum ObjectKind {
    Error,
    Tx,
    Block,
    FilteredBlock,
}

impl ObjectKind {
    fn encode(&self, buffer: &mut Vec<u8>) -> io::Result<()> {
        let value: u32 = match self {
            Self::Error => 0,
            Self::Tx => 1,
            Self::Block => 2,
            Self::FilteredBlock => 3,
        };

        buffer.write_all(&value.to_le_bytes())?;

        Ok(())
    }

    fn decode(bytes: &mut Cursor<&[u8]>) -> io::Result<Self> {
        let value = u32::from_le_bytes(read_n_bytes(bytes)?);

        let kind = match value {
            0 => Self::Error,
            1 => Self::Tx,
            2 => Self::Block,
            3 => Self::FilteredBlock,
            _ => unreachable!(),
        };

        Ok(kind)
    }
}
