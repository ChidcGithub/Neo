// Opt-in foreground metadata probe. Never queries Name, Value, TextPattern or password contents.
use super::*;
use serde_json::{json, Value};
use windows::core::Interface;
use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT,
    WM_NCHITTEST, HTMINBUTTON, HTMAXBUTTON, HTCLOSE,
};

type NativeHwnd = windows_sys::Win32::Foundation::HWND;

fn failure(stage: &str, e: windows::core::Error) -> String {
    format!("{stage}: HRESULT=0x{:08X}", e.code().0 as u32)
}

fn read<T>(stage: &str, result: WinResult<T>) -> Result<T, String> {
    result.map_err(|e| failure(stage, e))
}

fn pack_point(point: (i32, i32)) -> Option<isize> {
    let x = i16::try_from(point.0).ok()?;
    let y = i16::try_from(point.1).ok()?;
    Some(((u32::from(y as u16) << 16) | u32::from(x as u16)) as isize)
}

fn nc_hit(hwnd: NativeHwnd, point: (i32, i32)) -> Result<i32, String> {
    let packed = pack_point(point).ok_or("WM_NCHITTEST coordinate outside signed 16-bit range")?;
    let mut result = 0usize;
    let (ok, error) = unsafe {
        SetLastError(0);
        let ok = SendMessageTimeoutW(hwnd, WM_NCHITTEST, 0, packed,
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT, 200, &mut result);
        (ok, GetLastError())
    };
    if ok == 0 {
        return Err(format!("WM_NCHITTEST failed; Win32={error}; zero may mean unspecified failure/timeout; no result accepted"));
    }
    Ok(result as i32)
}

fn caption_code(code: i32) -> bool {
    [HTMINBUTTON as i32, HTMAXBUTTON as i32, HTCLOSE as i32].contains(&code)
}

