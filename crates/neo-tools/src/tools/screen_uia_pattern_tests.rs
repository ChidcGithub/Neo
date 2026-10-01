// In-process mock COM only: no COM activation, UIA service, desktop reads or input.
use super::*;
use core::{ffi::c_void, mem::size_of};
use std::sync::{Arc, Mutex, atomic::{AtomicU32, AtomicUsize, Ordering}};
use windows::core::{BOOL, GUID, HRESULT};
use windows::Win32::UI::Accessibility::{
    IUIAutomationElement_Vtbl, UIA_CONTROLTYPE_ID, UIA_PATTERN_ID,
    UIA_InvokePatternId, UIA_TogglePatternId, UIA_SelectionItemPatternId, UIA_ExpandCollapsePatternId,
};

const IDS: [UIA_PATTERN_ID; 4] = [UIA_InvokePatternId, UIA_TogglePatternId, UIA_SelectionItemPatternId, UIA_ExpandCollapsePatternId];
const STAGES: [&str; 4] = ["GetCurrentPattern(Invoke)", "GetCurrentPattern(Toggle)", "GetCurrentPattern(SelectionItem)", "GetCurrentPattern(ExpandCollapse)"];

#[derive(Default)]
struct Counts {
    queries: Mutex<Vec<i32>>,
    addrefs: AtomicUsize,
    releases: AtomicUsize,
    drops: AtomicUsize,
    properties: AtomicUsize,
}

struct Config {
    // (raw HRESULT, return an owned IUnknown). The latter also exercises failure cleanup.
    patterns: [(u32, bool); 4],
    control_type: i32,
    control_hr: u32,
    focusable: bool,
    password: bool,
    property_hr: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self { patterns: [(0, false); 4], control_type: 50020, control_hr: 0,
            focusable: false, password: false, property_hr: 0 }
    }
}

#[repr(C)]
struct MockElement {
    vtable: *const IUIAutomationElement_Vtbl,
    refs: AtomicU32,
    owned_vtable: Box<IUIAutomationElement_Vtbl>,
    config: Config,
    counts: Arc<Counts>,
}

impl Drop for MockElement {
    fn drop(&mut self) { self.counts.drops.fetch_add(1, Ordering::SeqCst); }
}

unsafe extern "system" fn query_interface(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        out.write(core::ptr::null_mut());
        if *iid == IUnknown::IID || *iid == IUIAutomationElement::IID {
            add_ref(this);
            out.write(this);
            HRESULT(0)
        } else { HRESULT(0x80004002u32 as i32) }
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let object = unsafe { &*this.cast::<MockElement>() };
    object.counts.addrefs.fetch_add(1, Ordering::SeqCst);
    object.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let object = unsafe { &*this.cast::<MockElement>() };
    object.counts.releases.fetch_add(1, Ordering::SeqCst);
    let remaining = object.refs.fetch_sub(1, Ordering::SeqCst) - 1;
    if remaining == 0 { drop(unsafe { Box::from_raw(this.cast::<MockElement>()) }); }
    remaining
}

unsafe extern "system" fn get_pattern(this: *mut c_void, id: UIA_PATTERN_ID, out: *mut *mut c_void) -> HRESULT {
    unsafe {
        let object = &*this.cast::<MockElement>();
        object.counts.queries.lock().unwrap().push(id.0);
        let Some(index) = IDS.iter().position(|known| *known == id) else {
            out.write(core::ptr::null_mut());
            return HRESULT(0x80070057u32 as i32);
        };
        let (hr, supported) = object.config.patterns[index];
        // Only IUnknown is exposed as the pattern: action invocation is impossible.
        if supported { add_ref(this); out.write(this); }
        else { out.write(core::ptr::null_mut()); }
        HRESULT(hr as i32)
    }
}

unsafe extern "system" fn control_type(this: *mut c_void, out: *mut UIA_CONTROLTYPE_ID) -> HRESULT {
    let object = unsafe { &*this.cast::<MockElement>() };
    unsafe { out.write(UIA_CONTROLTYPE_ID(object.config.control_type)); }
    HRESULT(object.config.control_hr as i32)
}

unsafe extern "system" fn focusable(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    let object = unsafe { &*this.cast::<MockElement>() };
    object.counts.properties.fetch_add(1, Ordering::SeqCst);
    unsafe { out.write(BOOL(i32::from(object.config.focusable))); }
    HRESULT(object.config.property_hr as i32)
}

