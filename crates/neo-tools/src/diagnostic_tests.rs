use super::*;

#[derive(Debug)]
struct Chain {
    message: String,
    next: Option<Box<Chain>>,
}

impl fmt::Display for Chain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for Chain {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.next.as_deref().map(|next| next as &dyn Error)
    }
}

fn chain(count: usize, message: &str) -> Chain {
    assert!(count > 0);
    Chain {
        message: message.to_owned(),
        next: (count > 1).then(|| Box::new(chain(count - 1, message))),
    }
}

#[derive(Debug)]
struct Untouched;

impl fmt::Display for Untouched {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        panic!("disabled diagnostics must not format sources")
    }
}

impl Error for Untouched {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        panic!("disabled diagnostics must not traverse sources")
    }
}

#[test]
fn disabled_capture_never_touches_source() {
    with_enabled_for_test(false, || {
        assert!(!is_enabled());
        assert!(ErrorTrace::capture(Some(&Untouched)).is_none());
        assert!(crate::ToolError::io("public")
            .with_source(&Untouched)
            .diagnostic
            .is_none());
        let captured = with_enabled_for_test(true, || crate::ToolError::io("public"));
        let original = captured.diagnostic.clone().unwrap();
        let unchanged = captured.with_source(&Untouched);
        assert!(Arc::ptr_eq(
            &original,
            unchanged.diagnostic.as_ref().unwrap()
        ));
    });
}

#[test]
fn capture_has_caller_full_stack_and_ordered_multiline_causes() {
    with_enabled_for_test(true, || {
        let error = Chain {
            message: "outer\n  detail\r\n".into(),
            next: Some(Box::new(chain(1, "inner"))),
        };
        let expected_line = line!() + 1;
        let trace = ErrorTrace::capture(Some(&error)).unwrap();
        assert_eq!(trace.location.file, file!());
        assert_eq!(trace.location.line, expected_line);
        assert!(trace.location.column > 0);
        assert!(!trace.backtrace.is_empty());
        assert!(trace.backtrace.contains('\n'));
        assert!(!trace.backtrace.contains("disabled backtrace"));
        // Do not assume debug symbols, frame count, or platform-specific names.
        assert!(
            trace.location.file.len()
                + trace.backtrace.len()
                + trace.causes.iter().map(String::len).sum::<usize>()
                <= MAX_REPORT_BYTES
        );
        if !trace.truncated {
            assert_eq!(trace.causes, ["outer\n  detail\r\n", "inner"]);
        }
    });
}

#[test]
fn causes_keep_order_and_line_breaks() {
    let error = Chain {
        message: "outer\n  detail\r\n".into(),
        next: Some(Box::new(chain(1, "inner"))),
    };
    let (causes, truncated) = capture_causes(&error, MAX_REPORT_BYTES);
    assert_eq!(causes, ["outer\n  detail\r\n", "inner"]);
    assert!(!truncated);
}

#[test]
fn cause_count_is_bounded_without_false_truncation_at_exact_limit() {
    for count in [MAX_CAUSES, MAX_CAUSES + 1] {
        let (causes, truncated) = capture_causes(&chain(count, "cause"), MAX_REPORT_BYTES);
        assert_eq!(causes.len(), MAX_CAUSES);
        assert_eq!(truncated, count > MAX_CAUSES);
    }
}

#[test]
fn detects_source_cycles() {
    #[derive(Debug)]
    struct Cycle(u8);
    impl fmt::Display for Cycle {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "cycle {}", self.0)
        }
    }
    impl Error for Cycle {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(self)
        }
    }
    let (causes, truncated) = capture_causes(&Cycle(1), MAX_REPORT_BYTES);
    assert_eq!(causes, ["cycle 1"]);
    assert!(truncated);
}

#[test]
fn wrapper_and_source_at_same_address_are_not_a_cycle() {
    #[derive(Debug)]
    #[repr(transparent)]
    struct Wrapper(std::io::Error);
    impl fmt::Display for Wrapper {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("wrapper")
        }
    }
    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }
    let (causes, truncated) =
        capture_causes(&Wrapper(std::io::Error::other("inner")), MAX_REPORT_BYTES);
    assert_eq!(causes, ["wrapper", "inner"]);
    assert!(!truncated);
}

