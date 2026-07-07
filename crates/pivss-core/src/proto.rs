//! Hand-written prost messages mirroring `vss-server`'s `vss.proto`
//! (field numbers must stay in sync with
//! <https://github.com/lightningdevkit/vss-server/blob/main/api/src/proto/vss.proto>).
//! Written by hand so builds don't require `protoc`.

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetObjectRequest {
    #[prost(string, tag = "1")]
    pub store_id: String,
    #[prost(string, tag = "2")]
    pub key: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct GetObjectResponse {
    #[prost(message, optional, tag = "2")]
    pub value: Option<KeyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PutObjectRequest {
    #[prost(string, tag = "1")]
    pub store_id: String,
    #[prost(int64, optional, tag = "2")]
    pub global_version: Option<i64>,
    #[prost(message, repeated, tag = "3")]
    pub transaction_items: Vec<KeyValue>,
    #[prost(message, repeated, tag = "4")]
    pub delete_items: Vec<KeyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PutObjectResponse {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DeleteObjectRequest {
    #[prost(string, tag = "1")]
    pub store_id: String,
    #[prost(message, optional, tag = "2")]
    pub key_value: Option<KeyValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DeleteObjectResponse {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ListKeyVersionsRequest {
    #[prost(string, tag = "1")]
    pub store_id: String,
    #[prost(string, optional, tag = "2")]
    pub key_prefix: Option<String>,
    #[prost(int32, optional, tag = "3")]
    pub page_size: Option<i32>,
    #[prost(string, optional, tag = "4")]
    pub page_token: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ListKeyVersionsResponse {
    #[prost(message, repeated, tag = "1")]
    pub key_versions: Vec<KeyValue>,
    #[prost(string, optional, tag = "2")]
    pub next_page_token: Option<String>,
    #[prost(int64, optional, tag = "3")]
    pub global_version: Option<i64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ErrorResponse {
    #[prost(enumeration = "ErrorCode", tag = "1")]
    pub error_code: i32,
    #[prost(string, tag = "2")]
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ErrorCode {
    Unknown = 0,
    ConflictException = 1,
    InvalidRequestException = 2,
    InternalServerException = 3,
    NoSuchKeyException = 4,
    AuthException = 5,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(int64, tag = "2")]
    pub version: i64,
    #[prost(bytes = "vec", tag = "3")]
    pub value: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn roundtrip_put_object() {
        let req = PutObjectRequest {
            store_id: "store".into(),
            global_version: Some(3),
            transaction_items: vec![KeyValue {
                key: "backup/abc/v1".into(),
                version: 0,
                value: b"hello".to_vec(),
            }],
            delete_items: vec![],
        };
        let bytes = req.encode_to_vec();
        let back = PutObjectRequest::decode(bytes.as_slice()).unwrap();
        assert_eq!(req, back);
    }
}