unsafe extern "system" fn password(this: *mut c_void, out: *mut BOOL) -> HRESULT {
    let object = unsafe { &*this.cast::<MockElement>() };
    object.counts.properties.fetch_add(1, Ordering::SeqCst);
    unsafe { out.write(BOOL(i32::from(object.config.password))); }
    HRESULT(object.config.property_hr as i32)
}

fn mock(config: Config) -> (IUIAutomationElement, Arc<Counts>) {
    // The generated implementation trait requires Ole/Variant features not enabled by
    // neo-tools. Initialize the actual generated vtable's storage instead. On Windows
    // every slot is a function pointer (or a feature-disabled usize placeholder).
    // A non-null function address is valid storage for each; only the correctly typed
    // slots installed below may be called. Never zero-initialize Rust function pointers.
    unsafe extern "system" fn unused() { std::process::abort(); }
    let mut table = core::mem::MaybeUninit::<IUIAutomationElement_Vtbl>::uninit();
    assert_eq!(size_of::<IUIAutomationElement_Vtbl>() % size_of::<unsafe extern "system" fn()>(), 0);
    unsafe {
        let slots = table.as_mut_ptr().cast::<unsafe extern "system" fn()>();
        for i in 0..size_of::<IUIAutomationElement_Vtbl>() / size_of::<unsafe extern "system" fn()>() {
            slots.add(i).write(unused);
        }
        let table_ptr = table.as_mut_ptr();
        core::ptr::addr_of_mut!((*table_ptr).base__).write(windows::core::IUnknown_Vtbl {
            QueryInterface: query_interface, AddRef: add_ref, Release: release,
        });
        core::ptr::addr_of_mut!((*table_ptr).GetCurrentPattern).write(get_pattern);
        core::ptr::addr_of_mut!((*table_ptr).CurrentControlType).write(control_type);
        core::ptr::addr_of_mut!((*table_ptr).CurrentIsKeyboardFocusable).write(focusable);
        core::ptr::addr_of_mut!((*table_ptr).CurrentIsPassword).write(password);
        let owned_vtable = Box::new(table.assume_init());
        let counts = Arc::new(Counts::default());
        let object = Box::new(MockElement { vtable: &*owned_vtable, refs: AtomicU32::new(1),
            owned_vtable, config, counts: counts.clone() });
        (IUIAutomationElement::from_raw(Box::into_raw(object).cast()), counts)
    }
}

fn queries(counts: &Counts) -> Vec<i32> { counts.queries.lock().unwrap().clone() }

#[test]
fn generated_wrapper_null_success_is_error_zero_but_raw_helper_is_absent() {
    let (el, counts) = mock(Config::default());
    let error = unsafe { el.GetCurrentPattern(UIA_InvokePatternId) }.unwrap_err();
    assert_eq!(error.code().0, 0, "pin the actual windows 0.62.2 wrapper behavior");
    assert!(current_pattern(&el, UIA_InvokePatternId).unwrap().is_none());
    assert!(!action_pattern_result(&el).unwrap());
    assert!(passive(&el).unwrap());
    assert_eq!(queries(&counts).len(), 10);
    assert_eq!(counts.addrefs.load(Ordering::SeqCst), 0);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 0);
    drop(el);
    assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
}

#[test]
fn absent_patterns_do_not_hide_later_support_and_owned_interfaces_are_released() {
    for supported in 0..4 {
        let mut config = Config::default();
        config.patterns[supported] = (0, true);
        let (el, counts) = mock(config);
        let wrapped = unsafe { el.GetCurrentPattern(IDS[supported]) }.unwrap();
        assert_eq!(counts.releases.load(Ordering::SeqCst), 0);
        drop(wrapped);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
        assert!(action_pattern_result(&el).unwrap());
        assert!(!passive(&el).unwrap());
        let mut expected = vec![IDS[supported].0];
        expected.extend(IDS[..=supported].iter().map(|id| id.0));
        expected.extend(IDS[..=supported].iter().map(|id| id.0));
        assert_eq!(queries(&counts), expected);
        assert_eq!(counts.addrefs.load(Ordering::SeqCst), 3);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 3);
        drop(el);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 4);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn explicit_unsupported_pattern_errors_remain_absent() {
    let (el, counts) = mock(Config { patterns: [(0x80004002, false), (0x80040204, false),
        (0, false), (0, false)], ..Config::default() });
    assert!(!action_pattern_result(&el).unwrap());
    assert!(passive(&el).unwrap());
    assert_eq!(queries(&counts).len(), 8);
}

