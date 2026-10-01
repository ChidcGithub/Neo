//! Opt-in, local-only error diagnostics; never include these in model messages.
//!
//! Capture records the error's creation stack, not the original failing OS stack.
//! This is not a complete machine dump: locals, registers and memory are not
//! captured, and frames may have unknown symbols (especially in stripped builds).
//! Paths and source messages can contain secrets. `ErrorTrace::Debug` exposes
//! them deliberately for local inspection; `ToolError::Debug` redacts its trace.
//! Disabling capture does not erase previously captured diagnostics.

use std::backtrace::Backtrace;
use std::error::Error;
use std::fmt::{self, Write};
use std::panic::Location;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static ENABLED: AtomicBool = AtomicBool::new(false);

// The budget covers retained UTF-8 text (file + stack + causes), not allocation
// overhead or UI formatting. Stream formatting so oversized messages are not
// first materialized as unbounded temporary Strings.
const MAX_REPORT_BYTES: usize = 64 * 1024;
const MAX_CAUSES: usize = 32;
const MAX_CAUSE_BYTES: usize = 4 * 1024;

/// Enable/disable future capture process-wide. Disabled by default; no env vars.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    #[cfg(test)]
    if let Some(enabled) = TEST_ENABLED.with(std::cell::Cell::get) {
        return enabled;
    }
    ENABLED.load(Ordering::Relaxed)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    pub file: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorTrace {
    pub location: SourceLocation,
    /// Full-style backtrace, preserving whitespace/newlines up to the byte cap.
    pub backtrace: String,
    /// Supplied error first, followed by `Error::source()` messages, in order.
    pub causes: Vec<String>,
    /// Any capture was clipped, cyclic, or could not be completely formatted.
    pub truncated: bool,
}

impl ErrorTrace {
    /// Capture only when enabled, regardless of `RUST_BACKTRACE`. Does not print
    /// or retain the borrowed source error. No source methods run when disabled.
    #[track_caller]
    pub fn capture(error: Option<&(dyn Error + 'static)>) -> Option<Arc<Self>> {
        if !is_enabled() {
            return None;
        }
        let caller = Location::caller();
        let (file, file_truncated) =
            bounded_format(format_args!("{}", caller.file()), MAX_REPORT_BYTES);
        let (backtrace, stack_truncated) = bounded_format(
            format_args!("{:#}", Backtrace::force_capture()),
            MAX_REPORT_BYTES - file.len(),
        );
        let mut trace = Self {
            location: SourceLocation {
                file,
                line: caller.line(),
                column: caller.column(),
            },
            backtrace,
            causes: Vec::new(),
            truncated: file_truncated || stack_truncated,
        };
        if let Some(error) = error {
            trace.replace_causes(error);
        }
        Some(Arc::new(trace))
    }

    // The caller checks the opt-in gate before cloning/mutating the trace.
    // Keep truncation sticky: replacing causes cannot undo a clipped capture.
    pub(crate) fn replace_causes(&mut self, error: &(dyn Error + 'static)) {
        let budget = MAX_REPORT_BYTES.saturating_sub(
            self.location
                .file
                .len()
                .saturating_add(self.backtrace.len()),
        );
        let (causes, truncated) = capture_causes(error, budget);
        self.causes = causes;
        self.truncated |= truncated;
    }
}

fn capture_causes(error: &(dyn Error + 'static), mut budget: usize) -> (Vec<String>, bool) {
    let mut causes = Vec::new();
    let mut seen: Vec<*const (dyn Error + 'static)> = Vec::new();
    let mut current = Some(error);
    let mut truncated = false;
    while let Some(error) = current {
        let pointer = error as *const (dyn Error + 'static);
        // Include vtable metadata: a wrapper and its first field can share an
        // address without being the same error. The count cap also bounds cycles
        // whose trait-object vtables are duplicated by code generation.
        if causes.len() == MAX_CAUSES
            || budget == 0
            || seen.iter().any(|previous| std::ptr::eq(*previous, pointer))
        {
            truncated = true;
            break;
        }
        seen.push(pointer);
        let (message, clipped) =
            bounded_format(format_args!("{error}"), budget.min(MAX_CAUSE_BYTES));
        budget -= message.len();
        causes.push(message);
        truncated |= clipped;
        current = error.source();
    }
    (causes, truncated)
}

struct BoundedText {
    text: String,
    limit: usize,
    truncated: bool,
}

impl Write for BoundedText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.truncated {
            return Err(fmt::Error);
        }
        let remaining = self.limit - self.text.len();
        if text.len() <= remaining {
            self.text.push_str(text);
            return Ok(());
        }
        let mut end = remaining;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.text.push_str(&text[..end]);
        self.truncated = true;
        Err(fmt::Error)
    }
}

fn bounded_format(args: fmt::Arguments<'_>, limit: usize) -> (String, bool) {
    let mut output = BoundedText {
        text: String::new(),
        limit,
        truncated: false,
    };
    if fmt::write(&mut output, args).is_err() {
        output.truncated = true;
    }
    (output.text, output.truncated)
}

#[cfg(test)]
std::thread_local! {
    static TEST_ENABLED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Scoped per-thread test override, restored even on panic; never mutates the
/// process-wide setting or environment and does not leak into other test threads.
#[cfg(test)]
pub(crate) fn with_enabled_for_test<T>(enabled: bool, test: impl FnOnce() -> T) -> T {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_ENABLED.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(TEST_ENABLED.with(|value| value.replace(Some(enabled))));
    test()
}

#[cfg(test)]
#[path = "diagnostic_tests.rs"]
mod tests;
