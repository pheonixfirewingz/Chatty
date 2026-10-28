use super::*;

#[test]
fn stopped_and_replaced_requests_cannot_update_playback() {
    let state = Mutex::new(State::default());
    let id = state.lock().unwrap().id;
    state.lock().unwrap().stop();
    assert!(!update_state(&state, id, |state| state.busy = true));
    assert!(!state.lock().unwrap().busy);
    let next_id = state.lock().unwrap().id;
    assert!(update_state(&state, next_id, |state| state.busy = true));
}

#[test]
fn long_replies_start_with_a_short_chunk_without_losing_words() {
    let text = format!("First sentence. {} Done!", "a long reply ".repeat(80));
    let chunks = speech_chunks(&text);
    assert_eq!(chunks[0], "First sentence.");
    assert!(chunks.iter().all(|chunk| chunk.len() <= 240));
    assert_eq!(
        chunks.join(" "),
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    );
}

#[test]
#[ignore = "loads the local neural model; may download model files"]
fn local_model_generates_audible_audio() {
    let mut model = load_model(|_| {}).expect("load local model");
    for voice in [FEMALE_VOICE, MALE_VOICE] {
        let samples = model
            .generate("Hello, this is Chatty speaking locally.", voice)
            .expect("generate local speech");
        assert!(samples.len() > 2_400);
        assert!(samples.iter().all(|sample| sample.is_finite()));
        assert!(samples.iter().any(|sample| sample.abs() > 0.01));
    }
}

#[test]
fn strips_common_markdown_before_speaking() {
    assert_eq!(speech_text("## Hello\n- **world**"), "Hello world");
}

#[test]
fn dialogue_keeps_only_quoted_speech() {
    assert_eq!(
        dialogue(
            "*He nods.* \"Alex the human?\" he repeats. \u{201c}Stay close.\u{201d} *He turns.* \"Come"
        ),
        "Alex the human? Stay close. Come"
    );
    assert_eq!(dialogue("*He nods silently.*"), "");
}

#[test]
fn roleplay_text_keeps_punctuation_for_natural_prosody() {
    assert_eq!(
        speech_text("*He nods.* \u{201c}Wait \u{2014} you\u{2019}re human?\u{201d}\u{2026}"),
        "He nods. \"Wait, you're human?\"..."
    );
}

#[test]
fn female_characters_get_the_female_voice() {
    for gender in ["Female", " mare ", "she/her", "F", "female (bat pony)"] {
        assert!(is_female(gender), "{gender}");
    }
    for gender in ["Male", "stallion", "", "non-binary"] {
        assert!(!is_female(gender), "{gender}");
    }
}

#[test]
fn tts_channel_emits_speaking_and_status_events() {
    let (mut tts, mut rx) = tts_channel();
    assert!(!tts.is_speaking());
    assert_eq!(tts.status(), "Ready. The voice model loads on first use.");
    assert!(rx.try_recv().is_err());

    tts.speak("Test sentence", false).expect("speak");
    assert!(tts.is_speaking());

    // Speak sets busy = true and status = "Preparing local speech…"
    let ev1 = rx.try_recv().expect("event 1");
    assert_eq!(ev1, TtsEvent::Speaking(true));

    let ev2 = rx.try_recv().expect("event 2");
    assert_eq!(ev2, TtsEvent::Status("Preparing local speech…".into()));

    // Stop sets busy = false and status = "Ready to read aloud."
    tts.stop();
    assert!(!tts.is_speaking());

    let ev3 = rx.try_recv().expect("event 3");
    assert_eq!(ev3, TtsEvent::Speaking(false));

    let ev4 = rx.try_recv().expect("event 4");
    assert_eq!(ev4, TtsEvent::Status("Ready to read aloud.".into()));

    // Subsequent stop while already idle should NOT emit redundant events
    tts.stop();
    assert!(rx.try_recv().is_err());
}
