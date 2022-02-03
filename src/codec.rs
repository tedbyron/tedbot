//! Bincode [de]serialization.

use bincode::de::BorrowDecode;
use bincode::error::{DecodeError, EncodeError};
use bincode::Encode;

type Config = bincode::config::Configuration<bincode::config::BigEndian, bincode::config::Fixint>;

const CONFIG: Config = bincode::config::standard()
    .with_big_endian()
    .with_fixed_int_encoding();

#[tracing::instrument(skip_all)]
pub fn encode<E>(val: E) -> Result<Vec<u8>, EncodeError>
where
    E: Encode,
{
    bincode::encode_to_vec(val, CONFIG)
}

#[tracing::instrument(skip_all)]
pub fn decode<'a, D>(src: &'a [u8]) -> Result<D, DecodeError>
where
    D: BorrowDecode<'a>,
{
    let (res, _) = bincode::decode_from_slice::<D, Config>(src, CONFIG)?;
    Ok(res)
}
