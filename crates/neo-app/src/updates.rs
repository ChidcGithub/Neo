//! 只检查公开 release 元数据；不下载更新、不访问应用数据或认证信息。

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use semver::Version;
use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

const ENDPOINT: &str = "https://api.github.com/repos/ChidcGithub/Neo/releases?per_page=100";
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_RELEASES: usize = 100;
const MAX_TAG_BYTES: usize = 128;
const MAX_URL_BYTES: usize = 512;
const NETWORK_ERROR: &str = "网络失败，请稍后重试";
const RESPONSE_ERROR: &str = "更新服务响应异常";
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
    pub fn request(&mut self, ctx: &egui::Context) {
        self.start(ctx, check, |job| {
            std::thread::Builder::new()
                .name("neo-update-check".into())
                .spawn(job)
                .map(|_| ())
        });
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
            Err(mpsc::TryRecvError::Disconnected) => Status::Failed(RESPONSE_ERROR.into()),
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
        let running = Arc::clone(&self.running);
        let repaint = ctx.clone();
        let job = Box::new(move || {
            let _completion = Completion { running, repaint };
            let result = worker();
            let _ = sender.send(result);
        });
        if spawn(job).is_err() {
            self.running.store(false, Ordering::Release);
            self.receiver = None;
            self.status = Status::Failed(SPAWN_ERROR.into());
            self.pending = true;
            ctx.request_repaint();
        }
    }
}

struct Completion {
    running: Arc<AtomicBool>,
    repaint: egui::Context,
}

impl Drop for Completion {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.repaint.request_repaint();
    }
}

