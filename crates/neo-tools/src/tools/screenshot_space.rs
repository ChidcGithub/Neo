//! 截图只提供坐标参考，不验证目标身份；不缓存图像内容，也不缩放像素。
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use crate::result::ToolError;
use super::screen::{self, Rect};

/// 仅桌面工具严格拒绝 null/非对象，不改变其他工具的默认参数语义。
pub fn validate_args(args: &crate::result::Args) -> Result<(), ToolError> {
    args.reject_unknown()?;
    let object = args.raw().as_object().ok_or_else(|| ToolError::bad_args("参数必须是 JSON 对象"))?;
    if let Some((key, _)) = object.iter().find(|(_, value)| value.is_null()) {
        return Err(ToolError::bad_args(format!("参数 `{key}` 不接受 null；不用的参数请省略")));
    }
    Ok(())
}

pub const TTL_SECS: u64 = 120;
const CAPACITY: usize = 16;

#[derive(Clone, Debug)]
pub struct ImageSpace {
    pub source: Rect,
    pub sent_width: u32,
    pub sent_height: u32,
    topology: Vec<Rect>,
}

fn canonical_topology(mut topology: Vec<Rect>) -> Vec<Rect> {
    topology.sort_unstable_by_key(|r| (r.x, r.y, r.width, r.height));
    topology.dedup();
    topology
}

pub fn rect_json(rect: Rect) -> Value {
    json!({"x": rect.x, "y": rect.y, "width": rect.width, "height": rect.height})
}

impl ImageSpace {
    pub fn new(source: Rect, sent_width: u32, sent_height: u32, topology: Vec<Rect>) -> Result<Self, ToolError> {
        if source.width <= 0 || source.height <= 0 || sent_width != source.width as u32 || sent_height != source.height as u32 {
            return Err(ToolError::bad_args("截图引用必须绑定原尺寸图像，不能猜测缩放关系"));
        }
        let bounds = screen::monitor_bounds(&topology)?;
        require_rect_inside(source, bounds)?;
        if screen::monitor_coverage(source, &topology) == 0 {
            return Err(ToolError::bad_args("截图区域完全落在显示器空洞中"));
        }
        Ok(Self { source, sent_width, sent_height, topology: canonical_topology(topology) })
    }

    pub fn point(&self, x: i32, y: i32) -> Result<(i32, i32), ToolError> {
        let image = Rect { x: 0, y: 0, width: self.sent_width as i32, height: self.sent_height as i32 };
        if !image.contains(x, y) { return Err(ToolError::bad_args("图内坐标超出截图像素范围")); }
        let x = self.source.x.checked_add(x).ok_or_else(|| ToolError::bad_args("图内 x 映射溢出"))?;
        let y = self.source.y.checked_add(y).ok_or_else(|| ToolError::bad_args("图内 y 映射溢出"))?;
        Ok((x, y))
    }

    pub fn crop(&self, rect: Rect) -> Result<Rect, ToolError> {
        require_rect_inside(rect, Rect { x: 0, y: 0, width: self.sent_width as i32, height: self.sent_height as i32 })?;
        let (x, y) = self.point(rect.x, rect.y)?;
        Ok(Rect { x, y, ..rect })
    }

    pub fn metadata(&self) -> Value {
        json!({
            "image_space": {"origin": {"x": 0, "y": 0}, "width": self.sent_width, "height": self.sent_height, "unit": "physical_pixel"},
            "desktop_space": {"unit": "physical_pixel", "source_rect": rect_json(self.source)},
            "image_to_desktop": {"scale_x": 1, "scale_y": 1, "offset_x": self.source.x, "offset_y": self.source.y,
                "formula": "desktop = image + offset", "dpi_conversion": false},
            "display_topology": self.topology.iter().copied().map(rect_json).collect::<Vec<_>>(),
            "contains_display_gaps": screen::monitor_coverage(self.source, &self.topology) < i64::from(self.source.width) * i64::from(self.source.height),
            "target_identity_verified": false
        })
    }
}

