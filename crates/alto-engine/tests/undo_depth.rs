use alto_doc::{DocumentMeta, Op};
use alto_engine::Engine;

#[test]
fn undo_limit_applies_to_existing_and_new_documents() {
    let mut engine = Engine::new();
    assert_eq!(engine.max_undo(), 100);
    assert!(engine.set_max_undo(0).is_err());
    assert!(engine.set_max_undo(1001).is_err());
    let first = engine
        .create_document(DocumentMeta::srgb_u8(10, 10))
        .unwrap();
    engine.set_max_undo(3).unwrap();
    let second = engine
        .create_document(DocumentMeta::srgb_u8(10, 10))
        .unwrap();
    for doc in [first, second] {
        for width in 11..=15 {
            let revision = engine.document(doc).unwrap().revision;
            engine
                .transact(
                    doc,
                    "resize",
                    vec![Op::SetMeta {
                        meta: DocumentMeta::srgb_u8(width, 10),
                    }],
                    revision,
                )
                .unwrap();
        }
        assert_eq!(engine.history_state(doc).unwrap().0, 3);
    }
    engine.set_max_undo(1).unwrap();
    for doc in [first, second] {
        assert_eq!(engine.history_state(doc).unwrap().0, 1);
        assert!(engine.undo(doc).unwrap().is_some());
        assert!(engine.undo(doc).unwrap().is_none());
    }
}

#[test]
fn rollback_preserves_more_than_default_history_and_respects_limit() {
    let mut engine = Engine::new();
    engine.set_max_undo(150).unwrap();
    let doc = engine
        .create_document(DocumentMeta::srgb_u8(10, 10))
        .unwrap();
    for width in 11..=140 {
        let revision = engine.document(doc).unwrap().revision;
        engine
            .transact(
                doc,
                "resize",
                vec![Op::SetMeta {
                    meta: DocumentMeta::srgb_u8(width, 10),
                }],
                revision,
            )
            .unwrap();
    }
    let state = engine.capture_version_state(doc).unwrap();
    engine.rollback_version_state(doc, state);
    assert_eq!(engine.history_state(doc).unwrap().0, 130);
    let state = engine.capture_version_state(doc).unwrap();
    engine.set_max_undo(3).unwrap();
    engine.rollback_version_state(doc, state);
    assert_eq!(engine.history_state(doc).unwrap().0, 3);
}

#[test]
fn recovery_and_version_replay_preserve_history_above_the_default_limit() {
    let mut engine = Engine::new();
    engine.set_max_undo(150).unwrap();
    let doc = engine
        .create_document(DocumentMeta::srgb_u8(10, 10))
        .unwrap();
    for width in 11..=140 {
        let revision = engine.document(doc).unwrap().revision;
        engine
            .transact(
                doc,
                "resize",
                vec![Op::SetMeta {
                    meta: DocumentMeta::srgb_u8(width, 10),
                }],
                revision,
            )
            .unwrap();
    }
    let digest = engine.state_digest(doc).unwrap();
    let checkpoint = engine
        .recovery_checkpoint(doc, &Default::default())
        .unwrap();
    let mut recovered = Engine::new();
    recovered.set_max_undo(150).unwrap();
    let (recovered_doc, warnings) = recovered
        .open_recovery_entries(checkpoint.entries, &Default::default())
        .unwrap();
    assert!(warnings.is_empty());
    assert_eq!(recovered.state_digest(recovered_doc).unwrap(), digest);
    assert_eq!(recovered.history_state(recovered_doc).unwrap(), (130, 0));

    let initial = engine.save_package(doc).unwrap();
    let before = engine.version_state_digest(doc).unwrap();
    engine.commit(doc, "checkpoint", 1, None, false).unwrap();
    let transition = engine.version_record(doc, before).unwrap();
    let mut replayed = Engine::new();
    replayed.set_max_undo(150).unwrap();
    let (replayed_doc, _) = replayed.open_package(&initial).unwrap();
    assert_eq!(replayed.history_state(replayed_doc).unwrap(), (0, 0));
    assert_eq!(
        replayed.replay_record(replayed_doc, &transition).unwrap(),
        alto_engine::Replayed::Applied
    );
    assert_eq!(replayed.state_digest(replayed_doc).unwrap(), digest);
    assert_eq!(replayed.history_state(replayed_doc).unwrap(), (130, 0));
}