fn check() -> Status {
    match fetch_release() {
        Ok(status) => status,
        Err(message) => Status::Failed(message.into()),
    }
}

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
        // build metadata 不影响 SemVer 优先级，不能把仅构建号变化视为更新。
        if !version.cmp_precedence(&current).is_gt() {
            continue;
        }
        if newest
            .as_ref()
            .is_none_or(|(best, _)| version.cmp_precedence(best).is_gt())
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
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn release(tag: &str, prerelease: bool) -> Value {
        json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/ChidcGithub/Neo/releases/tag/{tag}"),
            "draft": false,
            "prerelease": prerelease,
        })
    }

    fn select(releases: Vec<Value>, current: &str) -> Status {
        select_release(&serde_json::to_vec(&releases).unwrap(), current).unwrap()
    }

    fn available(tag: &str) -> Status {
        Status::Available {
            version: tag.strip_prefix('v').unwrap_or(tag).into(),
            url: format!("https://github.com/ChidcGithub/Neo/releases/tag/{tag}"),
        }
    }

    #[test]
    fn semver_orders_numeric_components_and_prerelease_identifiers() {
        assert_eq!(
            select(
                vec![release("v0.9.0", false), release("0.10.0", false)],
                "0.8.0"
            ),
            available("0.10.0")
        );
        assert_eq!(
            select(
                vec![release("v1.0.0-rc.10", true), release("v1.0.0-rc.2", true)],
                "1.0.0-rc.1",
            ),
            available("v1.0.0-rc.10")
        );
        // pre2/pre10 是字母数字标识符，必须遵从 SemVer 的字典序而不是自然数排序。
        assert_eq!(
            select(vec![release("v0.1.0-pre10", true)], "0.1.0-pre2"),
            Status::UpToDate
        );
        assert_eq!(
            select(vec![release("v1.0.0+new", false)], "1.0.0+old"),
            Status::UpToDate
        );
    }

    #[test]
    fn prerelease_channel_accepts_stable_and_prerelease() {
        assert_eq!(
            select(vec![release("v0.1.0-pre3", true)], "0.1.0-pre2"),
            available("v0.1.0-pre3")
        );
        assert_eq!(
            select(
                vec![release("v0.1.0", false), release("v0.1.0-pre3", true)],
                "0.1.0-pre2"
            ),
            available("v0.1.0")
        );
    }

    #[test]
    fn stable_channel_rejects_both_kinds_of_prerelease_marker() {
        assert_eq!(
            select(
                vec![
                    release("v2.0.0-rc.1", false),
                    release("v3.0.0", true),
                    release("v1.1.0", false)
                ],
                "1.0.0",
            ),
            available("v1.1.0")
        );
    }

    #[test]
    fn drafts_removed_releases_invalid_tags_and_older_versions_are_ignored() {
        let mut draft = release("v9.0.0", false);
        draft["draft"] = json!(true);
        assert_eq!(
            select(
                vec![
                    draft,
                    release("release-8.0.0", false),
                    release("v01.0.0", false),
                    release("v0.9.0", false)
                ],
                "1.0.0"
            ),
            Status::UpToDate
        );
        assert_eq!(select(Vec::new(), "0.1.0-pre2"), Status::UpToDate);
    }

    #[test]
    fn only_matching_repository_release_links_are_allowed() {
        for url in [
            "javascript:alert(1)",
            "http://github.com/ChidcGithub/Neo/releases/tag/v2.0.0",
            "https://evil.example/ChidcGithub/Neo/releases/tag/v2.0.0",
            "https://github.com/Other/Neo/releases/tag/v2.0.0",
            "https://github.com/ChidcGithub/Other/releases/tag/v2.0.0",
            "https://github.com@evil.example/ChidcGithub/Neo/releases/tag/v2.0.0",
            "https://github.com/ChidcGithub/Neo/releases/tag/v1.0.0",
            "https://github.com/ChidcGithub/Neo/releases/tag/v2.0.0?next=https://evil.example",
            "https://github.com/ChidcGithub/Neo/releases/tag/v2.0.0#fragment",
        ] {
            let mut item = release("v2.0.0", false);
            item["html_url"] = json!(url);
            assert_eq!(select(vec![item], "1.0.0"), Status::UpToDate, "{url}");
        }
    }

    #[test]
    fn giant_bodies_are_rejected_with_or_without_content_length() {
        let body = vec![b' '; MAX_BODY_BYTES + 10];
        assert_eq!(
            read_body(body.as_slice(), Some(body.len() as u64)),
            Err(RESPONSE_ERROR)
        );
        assert_eq!(read_body(body.as_slice(), None), Err(RESPONSE_ERROR));
        assert_eq!(read_body(body.as_slice(), Some(1)), Err(RESPONSE_ERROR));
        assert_eq!(select_release(&body, "1.0.0"), Err(RESPONSE_ERROR));
        let exact = vec![b' '; MAX_BODY_BYTES];
        assert_eq!(
            read_body(exact.as_slice(), None).unwrap().len(),
            MAX_BODY_BYTES
        );
    }

    #[test]
    fn malformed_and_unbounded_metadata_only_produce_safe_errors() {
        for body in [
            b"not json secret".as_slice(),
            b"{}",
            b"[{}]",
            b"null",
            b"[] trailing",
        ] {
            assert_eq!(select_release(body, "1.0.0"), Err(RESPONSE_ERROR));
        }
        let many = serde_json::to_vec(&vec![release("v2.0.0", false); MAX_RELEASES + 1]).unwrap();
        assert_eq!(select_release(&many, "1.0.0"), Err(RESPONSE_ERROR));
        for (field, limit) in [("tag_name", MAX_TAG_BYTES), ("html_url", MAX_URL_BYTES)] {
            let mut item = release("v2.0.0", false);
            item[field] = json!("x".repeat(limit + 1));
            assert_eq!(
                select_release(&serde_json::to_vec(&vec![item]).unwrap(), "1.0.0"),
                Err(RESPONSE_ERROR)
            );
        }
        assert_eq!(select_release(b"[]", "invalid"), Err(RESPONSE_ERROR));
    }

    #[test]
    fn duplicate_requests_are_single_flight_and_completion_is_delivered_once() {
        let ctx = egui::Context::default();
        let mut checker = UpdateChecker::default();
        assert_eq!(checker.status(), &Status::Idle);
        assert_eq!(checker.poll(), None);
        let mut job = None;
        checker.start(
            &ctx,
            || Status::UpToDate,
            |worker| {
                job = Some(worker);
                Ok(())
            },
        );
        assert_eq!(checker.status(), &Status::Checking);
        checker.start(&ctx, || unreachable!(), |_| panic!("duplicate spawn"));
        assert_eq!(checker.poll(), None);
        job.unwrap()();
        assert_eq!(checker.poll(), Some(Status::UpToDate));
        assert_eq!(checker.poll(), None);
    }

    #[test]
    fn cancel_discards_old_results_but_holds_single_flight_until_worker_finishes() {
        let ctx = egui::Context::default();
        let mut checker = UpdateChecker::default();
        let mut old_job = None;
        checker.start(
            &ctx,
            || available("v2.0.0"),
            |job| {
                old_job = Some(job);
                Ok(())
            },
        );
        checker.cancel();
        assert_eq!(checker.status(), &Status::Idle);
        assert!(checker.is_running());
        for _ in 0..10 {
            checker.start(
                &ctx,
                || unreachable!(),
                |_| panic!("cancel bypassed single flight"),
            );
            checker.cancel();
        }
        old_job.unwrap()();
        assert!(!checker.is_running());
        assert_eq!(checker.poll(), None);
        assert_eq!(checker.status(), &Status::Idle);
        checker.start(
            &ctx,
            || Status::UpToDate,
            |job| {
                job();
                Ok(())
            },
        );
        assert_eq!(checker.poll(), Some(Status::UpToDate));
    }

    #[test]
    fn cancel_also_discards_already_queued_completion() {
        let ctx = egui::Context::default();
        let mut checker = UpdateChecker::default();
        checker.start(
            &ctx,
            || available("v2.0.0"),
            |job| {
                job();
                Ok(())
            },
        );
        checker.cancel();
        assert_eq!(checker.poll(), None);
        assert_eq!(checker.status(), &Status::Idle);
    }

    #[test]
    fn spawn_failure_is_visible_once_and_can_be_retried() {
        let ctx = egui::Context::default();
        let mut checker = UpdateChecker::default();
        checker.start(
            &ctx,
            || unreachable!(),
            |_| Err(io::Error::other("private OS details")),
        );
        assert_eq!(checker.status(), &Status::Failed(SPAWN_ERROR.into()));
        assert_eq!(checker.poll(), Some(Status::Failed(SPAWN_ERROR.into())));
        assert_eq!(checker.poll(), None);
        checker.start(
            &ctx,
            || Status::UpToDate,
            |job| {
                job();
                Ok(())
            },
        );
        assert_eq!(checker.poll(), Some(Status::UpToDate));
    }

    #[test]
    fn drop_does_not_wait_for_pending_worker() {
        let ctx = egui::Context::default();
        let mut checker = UpdateChecker::default();
        let mut pending = None;
        checker.start(
            &ctx,
            || Status::UpToDate,
            |job| {
                pending = Some(job);
                Ok(())
            },
        );
        drop(checker);
        pending.unwrap()();
    }
}