fn check_foreground(hwnd: NativeHwnd, started: Instant) -> Result<(), String> {
    if started.elapsed() > Duration::from_secs(15) { return Err("probe cooperative deadline exceeded".into()); }
    if unsafe { GetForegroundWindow() } != hwnd { return Err("foreground changed; probe stopped".into()); }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Metadata {
    identity: ElementIdentity,
    control_type: i32,
    native_hwnd: isize,
    rect: [i32; 4],
    enabled: bool,
    offscreen: bool,
}

fn metadata(el: &IUIAutomationElement, hwnd: NativeHwnd, top_id: &[i32]) -> Result<Metadata, String> {
    unsafe {
        if read("IsPassword", el.CurrentIsPassword())?.as_bool() {
            return Err("password element excluded before reading metadata".into());
        }
        Ok(Metadata {
            identity: read("identity", identity(el, hwnd as isize, top_id.to_vec()))?,
            control_type: read("ControlType", el.CurrentControlType())?.0,
            native_hwnd: read("NativeWindowHandle", el.CurrentNativeWindowHandle())?.0 as isize,
            rect: read("BoundingRectangle", el.CurrentBoundingRectangle().and_then(rect_values))?,
            enabled: read("IsEnabled", el.CurrentIsEnabled())?.as_bool(),
            offscreen: read("IsOffscreen", el.CurrentIsOffscreen())?.as_bool(),
        })
    }
}

fn metadata_json(m: &Metadata) -> Value {
    json!({"runtime_id": m.identity.runtime_id, "control_type": m.control_type,
        "native_hwnd": m.native_hwnd, "process_id": m.identity.process_id,
        "process_started": m.identity.process_started, "rect": m.rect,
        "enabled": m.enabled, "offscreen": m.offscreen})
}

struct Candidate {
    el: IUIAutomationElement,
    parent: IUIAutomationElement,
    metadata: Metadata,
    parent_type: i32,
    parent_runtime_id: Vec<i32>,
    selection_ht: Option<i32>,
    selection_ht_error: Option<String>,
}

#[derive(Default)]
struct Coverage {
    visits: usize,
    pruned: usize,
    truncated: bool,
    errors: usize,
    samples: Vec<String>,
}

impl Coverage {
    fn error(&mut self, error: String) {
        self.errors += 1;
        if self.samples.len() < 8 && !self.samples.contains(&error) { self.samples.push(error); }
    }

    fn json(&self) -> Value {
        json!({"visits": self.visits, "pruned_subtrees": self.pruned, "truncated": self.truncated,
            "error_count": self.errors, "error_samples": self.samples,
            "scope": "structural_containers_near_top_only_not_full_snapshot"})
    }
}

// windows-core 0.62.2 converts successful null interfaces to Error::empty() (code 0),
// not E_POINTER. Use the ABI to distinguish end of tree from a real provider error.
fn navigate(walker: &IUIAutomationTreeWalker, el: &IUIAutomationElement, direction: &str) -> Result<Option<IUIAutomationElement>, String> {
    unsafe {
        let mut result = core::ptr::null_mut();
        let hr = match direction {
            "first_child" => (walker.vtable().GetFirstChildElement)(walker.as_raw(), el.as_raw(), &mut result),
            "next_sibling" => (walker.vtable().GetNextSiblingElement)(walker.as_raw(), el.as_raw(), &mut result),
            "parent" => (walker.vtable().GetParentElement)(walker.as_raw(), el.as_raw(), &mut result),
            _ => unreachable!(),
        };
        navigation_result(direction, hr, result)
    }
}

// A non-null pointer must be an owned interface returned by the UIA walker.
unsafe fn navigation_result(stage: &str, hr: windows::core::HRESULT, result: *mut core::ffi::c_void) -> Result<Option<IUIAutomationElement>, String> {
    // Own any returned interface even on failure, so a malformed provider cannot leak it.
    let element = if result.is_null() { None } else { Some(unsafe { IUIAutomationElement::from_raw(result) }) };
    read(stage, hr.ok())?;
    Ok(element)
}

// Name-free counterpart of collect/dfs: same ControlViewWalker, identity and rect helpers.
// Traverse only structural containers near the window top; never enter Document/Edit/Text.
fn collect_caption_metadata(uia: &IUIAutomation, hwnd: NativeHwnd, started: Instant) -> Result<(Vec<Candidate>, Coverage), String> {
    unsafe {
        let top = read("ElementFromHandle", uia.ElementFromHandle(HWND(hwnd)))?;
        let top_id = read("top.GetRuntimeId", runtime_id(&top))?;
        let walker = read("ControlViewWalker", uia.ControlViewWalker())?;
        let top_rect = read("top.rect", top.CurrentBoundingRectangle().and_then(rect_values))?;
        let mut stack = vec![(top, 0usize)];
        let mut candidates = Vec::new();
        let mut coverage = Coverage::default();
        while let Some((parent, depth)) = stack.pop() {
            check_foreground(hwnd, started)?;
            if coverage.visits >= 256 { coverage.truncated = true; break; }
            if read("parent.IsPassword", parent.CurrentIsPassword())?.as_bool() { coverage.pruned += 1; continue; }
            let parent_type = read("parent.ControlType", parent.CurrentControlType())?.0;
            let mut child = match navigate(&walker, &parent, "first_child") {
                Ok(child) => child,
                Err(error) => { coverage.error(error); continue; }
            };
            while let Some(el) = child {
                check_foreground(hwnd, started)?;
                if coverage.visits >= 256 { coverage.truncated = true; break; }
                coverage.visits += 1;
                if !read("IsPassword", el.CurrentIsPassword())?.as_bool() {
                    let ctype = read("ControlType", el.CurrentControlType())?.0;
                    if ctype == 50000 {
                        match read("button.rect", el.CurrentBoundingRectangle().and_then(rect_values)) {
                            Ok(rect) => {
                                let point = (rect[0].saturating_add(rect[2] / 2), rect[1].saturating_add(rect[3] / 2));
                                let near_top = near_window_top(rect, top_rect);
                                let (ht, ht_error) = if parent_type == 50037 || near_top {
                                    check_foreground(hwnd, started)?;
                                    match nc_hit(hwnd, point) {
                                        Ok(code) => (Some(code), None),
                                        Err(error) => { coverage.error(error.clone()); (None, Some(error)) }
                                    }
                                } else { (None, None) };
                                if parent_type == 50037 || ht.is_some_and(caption_code) {
                                    candidates.push(Candidate { metadata: metadata(&el, hwnd, &top_id)?,
                                        parent: parent.clone(), parent_type,
                                        parent_runtime_id: read("parent.GetRuntimeId", runtime_id(&parent))?,
                                        selection_ht: ht, selection_ht_error: ht_error, el: el.clone() });
                                    if candidates.len() >= 12 { coverage.truncated = true; break; }
                                }
                            }
                            Err(error) => coverage.error(error),
                        }
                    } else if depth < 6 && matches!(ctype, 50037 | 50033 | 50026 | 50021 | 50025) {
                        let near_top = match read("container.rect", el.CurrentBoundingRectangle().and_then(rect_values)) {
                            Ok(rect) => near_window_top(rect, top_rect),
                            Err(error) => { coverage.error(error); false }
                        };
                        if ctype == 50037 || near_top { stack.push((el.clone(), depth + 1)); }
                        else { coverage.pruned += 1; }
                    } else { coverage.pruned += 1; }
                } else { coverage.pruned += 1; }
                child = match navigate(&walker, &el, "next_sibling") {
                    Ok(child) => child,
                    Err(error) => { coverage.error(error); break; }
                };
            }
            if coverage.truncated { break; }
        }
        Ok((candidates, coverage))
    }
}

fn near_window_top(rect: [i32; 4], top: [i32; 4]) -> bool {
    let band = [top[0], top[1], top[2], top[3].min(160)];
    super::super::intersects(rect, band)
}

fn raw_chain(uia: &IUIAutomation, raw: &IUIAutomationTreeWalker, start: &IUIAutomationElement,
    top: &IUIAutomationElement, top_identity: &ElementIdentity, other: &IUIAutomationElement,
    hwnd: NativeHwnd, started: Instant) -> Result<Value, String> {
    unsafe {
        let mut node = start.clone();
        let mut chain = Vec::new();
        let mut seen = Vec::new();
        let mut contains_other = false;
        let mut reaches_top = false;
        let mut stop = "depth_limit";
        let mut error = None;
        for _ in 0..16 {
            check_foreground(hwnd, started)?;
            if read("chain.IsPassword", node.CurrentIsPassword())?.as_bool() { stop = "password_excluded"; break; }
            let id = read("chain.identity", identity(&node, hwnd as isize, top_identity.runtime_id.clone()))?;
            if seen.contains(&id.runtime_id) { stop = "cycle"; break; }
            seen.push(id.runtime_id.clone());
            let native = read("chain.NativeWindowHandle", node.CurrentNativeWindowHandle())?.0 as isize;
            let same_top = read("CompareElements(top)", uia.CompareElements(&node, top))?.as_bool();
            contains_other |= read("CompareElements(other)", uia.CompareElements(&node, other))?.as_bool();
            chain.push(json!({"runtime_id": id.runtime_id, "process_id": id.process_id,
                "process_started": id.process_started, "native_hwnd": native,
                "control_type": read("chain.ControlType", node.CurrentControlType())?.0,
                "compare_fresh_top": same_top}));
            if native == hwnd as isize && id == *top_identity && same_top {
                reaches_top = true; stop = "verified_top"; break;
            }
            // Never cross into a different native window or the desktop root.
            if same_top || (native != 0 && native != hwnd as isize
                && GetAncestor(native as NativeHwnd, GA_ROOT) != hwnd) {
                stop = "native_boundary_or_top_identity_mismatch"; break;
            }
            match navigate(raw, &node, "parent") {
                Ok(Some(parent)) => node = parent,
                Ok(None) => { stop = "end_of_tree"; break; }
                Err(e) => { error = Some(e); stop = "provider_error"; break; }
            }
        }
        Ok(json!({"nodes": chain, "reaches_fresh_native_top": reaches_top,
            "contains_other_by_compare_elements": contains_other, "stop": stop, "error": error}))
    }
}

fn run_probe() -> Result<(), String> {
    let _dpi = crate::tools::screen::physical_pixels().map_err(|_| "physical pixel DPI guard failed")?;
    let _com = read("CoInitializeEx", ComGuard::new())?;
    let started = Instant::now();
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() { return Err("no foreground window".into()); }
        let mut class = [0u16; 256];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let chromium_class = n > 0 && String::from_utf16_lossy(&class[..n as usize]).starts_with("Chrome_WidgetWin_");
        // Only a boolean class-family classification is emitted, not the class string or app/title.
        println!("{}", json!({"event": "foreground_probe", "hwnd": hwnd as isize,
            "chromium_class_family": chromium_class, "class_query_ok": n > 0,
            "names_or_content_read": false, "input_injected": false, "window_activated": false,
            "coordinate_space": "physical_pixels", "metadata_only_collect": true}));
        let uia: IUIAutomation = read("CoCreateInstance", CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER))?;
        let raw = read("RawViewWalker", uia.RawViewWalker())?;
        let top = read("initial.ElementFromHandle", uia.ElementFromHandle(HWND(hwnd)))?;
        let top_identity = read("initial.identity", identity(&top, hwnd as isize, read("initial.GetRuntimeId", runtime_id(&top))?))?;
        let (first, coverage) = collect_caption_metadata(&uia, hwnd, started)?;
        let groups: Vec<_> = [HTMINBUTTON, HTMAXBUTTON, HTCLOSE].into_iter().map(|ht|
            json!({"native_ht": ht, "candidate_count": first.iter().filter(|c| c.selection_ht == Some(ht as i32)).count()})).collect();
        println!("{}", json!({"event": "collect", "caption_candidates": first.len(), "coverage": coverage.json(),
            "native_groups": groups, "grouping_is_not_identity_proof": true}));
        for candidate in &first {
            check_foreground(hwnd, started)?;
            let m = &candidate.metadata;
            let mut clickable = windows::Win32::Foundation::POINT::default();
            let clickable_result = read("GetClickablePoint", candidate.el.GetClickablePoint(&mut clickable));
            let preferred = clickable_result.as_ref().ok().filter(|v| v.as_bool())
                .map(|_| (clickable.x, clickable.y));
            let points = super::super::candidate_points(m.rect, preferred, &read("monitors", monitors())?);
            println!("{}", json!({"event": "candidate", "collect": metadata_json(m),
                "parent_control_type": candidate.parent_type, "parent_runtime_id": candidate.parent_runtime_id,
                "selection_ht": candidate.selection_ht, "selection_ht_error": candidate.selection_ht_error,
                "preferred": preferred, "clickable_point_error": clickable_result.err(), "point_count": points.len()}));
            for point in points.into_iter().take(6) {
                check_foreground(hwnd, started)?;
                let native = WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 });
                let root = GetAncestor(native, GA_ROOT);
                if root != hwnd {
                    println!("{}", json!({"event": "point", "point": point, "native_hit": native as isize,
                        "native_root": root as isize, "native_root_matches": false, "uia_skipped": "foreign_native_hit"}));
                    continue;
                }
                let ht = nc_hit(hwnd, point);
                check_foreground(hwnd, started)?;
                let fresh_top = read("fresh.ElementFromHandle", uia.ElementFromHandle(HWND(hwnd)))?;
                let fresh_identity = read("fresh.identity", identity(&fresh_top, hwnd as isize, read("fresh.GetRuntimeId", runtime_id(&fresh_top))?))?;
                if fresh_identity != top_identity || fresh_identity.runtime_id != m.identity.window_runtime_id
                    || !read("CompareElements(fresh_top)", uia.CompareElements(&top, &fresh_top))?.as_bool() {
                    return Err("top identity changed since collection".into());
                }
                let hit = read("ElementFromPoint", uia.ElementFromPoint(windows::Win32::Foundation::POINT { x: point.0, y: point.1 }));
                let evidence = match hit {
                    Err(e) => json!({"error": e}),
                    Ok(hit) => {
                        let hit_metadata = metadata(&hit, hwnd, &fresh_identity.runtime_id);
                        let hit_chain = raw_chain(&uia, &raw, &hit, &fresh_top, &fresh_identity, &candidate.el, hwnd, started)?;
                        let target_chain = raw_chain(&uia, &raw, &candidate.el, &fresh_top, &fresh_identity, &hit, hwnd, started)?;
                        json!({"metadata": hit_metadata.as_ref().map(metadata_json).ok(), "metadata_error": hit_metadata.err(),
                            "compare_collected": read("CompareElements(target)", uia.CompareElements(&candidate.el, &hit))?.as_bool(),
                            "compare_collected_parent": read("CompareElements(parent)", uia.CompareElements(&candidate.parent, &hit))?.as_bool(),
                            "hit_raw_chain": hit_chain, "target_raw_chain": target_chain})
                    }
                };
                check_foreground(hwnd, started)?;
                println!("{}", json!({"event": "point", "point": point, "native_hit": native as isize,
                    "native_root": root as isize, "native_root_matches": root == hwnd,
                    "native_root_still_matches": GetAncestor(WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: point.0, y: point.1 }), GA_ROOT) == hwnd,
                    "nc_hit": ht.as_ref().ok(), "nc_hit_error": ht.err(), "uia": evidence}));
            }
        }
        let (second, second_coverage) = collect_caption_metadata(&uia, hwnd, started)?;
        for original in &first {
            let matches: Vec<_> = second.iter().filter(|c| c.metadata == original.metadata
                && c.parent_type == original.parent_type && c.parent_runtime_id == original.parent_runtime_id).collect();
            println!("{}", json!({"event": "recollect", "runtime_id": original.metadata.identity.runtime_id,
                "exact_metadata_matches": matches.len(), "names_not_compared": true,
                "compare_elements": if matches.len() == 1 { Some(read("CompareElements(recollect)", uia.CompareElements(&original.el, &matches[0].el))?.as_bool()) } else { None }}));
        }
        check_foreground(hwnd, started)?;
        println!("{}", json!({"event": "probe_complete", "foreground_unchanged": true,
            "recollect_coverage": second_coverage.json(), "elapsed_ms": started.elapsed().as_millis()}));
    }
    Ok(())
}

