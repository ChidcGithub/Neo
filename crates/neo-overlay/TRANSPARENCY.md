# Passive layered overlay: evidence and validation boundary

The drawing HWND permanently keeps `WS_EX_LAYERED | WS_EX_TRANSPARENT |
WS_EX_NOACTIVATE`. Cards have no input API; native interactive viewports belong
to the application, not this crate.

## Why the HWND needs a transparent backing

Microsoft's [CreateTargetForHwnd documentation](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)
explicitly permits layered HWNDs and describes HWND drawing as the bottom layer,
with the DComp visual tree above it. Clearing the swapchain does not clear this
separate bottom layer. `SetLayeredWindowAttributes(..., 255, LWA_ALPHA)` alone
therefore does not establish a transparent HWND backing.

`initialize_passive_window` supplies an all-zero 32-bit premultiplied DIB using
[UpdateLayeredWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-updatelayeredwindow)
with `ULW_ALPHA`. It preserves the window position and supplies the full window
size, repeating on size changes while hidden. Temporary bitmap/DC resources are
released on both success and failure. Failure aborts initialization/reconfiguration
instead of falling back to an input-blocking style.

The [BLENDFUNCTION contract](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ns-wingdi-blendfunction)
requires `SourceConstantAlpha=255` for per-pixel alpha alone. Only the HWND backing
pixels are zero-alpha; no global alpha=0, visual opacity=0, color key, or layered
style toggling is used. The DComp swapchain still uses premultiplied alpha and
contains the cards/ripple. We do not call `SetLayeredWindowAttributes`: the
[layered-window documentation](https://learn.microsoft.com/en-us/windows/win32/winmsg/window-features#layered-windows)
says that doing so prevents subsequent ULW calls until the layered bit is reset.
It also documents that `WS_EX_TRANSPARENT` on a layered window ignores its shape
for mouse routing.

Local dependency source inspected: `wgpu-hal 30.0.1`, `src/dx12/dcomp.rs` and
`src/dx12/mod.rs`. `DxgiFromVisual` creates a target with
`CreateTargetForHwnd(hwnd, false)`, sets the root visual, then attaches the
swapchain with `SetContent` and calls `Commit`. Thus the documented layered-HWND
DComp model applies to this backend, not just to a hypothetical custom renderer.

## Taskbar / Shell fullscreen classification

The reported reproduction is: showing the wake edge effect makes the taskbar
disappear unless the taskbar has focus. Source inspection finds a `WS_POPUP`
covering `screen::virtual_screen()`, with `WS_EX_TOPMOST`, `SW_SHOWNA` and
`SetWindowPos(HWND_TOPMOST, ..., SWP_NOACTIVATE)` on show (also topmost positioning
in `refresh_display`). `WS_EX_TOOLWINDOW` suppresses a taskbar **button**;
layered/transparent and no-activate styles address input/activation. None of
these is a documented opt-out from Shell fullscreen classification.

Official contracts consulted for this change:

- [ITaskbarList2::MarkFullscreenWindow](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-itaskbarlist2-markfullscreenwindow):
  `TRUE` marks fullscreen and lets Shell lower the taskbar in Z-order when that
  window is active. **`FALSE` only removes the explicit mark**; automatic
  detection may still classify the HWND as fullscreen. Calling it with `FALSE`
  is therefore not a reliable fix, even if the COM call returns `S_OK`.
  The same documentation explicitly prescribes, since Windows 7,
  `SetProp(hwnd, L"NonRudeHWND", reinterpret_cast<HANDLE>(TRUE))` **before showing**
  to opt out of automatic detection and the resulting taskbar Z-order adjustment.
- [The Taskbar / Taskbar Display Options](https://learn.microsoft.com/en-us/windows/win32/shell/taskbar#taskbar-display-options)
  describes screen-sized borderless windows covering the taskbar, and separately
  describes the user's auto-hide setting. Losing visible taskbar pixels does not
  by itself establish that auto-hide was enabled or that `WS_VISIBLE` changed.
- [SetWindowPos](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)
  separates Z-order from activation: `SWP_NOACTIVATE` does not imply
  `SWP_NOZORDER`. A passive window can still affect ordering.
- [CreateTargetForHwnd](https://learn.microsoft.com/en-us/windows/win32/api/dcomp/nf-dcomp-idcompositiondevice-createtargetforhwnd)'s
  `topmost` argument is relative to the HWND's **children**, not Explorer's
  taskbar or other top-level HWNDs; changing that argument is not this fix.

### Selected minimal fix

`initialize_passive_window` now sets `NonRudeHWND=1` on our hidden drawing HWND
before ULW initialization and before any surface/show. Failure propagates through
existing initialization/reconfiguration error handling: do not display an
unprotected fullscreen overlay. The property survives hide/show and geometry
changes; repeated initialization is idempotent. The production wndproc removes
it during `WM_NCDESTROY`, as required by
[SetPropW](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setpropw).
The value is the documented TRUE sentinel, not an allocated/owned handle.

No Shell COM dependency, taskbar lookup, taskbar `ShowWindow`/topmost forcing,
`ABM_SETSTATE`, auto-hide registration, focus request, input injection, style
toggling, one-pixel geometry trick, or work-area cropping was added. Full virtual
desktop geometry, permanent layered/transparent/no-activate styles, ULW backing
and DComp rendering remain intact. The existing topmost placement of **our**
HWND is unchanged; the property is not a promise that every taskbar pixel will
always be above every topmost visual.

### What is established, and what remains an inference

The fullscreen popup and official opt-out contract establish a concrete Shell
classification risk. The focus-dependent symptom is consistent with that risk,
but no real Explorer trace was collected in this change, so it is **not proof**
that the reported machine lowered rather than auto-hid its taskbar.

Drawing is a separate possible contributor: the backing DIB is zero-alpha and
the passes clear to transparent, but `shader.wgsl` deliberately draws nonzero
alpha edge glow and refracted captured desktop pixels (the lens can be nearly
opaque). Thus an overlay above the taskbar can tint/obscure it or show an older
sample even with correct backing transparency. The existing offscreen checkerboard
test checks synthetic center/band pixels, not live taskbar composition. This
change does not mask the taskbar out of the shader or claim to rule out visual
occlusion. Avoid redesigning geometry/rendering without reproduction evidence.

For acceptance on an explicitly operated test machine/VM, compare before/after
with the taskbar unfocused/focused and with the user's auto-hide setting both off
and on. Check first wake, repeated show/hide, cards-only, resume, resize/DPI and
multi-monitor taskbars. Auto-hide off must keep normal taskbar availability;
auto-hide on must retain normal hiding and edge reveal, not force visibility.
Foreground/focus must remain with the prior application and clicks must pass
through. Include another genuinely fullscreen application: this overlay must
not override that application's legitimate Shell behavior.

If the symptom persists, record taskbar visibility, rectangle and Z-order plus
foreground HWND around show/hide and correlate them with actual displayed pixels.
Read-only `SHAppBarMessage(ABM_GETSTATE)` can compare configured auto-hide state;
it does **not** report whether the taskbar is currently visible. An unchanged
setting/`WS_VISIBLE` alone cannot distinguish lowering from pixel occlusion.
Use a controlled rendering comparison (transparent content versus glow/refraction)
on that test machine to isolate drawing if HWND state does not explain it. Do
not substitute forced taskbar activation/show/topmost for diagnosis. None of
these real-Shell acceptance steps was performed by the automated private-desktop
tests below.

## Frame/texture contract

- `render` distinguishes `Skipped` from `Presented`. Timeout, occlusion, loss,
  outdated configuration, and empty work never authorize showing the window.
- A surface texture is acquired **before** the egui pass. Failed acquisition
  cannot consume the initial font atlas or pending image deltas.
- Successfully uploaded/freed deltas are explicitly cleared to satisfy egui
  0.36's `TexturesDelta::drop` check.
- Hide, surface reconfiguration, and geometry/DPI changes invalidate readiness.
  The old window is hidden before resize/configure. Showing requires both a
  ready current surface and `Presented` from this iteration.
- `wgpu 30.0.1`'s `Queue::present` returns `()`, not a DXGI HRESULT or composition
  fence. `Presented` means submit/present calls returned normally; it is **not**
  proof of GPU completion, DWM composition completion, or physical scanout.

## Tests run on this Windows machine

```text
cargo test -p neo-overlay --locked
cargo test -p neo-overlay --locked --lib native_tests::surface_retry_preserves_egui_textures_and_invalidates_readiness -- --ignored --exact
cargo check -p neo-overlay --locked --all-targets
```

Re-run for the taskbar change (2026-10-01): default tests **32 passed, 2 ignored**;
the explicitly selected private-desktop GPU test **1 passed**; all-target check
passed. The preview-export test was not run. No application runtime or real
input-desktop overlay was started.

The default suite includes a fresh, never-activated desktop with no
`DESKTOP_SWITCHDESKTOP` permission. HWND creation and `WindowFromPoint` queries
run on different threads. An opaque layered `HTTRANSPARENT`-only control is hit;
the same control with a zero-alpha ULW backing is not. Restoring opaque pixels
restores the hit without style changes. The production passive style passes
through even **opaque** pixels to the foreign base window and native button,
including hide/reshow and resize. These are real OS window-selection queries,
not manually sent `WM_NCHITTEST` messages.

The added `non_rude_property_lifecycle_on_isolated_desktop` test uses the production
initializer and wndproc. It verifies the property before first show and during
show notifications, across hide/reshow and ULW resize, checks unchanged passive
styles and exact window dimensions, and observes removal during `WM_NCDESTROY`.
An invalid HWND fails at `SetPropW` before ULW/show. `GetActiveWindow`/`GetFocus`
remain null on that private window thread; this is not a real foreground/focus
interaction test. The existing cross-thread hit test also checks the property
while proving opaque-pixel pass-through. A pure wndproc test checks that varied
`WM_MOUSEACTIVATE` payloads always return `MA_NOACTIVATE`, without dispatching
input. The input API feature is a **dev dependency only**, for read-only queries.

There is no Explorer taskbar on these never-activated desktops. These tests do
**not** exercise Shell fullscreen heuristics, taskbar Z-order or auto-hide; a
property readback is only proof that our HWND exposes the documented declaration.

The opt-in GPU test also runs only on a private desktop, constructs `Gfx`
directly (no capture thread), injects surface acquisition failures, checks that
they never execute the card closure, then performs real DX12/DComp
acquire/upload/submit/present. It checks that both the pending image and initial
font atlas reached the egui renderer, and that forced reconfiguration hides the
window and revokes readiness until another real frame is submitted. It also
checks the NonRude property and passive styles on the actual `Gfx` HWND before
show and after reconfiguration. It passed
on this machine; it remains opt-in because it requires DX12/DComp support.

Neither test switches desktops, captures the user's desktop, injects mouse or
keyboard input, moves the cursor, or activates an application. Cleanup destroys
windows on their owning threads and joins them before closing the desktop.

## Remaining risks / evidence limits

- `WindowFromPoint` establishes cross-thread HWND selection, not delivery of
  physical mouse clicks, focus behavior under real interaction, or accessibility
  tool behavior. No hardware-input test was performed.
- The private desktop is not displayed. ULW alpha hit testing and successful
  DComp presentation establish API behavior, **not final composed pixel colors**.
  Visible cards/ripple over a transparent background, first-show flicker,
  multi-monitor hotplug, mixed DPI, HDR, remote sessions and driver/device loss
  still need visual validation. No claim of screenshot-based proof is made.
- The backing DIB costs roughly width × height × 4 bytes transiently, plus the
  system's retained layered bitmap; it is rebuilt only for initialization/resize.
- Fatal asynchronous GPU errors remain governed by wgpu's error handling;
  this change does not add a device-recovery subsystem.
- These native tests are confined to this crate; application state tests do not
  establish physical input delivery through eframe's native windows.

## Application integration rules

- Toast, mini status, screenshot flash and edge effects are passive. Never add
  buttons or input-host HWNDs to `Card`, or toggle the drawing HWND's input role.
- Permission/question, interruption and classroom dialogs use separate opaque
  eframe viewports with `mouse_passthrough(false)`. Their complete client area,
  including whitespace, is interactive rather than a hole to the desktop.
- Deferred callbacks explicitly wake `ViewportId::ROOT` for business receipts.
  Cloning an egui context does not bind it to the viewport where it was cloned.
- Interrupt receipts carry session/task identity and a revocable dialog token.
  A desktop suspension rejects new answers but preserves answers already accepted
  for the same task. Tool feedback does not start a new task epoch.
- Native desktop barriers synchronously hide and verify windows, but never replay
  a saved auxiliary HWND list or rewrite winit-owned extended styles. Each live
  viewport restores its own current geometry and visibility. Retired interruption
  generations stay retired.
- eframe calls `logic` before `ui`; `render` must not tick business logic again.
  egui layout retries can call both again without returning to the native event
  loop, so the hide/verify barrier only advances on pass index zero.
- Keep the existing offscreen resident permission/class viewports: logic-only
  output can command existing HWNDs but cannot create new deferred HWNDs. Changing
  this lifecycle requires testing tray startup and recovery on both backends.
- Classroom click exclusion uses the child viewport's latest physical client
  rectangle, not its animation target or ROOT DPI. This remains a sampled
  rectangle, not a record of window ownership at the exact hook timestamp.

## Validation after app integration

Workspace all-target check and release build passed. The built executable's
`--neo-render-probe dx12` and `--neo-render-probe glow` both returned zero.
Targeted app tests covered graphics/startup, all four auxiliary-window modules,
task epochs, restoration and desktop barriers (including synchronous layout
retry). State tests passed serially; the parallel run exposed two shared
screenshot-cache tests invalidating each other. No snapshot files were regenerated.

Before treating this as a device-matrix acceptance, manually verify on a test
machine/VM: clicks and dragging through toast/status/flash; dialog buttons,
body and whitespace retaining input; toast over a dialog; tray first-open;
suspension and recovery while a dialog closes; negative-origin/mixed-DPI screens;
transparent background and visible card pixels on first show and after resize.
Do not replace these checks with manually sent mouse messages or a successful
`present()` call.
