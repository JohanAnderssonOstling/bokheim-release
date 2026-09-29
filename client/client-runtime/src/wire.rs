//! The encoded reply shape shared by every host transport.
//!
//! Library work and application work both answer requests with one opaque
//! payload, so the codec belongs to neither side. Browser hosts send these
//! bytes across a shared worker boundary; native hosts pass them between the
//! backend thread and the UI unchanged.

// Version 2 carries structured errors instead of plain error strings.
const WIRE_HEADER: &[u8; 5] = b"BKHW\x02";

pub fn encode_worker_message<T: serde::Serialize + ?Sized>(message: &T) -> Result<Vec<u8>, crate::BackendError> {
    let mut bytes = WIRE_HEADER.to_vec();
    message.serialize(&mut rmp_serde::Serializer::new(&mut bytes).with_struct_map()).map_err(crate::BackendError::operation)?;
    Ok(bytes)
}

pub fn decode_worker_message<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, crate::BackendError> {
    let payload = bytes.strip_prefix(WIRE_HEADER).ok_or_else(|| crate::BackendError::message("unsupported backend worker protocol"))?;
    rmp_serde::from_slice(payload).map_err(crate::BackendError::operation)
}

/// One encoded reply. Commands that carry bytes serialize them as `serde_bytes`
/// values inside this message rather than as a second payload shape.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct WorkerPayload(#[serde(with = "serde_bytes")] pub Vec<u8>);

pub fn encode_reply<T: serde::Serialize>(value: T) -> Result<WorkerPayload, crate::BackendError> {
    encode_worker_message(&value).map(WorkerPayload)
}

pub fn decode_worker_payload<T: serde::de::DeserializeOwned>(payload: WorkerPayload) -> Result<T, crate::BackendError> {
    decode_worker_message(&payload.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_carrying_bytes_decode_through_the_one_payload_shape() {
        let metadata = encode_reply(7usize).unwrap();
        assert_eq!(decode_worker_payload::<usize>(metadata).unwrap(), 7);
        // Thumbnails and file ranges travel as ordinary `serde_bytes` replies.
        let attachment = encode_reply(Some(serde_bytes::ByteBuf::from(vec![1, 2, 3]))).unwrap();
        assert_eq!(decode_worker_payload::<Option<serde_bytes::ByteBuf>>(attachment).unwrap().map(serde_bytes::ByteBuf::into_vec), Some(vec![1, 2, 3]));
        assert_eq!(decode_worker_payload::<Option<serde_bytes::ByteBuf>>(encode_reply(None::<serde_bytes::ByteBuf>).unwrap()).unwrap(), None);
        assert!(decode_worker_payload::<usize>(WorkerPayload(vec![0])).is_err());
    }
}
