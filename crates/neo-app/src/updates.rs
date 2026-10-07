//! 只检查公开 release 元数据；不下载更新、不访问应用数据或认证信息。

use crate::i18n::tr;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use semver::{Prerelease, Version};
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

#[allow(dead_code)] // used by the production network path; test builds stub it
const ENDPOINT: &str = "https://api.github.com/repos/ChidcGithub/Neo/releases?per_page=100";
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_RELEASES: usize = 100;
const MAX_TAG_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 512;
const NETWORK_ERROR: &str = "网络失败，请稍后重试";
const RESPONSE_ERROR: &str = "更新服务响应异常";
#[allow(dead_code)] // used by the production network path
const RATE_LIMIT_ERROR: &str = "更新服务限流，请稍后重试";
const SPAWN_ERROR: &str = "无法启动更新检查，请稍后重试";

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available {
        version: String,
        url: String,
    },
    Failed(String),
}

#[derive(Default)]
pub struct UpdateChecker {
    status: Status,
    generation: u64,
    receiver: Option<(u64, mpsc::Receiver<Status>)>,
    running: Arc<AtomicBool>,
    pending: bool,
}

impl UpdateChecker {
    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// 已有工作时忽略请求，包括取消后尚未结束的网络请求。节流由调用方负责。
    /// Production entry point; test builds use request_controlled.
    #[allow(dead_code)]
    pub fn request(&mut self, ctx: &egui::Context) {
        self.start(ctx, check, |job| {
            std::thread::Builder::new()
                .name("neo-update-check".into())
                .spawn(job)
                .map(|_| ())
        });
    }

    #[cfg(test)]
    pub(crate) fn request_controlled(
        &mut self,
        ctx: &egui::Context,
        result: Status,
    ) -> Box<dyn FnOnce() + Send> {
        let mut pending = None;
        self.start(
            ctx,
            move || result,
            |job| {
                pending = Some(job);
                Ok(())
            },
        );
        pending.expect("controlled worker must acquire single flight")
    }

    /// 非阻塞领取一次完成结果；无工作时不安排轮询或重绘。
    pub fn poll(&mut self) -> Option<Status> {
        if self.pending {
            self.pending = false;
            return Some(self.status.clone());
        }
        let (generation, receiver) = self.receiver.as_ref()?;
        let generation = *generation;
        let result = match receiver.try_recv() {
            Ok(status) => status,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Status::Failed(tr(RESPONSE_ERROR).into()),
        };
        self.receiver = None;
        if generation != self.generation {
            return None;
        }
        self.status = result;
        Some(self.status.clone())
    }

    /// 丢弃接收端并撤销旧代次，不在 UI 线程等待 blocking 请求结束。
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.receiver = None;
        self.pending = false;
        self.status = Status::Idle;
        // running 只由 worker 释放，避免反复取消/重试产生无界后台线程。
    }

    fn start<W, S>(&mut self, ctx: &egui::Context, worker: W, spawn: S)
    where
        W: FnOnce() -> Status + Send + 'static,
        S: FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
    {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some((self.generation, receiver));
        self.pending = false;
        self.status = Status::Checking;
        // The guard must exist before spawn so dropping an unstarted job also
        // releases single-flight ownership and wakes the UI.
        let completion = Completion {
            running: Arc::clone(&self.running),
            repaint: ctx.clone(),
            sender: Some(sender),
        };
        let job = Box::new(move || {
            let completion = completion;
            let result = worker();
            if let Some(sender) = completion.sender.as_ref() {
                let _ = sender.send(result);
            }
        });
        if spawn(job).is_err() {
            self.running.store(false, Ordering::Release);
            self.receiver = None;
            self.status = Status::Failed(tr(SPAWN_ERROR).into());
            self.pending = true;
            ctx.request_repaint();
        }
    }
}

struct Completion {
    running: Arc<AtomicBool>,
    repaint: egui::Context,
    sender: Option<mpsc::Sender<Status>>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        // A failed worker must be observable as Disconnected before the repaint.
        drop(self.sender.take());
        self.running.store(false, Ordering::Release);
        self.repaint.request_repaint();
    }
}

#[allow(dead_code)] // used by the production network path
fn check() -> Status {
    match fetch_release() {
        Ok(status) => status,
        Err(message) => Status::Failed(tr(message).into()),
    }
}

#[allow(dead_code)] // used by the production network path
fn fetch_release() -> Result<Status, &'static str> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent(concat!("Neo/", env!("CARGO_PKG_VERSION"), " update-check"))
        .build()
        .map_err(|_| NETWORK_ERROR)?;
    let response = client
        .get(ENDPOINT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .map_err(|_| NETWORK_ERROR)?;
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN
            && (response
                .headers()
                .get("x-ratelimit-remaining")
                .is_some_and(|v| v == "0")
                || response
                    .headers()
                    .contains_key(reqwest::header::RETRY_AFTER)))
    {
        return Err(RATE_LIMIT_ERROR);
    }
    if status != reqwest::StatusCode::OK {
        return Err(RESPONSE_ERROR);
    }
    let length = response.content_length();
    let body = read_body(response, length)?;
    select_release(&body, env!("CARGO_PKG_VERSION"))
}

