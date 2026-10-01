//! `web_search` —— 用 Bing 搜索互联网（抓结果页，不需要任何 API key/token）。
//!
//! 两种形态：
//!
//! - **默认**：HTTPS GET 抓 Bing 结果页，解析出标题 / 链接 / 摘要回给模型 ——
//!   模型拿到事实接着回答（「今天的新闻」「这个概念的最新进展」）。
//! - **`open_browser: true`**：在用户默认浏览器里打开搜索页 —— 结果是
//!   「给用户看」的，比如用户说「帮我搜一下…」想自己点着看。
//!
//! 边界：只发 GET、不登录、不带 cookie；查询词会发给 Bing —— 课堂场景里
//! 这和老师自己开浏览器搜一下等价，风险档定 [`crate::Risk::Read`]（默认放行）。
//! 本工具是 neo-tools「无网络」宣言的**唯一例外**，网络面就集中在这一处：
//! 超时有上限、不跟随页面执行脚本、不把结果 HTML 原样回灌给模型。
//!
//! 名字破例不是「动词在前」：`web_search` 是各家模型在训练语料里
//! 见得最多的联网工具名，语感最强（`screen_element_search` 已是先例）。

use serde_json::json;

use crate::result::{Args, ErrorKind, Outcome, ToolError};
use crate::spec::Param;
use crate::Scope;

/// 浏览器 UA：不带它 Bing 容易给简版页或触发反爬。
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// 抓取与连接的总超时。课堂网络慢，但也不能让工具线程吊死。
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// 单条摘要回灌上限（字符）。Bing 摘要一般 100 字上下，截掉长尾。
const SNIPPET_CHARS: usize = 300;

pub static PARAMS: &[Param] = &[
    Param::text("query", "搜索词。自然语言问题或关键词都行；越具体结果越准。"),
    Param::opt_int("count", "返回的结果条数。", 5, 1, 10),
    Param::flag(
        "open_browser",
        "true 表示同时在用户的默认浏览器里打开搜索页（结果给用户亲眼看的场景）。默认 false。",
    ),
];

pub fn preview(args: &Args) -> String {
    let q = args.opt_str("query").unwrap_or_default();
    if args.flag("open_browser").unwrap_or(false) {
        format!("搜索「{q}」并在浏览器里打开结果页")
    } else {
        format!("搜索「{q}」")
    }
}

pub fn run(scope: &Scope, args: &Args) -> Outcome {
    // 不碰工作区：搜索是唯一的「出网不出围栏」工具。
    let _ = scope;
    match search(args) {
        Ok(o) => o,
        Err(e) => Outcome::fail("web_search", e),
    }
}

fn search(args: &Args) -> Result<Outcome, ToolError> {
    let query = args.require_str("query")?;
    let count = args.opt_int("count")?.clamp(1, 10) as usize;
    let open_browser = args.flag("open_browser")?;
    let page_url = page_url(&query, count);

    // 「给用户看」的一侧：先开浏览器（失败整个算失败 —— 用户要的就是那扇窗）。
    if open_browser {
        open_in_browser(&page_url)?;
    }

    // 「给模型读」的一侧：抓取 + 解析。浏览器已经打开时抓取失败不算白跑 ——
    // 用户已经能看到结果页，模型拿到 note 如实转述即可。
    let hits = match fetch_and_parse(&page_url, count) {
        Ok(h) if h.is_empty() => {
            if open_browser {
                Vec::new()
            } else {
                return Err(ToolError::new(
                    ErrorKind::Unsupported,
                    "结果页里一条结果也没解析出来 —— 页面结构可能变了，或触发了反爬",
                )
                .with_hint("加 open_browser=true 让用户在浏览器里直接看；或换个搜索词再试"));
            }
        }
        Ok(h) => h,
        Err(e) => {
            if open_browser {
                Vec::new()
            } else {
                return Err(e);
            }
        }
    };

    let results: Vec<_> = hits
        .iter()
        .map(|h| json!({ "title": h.title, "url": h.url, "snippet": h.snippet }))
        .collect();
    let summary = if open_browser {
        format!("已在浏览器打开搜索「{query}」（另解析到 {} 条结果）", hits.len())
    } else {
        format!("搜索「{query}」：{} 条结果", hits.len())
    };
    let mut data = json!({
        "query": query,
        "results": results,
        "opened_in_browser": open_browser,
    });
    if hits.is_empty() {
        data["note"] = json!("浏览器已打开结果页，但抓取解析失败；用户能看到网页，请如实转述");
    }
    Ok(Outcome::ok("web_search", summary, data))
}

