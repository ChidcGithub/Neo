
    use super::*;

    const SAMPLE: &str = r#"
<html><body><ol id="b_results">
<li class="b_algo" data-id="1">
  <div class="b_tpcn"><span class="wr"></span></div>
  <h2 class=""><a href="https://example.com/li-bai" target="_blank" h="ID=SERP,1">李白 - 百度百科</a></h2>
  <div class="b_caption"><p class="b_lineclamp_3">李白（701年—762年），字太白，号青莲居士，唐代伟大的浪漫主义诗人……</p></div>
</li>
<li class="b_algo" data-id="2">
  <h2><a href="https://zh.wikisource.org/wiki/李白" h="ID=SERP,2">李白<em>诗集</em> &amp; 年谱</a></h2>
  <p>收录诗作九百余首 &#8212; 维基文库</p>
</li>
<li class="b_ans">非自然结果块（答案卡），没有 h2/a，要被跳过</li>
</ol></body></html>"#;

    #[test]
    fn parses_algo_blocks() {
        let hits = parse_results(SAMPLE, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "李白 - 百度百科");
        assert_eq!(hits[0].url, "https://example.com/li-bai");
        assert!(hits[0].snippet.starts_with("李白（701年"));
        // <em> 高亮的文字保留、命名与数字实体被解码
        assert_eq!(hits[1].title, "李白诗集 & 年谱");
        assert!(hits[1].snippet.contains('—'));
    }

    #[test]
    fn empty_when_structure_changed() {
        assert!(parse_results("<html><body>consent wall</body></html>", 5).is_empty());
    }

    #[test]
    fn respects_count() {
        assert_eq!(parse_results(SAMPLE, 1).len(), 1);
    }

    #[test]
    fn encodes_query() {
        assert_eq!(url_encode("牛顿 第二定律&公式"), "%E7%89%9B%E9%A1%BF%20%E7%AC%AC%E4%BA%8C%E5%AE%9A%E5%BE%8B%26%E5%85%AC%E5%BC%8F");
        assert!(page_url("a b", 3).contains("q=a%20b"));
    }

    #[test]
    fn rejects_blank_query() {
        let tool = crate::find("web_search").unwrap();
        let value = serde_json::json!({ "query": "  " });
        let args = Args::new(tool, &value);
        let scope = crate::Scope::new(std::env::temp_dir());
        let out = run(&scope, &args);
        assert!(!out.is_ok());
        assert_eq!(out.error.unwrap().kind, ErrorKind::BadArguments);
    }

    /// 真实抓取 smoke test：解析器对着 bing.com 当前的真实页面验一次。
    /// 默认不跑（依赖外网），手动：`cargo test -p neo-tools -- --ignored --nocapture`
    #[test]
    #[ignore = "真连 bing.com"]
    fn real_bing_page_parses() {
        let hits = fetch_and_parse(&page_url("牛顿第二定律", 5), 5).expect("抓取失败");
        for h in &hits {
            eprintln!("- {}\n  {}\n  {}…", h.title, h.url, h.snippet.chars().take(40).collect::<String>());
            assert!(h.url.starts_with("http"), "链接不像 URL：{}", h.url);
        }
        assert!(!hits.is_empty(), "一条都没解析出来：b_algo 结构可能变了");
    }