#[test]
fn real_pattern_errors_keep_hresult_and_specific_stage_and_fail_closed() {
    use crate::result::ErrorKind;
    for (hr, kind) in [(0x80004003, ErrorKind::Io), (0x80070005, ErrorKind::NotAllowed),
        (0x80131505, ErrorKind::Timeout), (0x800705B4, ErrorKind::Timeout),
        (0x80040201, ErrorKind::NotFound), (0x80004005, ErrorKind::Io)] {
        for failed in 0..4 {
            let mut config = Config::default();
            config.patterns[failed] = (hr, false);
            // A later success must not erase an earlier permission/timeout/provider failure.
            if failed < 3 { config.patterns[failed + 1] = (0, true); }
            let (el, counts) = mock(config);
            let wrapped = unsafe { el.GetCurrentPattern(IDS[failed]) }.unwrap_err();
            assert_eq!(wrapped.code().0 as u32, hr);
            let failure = action_pattern_result(&el).unwrap_err();
            assert_eq!(failure.kind, kind);
            assert_eq!(failure.detail, format!("stage={}, HRESULT=0x{hr:08X}", STAGES[failed]));
            let passive_failure = passive(&el).unwrap_err();
            assert_eq!(passive_failure.detail, failure.detail);
            assert_eq!(counts.properties.load(Ordering::SeqCst), 0);
            assert_eq!(counts.addrefs.load(Ordering::SeqCst), 0);
            assert_eq!(queries(&counts).len(), 1 + 2 * (failed + 1));
            assert_eq!(action_pattern_state(&el), None);
            assert!(!action_pattern(&el));
        }
    }
}

#[test]
fn raw_pattern_helper_releases_nonnull_outputs_even_on_failure() {
    for hr in [0x80004003, 0x80070005, 0x80131505, 0x80004002, 0x80040204] {
        let mut config = Config::default();
        config.patterns[0] = (hr, true);
        let (el, counts) = mock(config);
        // Do not use the generated wrapper on malformed failure + non-null output:
        // its and_then never adopts that pointer. The production helper must do so.
        assert_eq!(current_pattern(&el, IDS[0]).unwrap_err().code().0 as u32, hr);
        assert_eq!(counts.addrefs.load(Ordering::SeqCst), 1);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 1);
        drop(el);
        assert_eq!(counts.releases.load(Ordering::SeqCst), 2);
        assert_eq!(counts.drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn passive_non_content_mock_never_queries_patterns_or_passive_properties() {
    for control_type in [50000, 50037, 50032, 50033, 50025] {
        let (el, counts) = mock(Config { control_type, patterns: [(0x80070005, false); 4],
            property_hr: 0x80131505, ..Config::default() });
        assert!(!passive(&el).unwrap());
        assert!(queries(&counts).is_empty());
        assert_eq!(counts.properties.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn passive_content_mock_preserves_property_gates_and_failures() {
    for control_type in [50020, 50006] {
        for (focusable, password, allowed) in [(false, false, true), (true, false, false), (false, true, false)] {
            let (el, _) = mock(Config { control_type, focusable, password, ..Config::default() });
            assert_eq!(passive(&el).unwrap(), allowed);
        }
        for hr in [0x80070005, 0x80131505] {
            let (el, _) = mock(Config { control_type, property_hr: hr, ..Config::default() });
            assert_eq!(passive(&el).unwrap_err().detail,
                format!("stage=passive_hit.CurrentIsKeyboardFocusable, HRESULT=0x{hr:08X}"));
        }
    }
    for hr in [0x80070005, 0x80131505] {
        let (el, counts) = mock(Config { control_type: 50037, control_hr: hr, ..Config::default() });
        assert_eq!(passive(&el).unwrap_err().detail,
            format!("stage=passive_hit.CurrentControlType, HRESULT=0x{hr:08X}"));
        assert!(queries(&counts).is_empty());
    }
}