fn read_body(reader: impl Read, length: Option<u64>) -> Result<Vec<u8>, &'static str> {
    if length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return Err(RESPONSE_ERROR);
    }
    let mut body = Vec::new();
    // 多读一个字节仅用来识别未声明长度/虚报长度的超限响应。
    reader
        .take((MAX_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|_| NETWORK_ERROR)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(RESPONSE_ERROR);
    }
    Ok(body)
}

#[derive(Deserialize)]
struct Release {
    #[serde(deserialize_with = "deserialize_tag")]
    tag_name: String,
    #[serde(deserialize_with = "deserialize_url")]
    html_url: String,
    draft: bool,
    prerelease: bool,
}

fn deserialize_tag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    deserialize_string(deserializer, MAX_TAG_BYTES)
}

fn deserialize_url<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    deserialize_string(deserializer, MAX_URL_BYTES)
}

fn deserialize_string<'de, D: Deserializer<'de>>(
    deserializer: D,
    limit: usize,
) -> Result<String, D::Error> {
    struct BoundedString(usize);
    impl Visitor<'_> for BoundedString {
        type Value = String;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a bounded release field")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > self.0 {
                return Err(E::custom("release field too long"));
            }
            Ok(value.to_owned())
        }
    }
    deserializer.deserialize_str(BoundedString(limit))
}

struct Releases(Vec<Release>);

impl<'de> Deserialize<'de> for Releases {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ReleaseList;
        impl<'de> Visitor<'de> for ReleaseList {
            type Value = Releases;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a bounded release list")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Releases, A::Error> {
                let mut releases = Vec::new();
                while let Some(release) = sequence.next_element::<Release>()? {
                    if releases.len() == MAX_RELEASES {
                        return Err(de::Error::custom("too many releases"));
                    }
                    releases.push(release);
                }
                Ok(Releases(releases))
            }
        }
        deserializer.deserialize_seq(ReleaseList)
    }
}

/// Neo 在 0.1.0 发布前从 rc + 九位数字的日期式构建号改为 preN（3dcfcde7）。
/// 这批九位数字 rc 是早期构建，不是比 preN 更新的候选版；仅保留该历史格式的兼容。
/// 新的候选版应使用标准 rc.N，其他版本线不继承这段历史。
fn is_legacy_rc(version: &Version) -> bool {
    (version.major, version.minor, version.patch) == (0, 1, 0)
        && version
            .pre
            .as_str()
            .strip_prefix("rc")
            .is_some_and(|n| n.len() == 9 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn update_prerelease(version: &Version) -> Prerelease {
    let pre = version.pre.as_str();
    let prefix = if is_legacy_rc(version) { "rc" } else { "pre" };
    if let Some(n) = pre.strip_prefix(prefix) {
        if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
            // pre9/pre10 是 SemVer 字符串，更新策略则把项目序号解释为 pre.9/pre.10。
            // 不转为机器整数，避免合法的长序号溢出；前导零不构成新版本。
            let n = n.trim_start_matches('0');
            let n = if n.is_empty() { "0" } else { n };
            return Prerelease::new(&format!("{prefix}.{n}"))
                .expect("ASCII prefix and decimal identifier are valid SemVer");
        }
    }
    version.pre.clone()
}

fn cmp_update_versions(left: &Version, right: &Version) -> std::cmp::Ordering {
    let stage = |v: &Version| {
        if v.pre.is_empty() {
            2
        } else if is_legacy_rc(v) {
            0
        } else {
            1
        }
    };
    (left.major, left.minor, left.patch)
        .cmp(&(right.major, right.minor, right.patch))
        .then_with(|| stage(left).cmp(&stage(right)))
        .then_with(|| update_prerelease(left).cmp(&update_prerelease(right)))
}

fn select_release(body: &[u8], current: &str) -> Result<Status, &'static str> {
    if body.len() > MAX_BODY_BYTES {
        return Err(RESPONSE_ERROR);
    }
    let current = Version::parse(current).map_err(|_| RESPONSE_ERROR)?;
    let releases: Releases = serde_json::from_slice(body).map_err(|_| RESPONSE_ERROR)?;
    let mut newest: Option<(Version, String)> = None;
    for release in releases.0 {
        if release.draft {
            continue;
        }
        let tag = &release.tag_name;
        let Ok(version) = Version::parse(tag.strip_prefix('v').unwrap_or(tag)) else {
            continue;
        };
        if current.pre.is_empty() && (release.prerelease || !version.pre.is_empty()) {
            continue;
        }
        // semver 标签的字符集无需 URL 转义；精确匹配同时拒绝外链、凭据、查询串等。
        if release.html_url != format!("https://github.com/ChidcGithub/Neo/releases/tag/{tag}") {
            continue;
        }
        // 使用同一项目排序比较当前版本和候选版本；build metadata 不构成更新。
        if !cmp_update_versions(&version, &current).is_gt() {
            continue;
        }
        if newest
            .as_ref()
            .is_none_or(|(best, _)| cmp_update_versions(&version, best).is_gt())
        {
            newest = Some((version, release.html_url));
        }
    }
    Ok(match newest {
        Some((version, url)) => Status::Available {
            version: version.to_string(),
            url,
        },
        None => Status::UpToDate,
    })
}

#[cfg(test)]
#[path = "updates_tests.rs"]
mod tests;
