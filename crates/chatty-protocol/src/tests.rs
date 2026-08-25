use crate::*;

#[test]
fn utc_error_timestamp_is_stable_and_readable() {
    let timestamp = time::macros::datetime!(2026-08-24 14:32:07 UTC);
    assert_eq!(format_utc_timestamp(timestamp), "2026-08-24 14:32:07 UTC");
}

#[tokio::test]
async fn compressed_round_trip() {
    let (mut a, mut b) = tokio::io::duplex(32 * 1024);
    let value = StreamChunk {
        message_id: "m".into(),
        sequence: 2,
        text: "roleplay ".repeat(100),
    };
    let send = tokio::spawn(async move {
        write_message(&mut a, MessageType::StreamChunk, 7, &value)
            .await
            .unwrap()
    });
    let frame = read_frame(&mut b).await.unwrap();
    send.await.unwrap();
    assert!(frame.compressed);
    assert_eq!(frame.request_id, 7);
    assert_eq!(decode::<StreamChunk>(&frame.payload).unwrap().sequence, 2);
}

#[tokio::test]
async fn small_stream_chunks_are_still_compressed() {
    let (mut a, mut b) = tokio::io::duplex(1024);
    let value = StreamChunk {
        message_id: "m".into(),
        sequence: 1,
        text: "short batch".into(),
    };
    tokio::spawn(async move {
        write_message(&mut a, MessageType::StreamChunk, 1, &value)
            .await
            .unwrap()
    });
    assert!(read_frame(&mut b).await.unwrap().compressed);
}

#[tokio::test]
async fn fragmented_high_latency_frame_recovers() {
    use tokio::time::{Duration, sleep};
    let (mut a, mut b) = tokio::io::duplex(64);
    let task = tokio::spawn(async move {
        let raw = encode(&Response::Pong).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(raw.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&[0, MessageType::Response as u8]);
        bytes.extend_from_slice(&99u64.to_be_bytes());
        bytes.extend(raw);
        for part in bytes.chunks(2) {
            a.write_all(part).await.unwrap();
            sleep(Duration::from_millis(1)).await;
        }
    });
    let frame = read_frame(&mut b).await.unwrap();
    task.await.unwrap();
    assert_eq!(frame.request_id, 99);
    assert!(matches!(decode(&frame.payload).unwrap(), Response::Pong));
}

#[tokio::test]
async fn rejects_oversized_frame_before_allocation() {
    let (mut a, mut b) = tokio::io::duplex(32);
    tokio::spawn(async move {
        a.write_all(&((MAX_PAYLOAD as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        a.write_all(&[0, MessageType::Request as u8]).await.unwrap();
        a.write_all(&0u64.to_be_bytes()).await.unwrap();
    });
    assert!(matches!(
        read_frame(&mut b).await,
        Err(ProtocolError::Invalid("payload too large"))
    ));
}

#[tokio::test]
async fn repetitive_rp_stream_has_strong_compression_ratio() {
    let text = "The lantern flickered against the old stone wall. ".repeat(400);
    let raw_len = encode(&StreamChunk {
        message_id: "message".into(),
        sequence: 1,
        text: text.clone(),
    })
    .unwrap()
    .len();
    let (mut writer, mut reader) = tokio::io::duplex(raw_len * 2);
    tokio::spawn(async move {
        write_message(
            &mut writer,
            MessageType::StreamChunk,
            1,
            &StreamChunk {
                message_id: "message".into(),
                sequence: 1,
                text,
            },
        )
        .await
        .unwrap();
    });
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await.unwrap();
    let compressed_len = u32::from_be_bytes(header[0..4].try_into().unwrap()) as usize;
    assert_eq!(header[4], FLAG_ZSTD);
    assert!(
        compressed_len * 10 < raw_len,
        "{compressed_len} vs {raw_len}"
    );
}

#[tokio::test]
async fn bounded_transport_survives_streaming_stress() {
    let (mut writer, mut reader) = tokio::io::duplex(128);
    let send = tokio::spawn(async move {
        for sequence in 0..1_000 {
            write_message(
                &mut writer,
                MessageType::StreamChunk,
                42,
                &StreamChunk {
                    message_id: "stress".into(),
                    sequence,
                    text: "sixteen token batch delivered through a deliberately tiny bounded transport buffer".into(),
                },
            )
            .await
            .unwrap();
        }
    });
    for expected in 0..1_000 {
        let frame = read_frame(&mut reader).await.unwrap();
        let chunk: StreamChunk = decode(&frame.payload).unwrap();
        assert_eq!(chunk.sequence, expected);
    }
    send.await.unwrap();
}

#[tokio::test]
async fn rejects_zstd_expansion_beyond_limit() {
    let compressed = zstd::stream::encode_all(Cursor::new(vec![b'x'; MAX_PAYLOAD + 1]), 1).unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(compressed.len() + HEADER_LEN);
    tokio::spawn(async move {
        writer
            .write_all(&(compressed.len() as u32).to_be_bytes())
            .await
            .unwrap();
        writer
            .write_all(&[FLAG_ZSTD, MessageType::Request as u8])
            .await
            .unwrap();
        writer.write_all(&1u64.to_be_bytes()).await.unwrap();
        writer.write_all(&compressed).await.unwrap();
    });
    assert!(matches!(
        read_frame(&mut reader).await,
        Err(ProtocolError::Invalid("decompressed payload too large"))
    ));
}