pub fn require_rect_inside(rect: Rect, bounds: Rect) -> Result<(), ToolError> {
    if rect.width <= 0 || rect.height <= 0 || !bounds.contains(rect.x, rect.y)
        || i64::from(rect.x) + i64::from(rect.width) > i64::from(bounds.x) + i64::from(bounds.width)
        || i64::from(rect.y) + i64::from(rect.height) > i64::from(bounds.y) + i64::from(bounds.height) {
        return Err(ToolError::bad_args("区域宽高必须为正，且整个区域必须在参考坐标范围内；width/height 不是 right/bottom"));
    }
    Ok(())
}

struct Entry { id: String, created: Instant, space: ImageSpace, pending_scope: Option<crate::Scope> }
#[derive(Default)]
struct Cache { entries: VecDeque<Entry>, sequence: u64, generation: u64 }
impl Cache {
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|e| !e.pending_scope.as_ref().is_some_and(crate::Scope::is_cancelled)
            && now.saturating_duration_since(e.created) < Duration::from_secs(TTL_SECS));
    }
    fn insert(&mut self, space: ImageSpace, now: Instant, scope: &crate::Scope) -> String {
        self.prune(now);
        self.entries.retain(|e| e.space.topology == space.topology);
        while self.entries.len() >= CAPACITY { self.entries.pop_front(); }
        self.sequence += 1;
        let id = format!("shot-{}-{}-{}", std::process::id(), screen::epoch_millis(), self.sequence);
        self.entries.push_back(Entry { id: id.clone(), created: now, space, pending_scope: Some(scope.clone()) });
        id
    }
    fn lookup(&mut self, id: &str, now: Instant) -> Result<ImageSpace, ToolError> {
        self.prune(now);
        self.entries.iter().find(|e| e.id == id).map(|e| e.space.clone()).ok_or_else(stale)
    }
    fn resolve(&mut self, id: &str, topology: Vec<Rect>, now: Instant) -> Result<ImageSpace, ToolError> {
        let topology = canonical_topology(topology);
        self.entries.retain(|e| e.space.topology == topology);
        self.lookup(id, now)
    }
}
fn stale() -> ToolError {
    ToolError::bad_args("screenshot_id 未知、过期、已被桌面动作失效或显示拓扑已改变；不回退猜坐标")
        .with_hint("重新获取必要区域的 screenshot；截图引用仅作坐标参考，优先 UIA 元素定位")
}
fn cache() -> &'static Mutex<Cache> { static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new(); CACHE.get_or_init(|| Mutex::new(Cache::default())) }

pub fn generation() -> u64 { cache().lock().unwrap().generation }
pub fn register(space: ImageSpace, scope: &crate::Scope, generation: u64) -> Result<String, ToolError> {
    let mut cache = cache().lock().unwrap();
    if scope.is_cancelled() { return Err(crate::cancelled_error()); }
    if cache.generation != generation { return Err(stale()); }
    Ok(cache.insert(space, Instant::now(), scope))
}
/// 仅在成功结果的图片已被接收后调用；取消/失效的引用不可复活。
/// 交付后的生命周期由 TTL、桌面动作及应用任务取消/会话重置控制。
pub fn confirm_delivery(id: &str) -> Result<(), ToolError> {
    let mut cache = cache().lock().unwrap();
    cache.prune(Instant::now());
    let entry = cache.entries.iter_mut().find(|e| e.id == id).ok_or_else(stale)?;
    entry.pending_scope = None;
    Ok(())
}
/// 拒收图片只撤销该条引用，不影响其他截图或正在注册的结果。
pub fn revoke(id: &str) { cache().lock().unwrap().entries.retain(|e| e.id != id); }
/// 未知引用在查询真实桌面之前就拒绝；有效引用仍需重新核验拓扑。
pub fn require_known(id: &str) -> Result<(), ToolError> { cache().lock().unwrap().lookup(id, Instant::now()).map(|_| ()) }
pub fn resolve(id: &str, topology: Vec<Rect>) -> Result<ImageSpace, ToolError> { cache().lock().unwrap().resolve(id, topology, Instant::now()) }
/// 桌面动作入口调用；观察、UIA 枚举和连续截图不应调用。
pub fn invalidate() {
    let mut cache = cache().lock().unwrap();
    cache.entries.clear();
    cache.generation = cache.generation.wrapping_add(1);
}

#[cfg(test)]
#[path = "screenshot_space_tests.rs"]
mod tests;
