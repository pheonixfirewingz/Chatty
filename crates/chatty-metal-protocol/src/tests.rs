use super::*;
use std::io::Cursor;

fn round_trip<T>(message_type: MessageType, value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let mut wire = Vec::new();
    write_message(&mut wire, message_type, 7, value).unwrap();
    let frame = read_frame(&mut Cursor::new(wire)).unwrap();
    assert_eq!(frame.message_type, message_type);
    assert_eq!(frame.request_id, 7);
    let decoded: T = decode(&frame.payload).unwrap();
    assert_eq!(&decoded, value);
}

#[test]
fn every_message_round_trips() {
    round_trip(MessageType::Hello, &Hello::new("broker"));
    round_trip(
        MessageType::HelloAck,
        &HelloAck {
            version: PROTOCOL_VERSION,
            model: "DeepSeek-V4.1-Flash-1017-FP8".into(),
            context_tokens: 65_536,
            max_output_tokens: MAX_OUTPUT_TOKENS,
            state: ServerState::Ready,
            detail: "ready".into(),
        },
    );
    round_trip(
        MessageType::Status,
        &Status {
            state: ServerState::Ready,
            detail: "ready".into(),
            model: "model".into(),
            context_tokens: 65_536,
            uptime_ms: 42,
            tokens_generated: 9,
            requests_served: 2,
            last_decode_tokens_per_second: 6.71,
            queue_depth: 0,
        },
    );
    round_trip(
        MessageType::Generate,
        &Generate {
            prompt: vec![
                Piece::Marker(Marker::BeginOfSentence),
                Piece::Marker(Marker::User),
                Piece::Text("hello".into()),
                Piece::Marker(Marker::Assistant),
                Piece::Marker(Marker::ThoughtEnd),
            ],
            sampling: Sampling::default(),
            max_tokens: 32,
        },
    );
    round_trip(
        MessageType::Chunk,
        &Chunk {
            sequence: 3,
            text: "hello".into(),
            reasoning: false,
        },
    );
    round_trip(
        MessageType::Chunk,
        &Chunk {
            sequence: 4,
            text: String::new(),
            reasoning: true,
        },
    );
    round_trip(
        MessageType::Done,
        &Done {
            finish: FinishReason::EndOfSentence,
            prompt_tokens: 12,
            completion_tokens: 8,
            decode_ms: 1190,
        },
    );
    round_trip(
        MessageType::Cancel,
        &Cancel {
            reason: "client closed".into(),
        },
    );
    round_trip(
        MessageType::Error,
        &WireError {
            code: ErrorCode::ContextExceeded,
            message: "prompt is too long".into(),
        },
    );
}

#[test]
fn header_layout_is_fixed() {
    let mut wire = Vec::new();
    write_message(
        &mut wire,
        MessageType::Hello,
        0x0102_0304_0506_0708,
        &Hello::new("b"),
    )
    .unwrap();
    assert_eq!(
        &wire[..4],
        &((wire.len() - HEADER_LEN) as u32).to_be_bytes()[..]
    );
    assert_eq!(wire[4], 0);
    assert_eq!(wire[5], MessageType::Hello as u8);
    assert_eq!(&wire[6..14], &0x0102_0304_0506_0708u64.to_be_bytes()[..]);
}

#[test]
fn large_payloads_are_compressed_and_restored() {
    let text = "roleplay ".repeat(4096);
    let mut wire = Vec::new();
    write_message(
        &mut wire,
        MessageType::Chunk,
        1,
        &Chunk {
            sequence: 0,
            text: text.clone(),
            reasoning: false,
        },
    )
    .unwrap();
    assert_eq!(wire[4], FLAG_ZSTD);
    let frame = read_frame(&mut Cursor::new(wire)).unwrap();
    assert!(frame.compressed);
    let chunk: Chunk = decode(&frame.payload).unwrap();
    assert_eq!(chunk.text, text);
}

#[test]
fn rejects_oversized_declared_length_before_allocating() {
    let mut wire = vec![0u8; HEADER_LEN];
    wire[..4].copy_from_slice(&((MAX_PAYLOAD + 1) as u32).to_be_bytes());
    wire[5] = MessageType::Status as u8;
    let error = read_frame(&mut Cursor::new(wire)).unwrap_err();
    assert!(error.to_string().contains("too large"));
}

#[test]
fn rejects_unknown_type_and_flags() {
    let mut wire = vec![0u8; HEADER_LEN];
    wire[5] = 99;
    assert!(read_frame(&mut Cursor::new(wire.clone())).is_err());
    wire[5] = MessageType::Status as u8;
    wire[4] = 0x80;
    assert!(read_frame(&mut Cursor::new(wire)).is_err());
}

#[test]
fn rejects_trailing_payload_bytes() {
    let mut bytes = encode(&Hello::new("broker")).unwrap();
    bytes.push(0);
    assert!(decode::<Hello>(&bytes).is_err());
}

#[test]
fn validates_prompt_and_sampling_bounds() {
    let mut generate = Generate {
        prompt: Vec::new(),
        sampling: Sampling::default(),
        max_tokens: 1,
    };
    assert!(generate.validate().is_err());
    generate.prompt.push(Piece::Text("hi".into()));
    assert!(generate.validate().is_ok());
    generate.max_tokens = 0;
    assert!(generate.validate().is_err());
    generate.max_tokens = MAX_OUTPUT_TOKENS + 1;
    assert!(generate.validate().is_err());
    generate.max_tokens = 1;
    generate.sampling.temperature = f32::NAN;
    assert!(generate.validate().is_err());
    generate.sampling.temperature = 0.0;
    generate.sampling.top_p = 1.5;
    assert!(generate.validate().is_err());
}

#[test]
fn rejects_unknown_protocol_version() {
    let hello = Hello {
        version: PROTOCOL_VERSION + 1,
        client: "broker".into(),
    };
    assert!(hello.validate().is_err());
}