/// Bing 结果页 URL（统一走 https + 中文界面）。
fn page_url(query: &str, count: usize) -> String {
    format!(
        "https://www.bing.com/search?q={}&setlang=zh-CN&count={}",
        url_encode(query),
        count
    )
}

fn fetch_and_parse(url: &str, count: usize) -> Result<Vec<Hit>, ToolError> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(UA)
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| ToolError::new(ErrorKind::Internal, format!("无法创建 HTTP 客户端：{e}")))?;
    let resp = client
        .get(url)
        .header(reqwest::header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
        .send()
        .map_err(|e| {
            if e.is_timeout() {
                ToolError::new(ErrorKind::Timeout, "连接 bing.com 超时")
                    .with_hint("课堂网络可能限制了外网；检查网络后重试")
            } else {
                ToolError::io(format!("无法连接 bing.com：{e}"))
                    .with_hint("检查网络；或加 open_browser=true 让用户自己在浏览器里搜")
            }
        })?;
    if !resp.status().is_success() {
        return Err(ToolError::io(format!("bing.com 返回 {}", resp.status()))
            .with_hint("被反爬挡住的概率大：换个搜索词，或加 open_browser=true"));
    }
    let html = resp
        .text()
        .map_err(|e| ToolError::io(format!("读取结果页失败：{e}")))?;
    Ok(parse_results(&html, count))
}

/// 在用户默认浏览器里打开一个 URL（只拉起、不等待）。
///
/// Windows 走 `ShellExecuteW` 而不是 `cmd /C start`：URL 里的 `&` 会被 cmd
/// 当命令分隔符（搜索页地址被截断、余下参数被当命令执行），而且 GUI 进程
/// spawn 控制台子进程会闪一个黑窗。ShellExecute 是「用默认程序打开」的
/// 系统原生入口，没有命令行解析这一层。
#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<(), ToolError> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let url_w: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // lpOperation 传 null = 默认动词（等价 "open"）。
    // 返回值 ≤ 32 是失败（错误码），> 32 才是有效的实例句柄。
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            url_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (r as usize) <= 32 {
        return Err(ToolError::io(format!("无法调起默认浏览器（错误码 {}）", r as usize))
            .with_hint("也可以只抓取（open_browser=false），把结果读给用户听"));
    }
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_in_browser(url: &str) -> Result<(), ToolError> {
    #[cfg(target_os = "macos")]
    let (program, argv): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(not(target_os = "macos"))]
    let (program, argv): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    std::process::Command::new(program)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            ToolError::io(format!("无法调起默认浏览器（{program}）：{e}"))
                .with_hint("也可以只抓取（open_browser=false），把结果读给用户听")
        })?;
    Ok(())
}

/// 一条搜索结果。
struct Hit {
    title: String,
    url: String,
    snippet: String,
}

/// 从 Bing 结果页 HTML 里挑出自然结果（`<li class="b_algo">` 块）。
///
/// 手写扫描而不是引 HTML 解析器：结果页结构十年没变过几次，真变了
/// 就返回空（上层报 Unsupported），不会因为依赖升级悄悄改变行为。
fn parse_results(html: &str, count: usize) -> Vec<Hit> {
    const MARK: &str = "<li class=\"b_algo\"";
    let mut hits = Vec::new();
    let mut rest = html;
    while hits.len() < count {
        let Some(start) = rest.find(MARK) else { break };
        let after = &rest[start + MARK.len()..];
        let end = after.find(MARK).unwrap_or(after.len());
        if let Some(hit) = parse_block(&after[..end]) {
            hits.push(hit);
        }
        rest = &after[end..];
    }
    hits
}