#[test]
fn nchittest_coordinates_reject_truncation_and_preserve_negative_values() {
    assert_eq!(pack_point((-1, -2)), Some(0xFFFEFFFFu32 as isize));
    assert!(pack_point((32768, 0)).is_none());
    assert!(pack_point((0, -32769)).is_none());
    assert!(caption_code(HTMINBUTTON as i32));
    assert!(caption_code(HTMAXBUTTON as i32));
    assert!(caption_code(HTCLOSE as i32));
    assert!(!caption_code(1));
}

#[test]
fn walker_null_success_is_not_confused_with_provider_failure() {
    use windows::core::HRESULT;
    unsafe {
        assert!(navigation_result("test", HRESULT(0), core::ptr::null_mut()).unwrap().is_none());
        for hr in [0x80004003u32, 0x80070005, 0x80040201] {
            let error = navigation_result("test", HRESULT(hr as i32), core::ptr::null_mut()).unwrap_err();
            assert!(error.contains(&format!("HRESULT=0x{hr:08X}")));
        }
    }
}

#[test]
fn caption_scope_excludes_nonintersecting_regions_and_reports_incomplete_coverage() {
    let top = [-100, -200, 800, 600];
    assert!(near_window_top([-80, -190, 40, 30], top));
    assert!(!near_window_top([-80, -400, 40, 30], top));
    assert!(!near_window_top([900, -190, 40, 30], top));
    assert!(!near_window_top([-80, 0, 40, 30], top));
    let mut coverage = Coverage::default();
    for _ in 0..12 { coverage.error("provider failure".into()); }
    assert_eq!(coverage.errors, 12);
    assert_eq!(coverage.samples.len(), 1);
    assert_eq!(coverage.json()["error_count"], 12);
}

#[test]
#[ignore = "opt-in read-only foreground caption metadata; no names/content/input/activation"]
fn foreground_caption_metadata_probe() {
    assert_eq!(std::env::var("NEO_UIA_TITLEBAR_PROBE").as_deref(), Ok("1"),
        "set NEO_UIA_TITLEBAR_PROBE=1 and explicitly run this ignored test");
    if let Err(error) = run_probe() { panic!("foreground metadata probe stopped: {error}"); }
}
