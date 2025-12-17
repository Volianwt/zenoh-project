use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;

#[derive(Debug, Clone)]
pub struct StorageEntry {
    pub key: Option<OwnedKeyExpr>,
    pub payload: ZBytes,
    pub encoding: Encoding,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    pub limit: u32,
    pub offset: u32,
}

