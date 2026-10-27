//! Bulk byte serialization without changing bincode's existing Vec<u8> layout.

use serde::de::{SeqAccess, Visitor};
use serde::{Deserializer, Serializer};

pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bytes(bytes)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;

    impl<'de> Visitor<'de> for Bytes {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a byte buffer or sequence of bytes")
        }

        fn visit_bytes<E: serde::de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            Ok(bytes.to_vec())
        }

        fn visit_byte_buf<E: serde::de::Error>(self, bytes: Vec<u8>) -> Result<Self::Value, E> {
            Ok(bytes)
        }

        // Preserve array-based formats used by character import/export. Do not
        // reserve an allocation from an untrusted sequence's size hint.
        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let mut bytes = Vec::new();
            while let Some(byte) = sequence.next_element()? {
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }

    deserializer.deserialize_bytes(Bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CharacterImage, DeltaOperation, Response, StateDelta, decode, encode};
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct LegacyImage {
        id: String,
        label: String,
        data: Vec<u8>,
    }

    #[test]
    fn image_bytes_preserve_legacy_encoding_at_length_boundaries() {
        for len in [0, 1, 250, 251, 255, 256, 65_535, 65_536, 131_072] {
            let legacy = LegacyImage {
                id: "portrait".into(),
                label: "happy".into(),
                data: (0..len).map(|i| i as u8).collect(),
            };
            let image = CharacterImage {
                id: legacy.id.clone(),
                label: legacy.label.clone(),
                data: legacy.data.clone(),
            };
            let old_bytes = encode(&legacy).unwrap();
            let new_bytes = encode(&image).unwrap();
            assert_eq!(old_bytes, new_bytes);
            assert_eq!(decode::<CharacterImage>(&old_bytes).unwrap(), image);
            assert_eq!(decode::<LegacyImage>(&new_bytes).unwrap().data, legacy.data);
        }
    }

    #[test]
    fn image_response_and_delta_keep_existing_wire_layouts() {
        let data: Vec<u8> = (0..4096).map(|i| i as u8).collect();
        let response = Response::CharacterImage {
            character_id: "character".into(),
            image_id: "portrait".into(),
            data: data.clone(),
        };
        // Protocol 19's CharacterImage discriminant followed by legacy fields.
        let mut legacy_response = vec![6];
        legacy_response.extend(encode(&("character", "portrait", &data)).unwrap());
        assert_eq!(encode(&response).unwrap(), legacy_response);
        assert!(
            matches!(decode::<Response>(&legacy_response).unwrap(), Response::CharacterImage { data: decoded, .. } if decoded == data)
        );

        let delta = StateDelta {
            revision: 7,
            entity_type: "message".into(),
            entity_id: "id".into(),
            operation: DeltaOperation::Update,
            changed_fields: data.clone(),
        };
        let legacy_delta = encode(&(7i64, "message", "id", DeltaOperation::Update, &data)).unwrap();
        assert_eq!(encode(&delta).unwrap(), legacy_delta);
        assert_eq!(
            decode::<StateDelta>(&legacy_delta).unwrap().changed_fields,
            data
        );
    }

    #[derive(Deserialize)]
    struct ByteBuffer(#[serde(with = "super")] Vec<u8>);

    #[test]
    fn array_deserializers_still_work() {
        let input = serde::de::value::SeqDeserializer::<_, serde::de::value::Error>::new(
            [0u8, 127, 255].into_iter(),
        );
        assert_eq!(deserialize(input).unwrap(), [0, 127, 255]);
    }

    #[test]
    fn truncated_or_impossible_byte_lengths_are_rejected() {
        let encoded = encode(&vec![1u8, 2, 3]).unwrap();
        assert_eq!(decode::<ByteBuffer>(&encoded).unwrap().0, [1, 2, 3]);
        assert!(decode::<ByteBuffer>(&encoded[..encoded.len() - 1]).is_err());
        let impossible_length = encode(&u64::MAX).unwrap();
        assert!(decode::<ByteBuffer>(&impossible_length).is_err());
    }
}