#[test]
fn cause_messages_and_aggregate_text_are_utf8_bounded() {
    let error = chain(MAX_CAUSES, &"汉".repeat(MAX_CAUSE_BYTES));
    let (causes, truncated) = capture_causes(&error, MAX_REPORT_BYTES);
    assert!(truncated);
    assert!(causes.iter().all(|cause| cause.len() <= MAX_CAUSE_BYTES));
    assert!(causes.iter().map(String::len).sum::<usize>() <= MAX_REPORT_BYTES);
    assert!(causes
        .iter()
        .all(|cause| cause.chars().all(|ch| ch == '汉')));
    let (causes, truncated) = capture_causes(&chain(1, "abc"), 3);
    assert_eq!(causes, ["abc"]);
    assert!(!truncated);
    let (causes, truncated) = capture_causes(&error, 0);
    assert!(causes.is_empty());
    assert!(truncated);
}

#[test]
fn bounded_format_preserves_whitespace_and_reports_clipping() {
    let text = "frame\n  汉\r\n";
    assert_eq!(
        bounded_format(format_args!("{text}"), text.len()),
        (text.into(), false)
    );
    assert_eq!(
        bounded_format(format_args!("{text}"), 10),
        ("frame\n  ".into(), true)
    );
    let long = "frame\n".repeat(MAX_REPORT_BYTES);
    let (stack, truncated) = bounded_format(format_args!("{long}"), MAX_REPORT_BYTES);
    assert_eq!(stack.len(), MAX_REPORT_BYTES);
    assert!(long.starts_with(&stack));
    assert!(truncated);
    let mut output = BoundedText {
        text: String::new(),
        limit: 2,
        truncated: false,
    };
    assert!(output.write_str("汉").is_err());
    assert!(output.write_str("x").is_err());
    assert!(output.text.is_empty());
}

#[test]
fn replacing_causes_respects_remaining_report_budget() {
    let mut trace = ErrorTrace {
        location: SourceLocation {
            file: "file".into(),
            line: 1,
            column: 1,
        },
        backtrace: "s".repeat(MAX_REPORT_BYTES - 6),
        causes: vec!["old".into()],
        truncated: false,
    };
    trace.replace_causes(&chain(1, "abcd"));
    assert_eq!(trace.causes, ["ab"]);
    assert!(trace.truncated);
    assert_eq!(
        trace.location.file.len() + trace.backtrace.len() + trace.causes[0].len(),
        MAX_REPORT_BYTES
    );
}

#[test]
fn scoped_override_is_nested_panic_safe_and_thread_local() {
    with_enabled_for_test(false, || {
        with_enabled_for_test(true, || {
            assert!(is_enabled());
            std::thread::spawn(|| with_enabled_for_test(false, || assert!(!is_enabled())))
                .join()
                .unwrap();
            assert!(is_enabled());
        });
        assert!(!is_enabled());
        let result = std::panic::catch_unwind(|| {
            with_enabled_for_test(true, || panic!("test restoration"));
        });
        assert!(result.is_err());
        assert!(!is_enabled());
    });
}

#[test]
fn read_bounded_attaches_concrete_io_source_only_when_enabled() {
    // Empty paths cannot name a file; no filesystem mutation or fixture required.
    let path = std::path::Path::new("");
    let expected = std::fs::File::open(path).unwrap_err().to_string();
    with_enabled_for_test(true, || {
        let error = crate::tools::read_file::read_bounded(path, 1).unwrap_err();
        assert_eq!(error.message, expected);
        let trace = error.diagnostic.unwrap();
        assert!(trace.location.file.ends_with("read_file.rs"));
        if !trace.truncated {
            assert_eq!(trace.causes, [expected.clone()]);
        }
    });
    with_enabled_for_test(false, || {
        let error = crate::tools::read_file::read_bounded(path, 1).unwrap_err();
        assert_eq!(error.message, expected);
        assert!(error.diagnostic.is_none());
    });
}
