use neo_log::Level;

#[test]
fn subcrate_style_macros_reach_the_capture_buffer() {
    neo_log::set_max_level(Level::Trace);
    let records = neo_log::capture_for_test(|| {
        neo_log::info!("llm", "stream done"; "finish_reason" => "stop", "tool_calls" => 1);
        neo_log::info!("tools", "tool finished"; "tool" => "read_file", "elapsed_ms" => 12u64);
        neo_log::warn!("store", "db error"; "category" => "constraint");
        neo_log::error!("wake", "mic lost");
        neo_log::debug!("overlay", "card set"; "id" => 2u8, "has_card" => true);
        neo_log::info!("stt", "transcribed"; "samples" => 1600usize, "chars" => 8usize);
    });

    assert_eq!(records.len(), 6);

    assert_eq!(records[0].level, Level::Info);
    assert_eq!(&*records[0].component, "llm");
    assert_eq!(&*records[0].message, "stream done");
    assert_eq!(records[0].fields.len(), 2);
    assert_eq!(&*records[0].fields[0].key, "finish_reason");
    assert_eq!(&*records[0].fields[0].value, "stop");
    assert_eq!(&*records[0].fields[1].key, "tool_calls");
    assert_eq!(&*records[0].fields[1].value, "1");

    assert_eq!(records[1].level, Level::Info);
    assert_eq!(&*records[1].component, "tools");
    assert_eq!(&*records[1].message, "tool finished");
    assert_eq!(records[1].fields.len(), 2);
    assert_eq!(&*records[1].fields[0].key, "tool");
    assert_eq!(&*records[1].fields[0].value, "read_file");
    assert_eq!(&*records[1].fields[1].key, "elapsed_ms");
    assert_eq!(&*records[1].fields[1].value, "12");

    assert_eq!(records[2].level, Level::Warn);
    assert_eq!(&*records[2].component, "store");
    assert_eq!(&*records[2].message, "db error");
    assert_eq!(records[2].fields.len(), 1);
    assert_eq!(&*records[2].fields[0].key, "category");
    assert_eq!(&*records[2].fields[0].value, "constraint");

    assert_eq!(records[3].level, Level::Error);
    assert_eq!(&*records[3].component, "wake");
    assert_eq!(&*records[3].message, "mic lost");
    assert!(records[3].fields.is_empty());

    assert_eq!(records[4].level, Level::Debug);
    assert_eq!(&*records[4].component, "overlay");
    assert_eq!(&*records[4].message, "card set");
    assert_eq!(records[4].fields.len(), 2);
    assert_eq!(&*records[4].fields[0].key, "id");
    assert_eq!(&*records[4].fields[0].value, "2");
    assert_eq!(&*records[4].fields[1].key, "has_card");
    assert_eq!(&*records[4].fields[1].value, "true");

    assert_eq!(records[5].level, Level::Info);
    assert_eq!(&*records[5].component, "stt");
    assert_eq!(&*records[5].message, "transcribed");
    assert_eq!(records[5].fields.len(), 2);
    assert_eq!(&*records[5].fields[0].key, "samples");
    assert_eq!(&*records[5].fields[0].value, "1600");
    assert_eq!(&*records[5].fields[1].key, "chars");
    assert_eq!(&*records[5].fields[1].value, "8");

    assert!(records[0].location.line > 0);
    assert!(!records[0].thread.is_empty());
}