/// 一个 `b_algo` 块：`<h2><a href>标题</a></h2>` + 摘要 `<p>`。
fn parse_block(block: &str) -> Option<Hit> {
    let h2 = block.find("<h2")?;
    let a = block[h2..].find("<a ")? + h2;
    let href = block[a..].find("href=\"")? + a + 6;
    let url_end = block[href..].find('"')? + href;
    let url = &block[href..url_end];

    let a_open_end = block[a..].find('>')? + a;
    let title_end = block[a_open_end..].find("</a>")? + a_open_end;
    let title = decode_entities(&strip_tags(&block[a_open_end + 1..title_end]));

    let snippet = block
        .match_indices("<p")
        // 只要 `<p ` 或 `<p>`，别把 `<pre>`/`<path>` 当摘要。
        .find(|(i, _)| matches!(block.as_bytes().get(i + 2), Some(b' ' | b'>')))
        .and_then(|(i, _)| tag_text(&block[i..], "<p", "</p>"))
        .map(|s| decode_entities(&strip_tags(&s)))
        .map(|s| s.chars().take(SNIPPET_CHARS).collect::<String>())
        .unwrap_or_default();

    let title = title.trim().to_owned();
    if url.is_empty() || title.is_empty() {
        return None;
    }
    Some(Hit {
        title,
        // href 里的 &amp; 等实体也要解码 —— 不然 Bing 跳转链接原样进数据，
        // 照抄就是坏链。
        url: decode_entities(url),
        snippet: snippet.trim().to_owned(),
    })
}

/// 第一对 `open`/`close` 标签之间的内容。
fn tag_text(s: &str, open: &str, close: &str) -> Option<String> {
    let start = s.find(open)?;
    let content_start = s[start..].find('>')? + start + 1;
    let end = s[content_start..].find(close)? + content_start;
    Some(s[content_start..end].to_owned())
}

/// 丢标签、留文本（`<em>` 这类高亮标签的内容保留）。
///
/// 容错：正文里未转义的裸 `<`/`>` 是合法的（数学摘要「a > b」）——
/// `<` 后接的字符不像标签起始（字母 / `/` / `!` / `?`）就当文本留下；
/// 不在标签里的 `>` 同理。不然正文字符会被静默吃掉。
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut it = s.chars().peekable();
    while let Some(ch) = it.next() {
        match ch {
            '<' if !in_tag => {
                let starts_tag = it
                    .peek()
                    .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?'));
                if starts_tag {
                    in_tag = true;
                } else {
                    out.push('<');
                }
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 解码常见 HTML 实体（命名 + 十/十六进制数字），不认识的按原样保留。
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        match after.find(';') {
            Some(semi) if semi <= 10 => {
                let entity = &after[..semi];
                let decoded = match entity {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    // 中文结果页里高频的空白与标点实体。
                    "nbsp" | "ensp" | "emsp" | "thinsp" => Some(' '),
                    "ndash" => Some('–'),
                    "mdash" => Some('—'),
                    "middot" => Some('·'),
                    "hellip" => Some('…'),
                    _ => entity
                        .strip_prefix("#x")
                        .or_else(|| entity.strip_prefix("#X"))
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .or_else(|| {
                            entity
                                .strip_prefix('#')
                                .and_then(|d| d.parse::<u32>().ok())
                        })
                        .and_then(char::from_u32),
                };
                match decoded {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push('&');
                        out.push_str(entity);
                        out.push(';');
                    }
                }
                rest = &after[semi + 1..];
            }
            _ => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// URL 百分号编码（query 进 `?q=`）：保留 unreserved，其余按 UTF-8 字节转义。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "web_search_tests.rs"]
mod tests;
