use std::collections::HashSet;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use scraper::{Html, Selector};
use tracing::{info, warn};
use url::Url;

use crate::config::Config;

const DUCKDUCKGO_API: &str = "https://api.duckduckgo.com/";
const BING_SEARCH: &str = "https://www.bing.com/search";
const WEB_USER_AGENT: &str = "KomiDiscordBot/0.1 (+local Discord assistant)";

#[derive(Clone)]
pub struct WebSearch {
    enabled: bool,
    always: bool,
    max_results: usize,
    fetch_links: bool,
    fetch_max_urls: usize,
    fetch_chars: usize,
    fetch_body_bytes: usize,
    crawl_links: bool,
    crawl_max_pages: usize,
    crawl_chars: usize,
    http: reqwest::Client,
}

impl WebSearch {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(config.llama_timeout)
            .user_agent(WEB_USER_AGENT)
            .build()
            .context("failed to build web search HTTP client")?;

        Ok(Self {
            enabled: config.web_search_enabled,
            always: config.web_search_always,
            max_results: config.web_search_results,
            fetch_links: config.web_fetch_links,
            fetch_max_urls: config.web_fetch_max_urls,
            fetch_chars: config.web_fetch_chars,
            fetch_body_bytes: config.web_fetch_body_bytes,
            crawl_links: config.web_crawl_links,
            crawl_max_pages: config.web_crawl_max_pages,
            crawl_chars: config.web_crawl_chars,
            http,
        })
    }

    pub fn has_links(&self, prompt: &str) -> bool {
        !extract_urls(prompt).is_empty()
    }

    pub async fn context_for(&self, prompt: &str) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }

        let urls = extract_urls(prompt);
        if !self.should_search(prompt) && urls.is_empty() {
            return Ok(None);
        }

        let mut sections = Vec::new();

        if urls.is_empty() && self.should_search(prompt) {
            match self.search_context(prompt).await {
                Ok(Some(context)) => sections.push(context),
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "web search failed; continuing without search context");
                }
            }
        }

        if self.fetch_links {
            match self.link_context(&urls).await {
                Ok(linked_context) if !linked_context.is_empty() => sections.push(linked_context),
                Ok(_) => {}
                Err(err) => {
                    warn!(error = %err, "linked page fetch failed; continuing without link context");
                }
            }
        }

        if sections.is_empty() {
            Ok(None)
        } else {
            Ok(Some(sections.join("\n\n")))
        }
    }

    fn should_search(&self, prompt: &str) -> bool {
        self.always || prompt_requests_web(prompt)
    }

    async fn search_context(&self, prompt: &str) -> Result<Option<String>> {
        match self.bing_search(prompt).await {
            Ok(results) if !results.is_empty() => {
                info!(result_count = results.len(), "web search completed");
                return Ok(Some(format_web_context(prompt, &results)));
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "Bing web search failed; trying instant answer API");
            }
        }

        let results = self.instant_answer_search(prompt).await?;
        if results.is_empty() {
            Ok(None)
        } else {
            info!(
                result_count = results.len(),
                "instant answer search completed"
            );
            Ok(Some(format_web_context(prompt, &results)))
        }
    }

    async fn bing_search(&self, prompt: &str) -> Result<Vec<SearchResult>> {
        let response = self
            .http
            .get(BING_SEARCH)
            .query(&[("q", prompt)])
            .send()
            .await
            .context("failed to call Bing web search")?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Bing web search returned {status}"));
        }

        let body = response
            .text()
            .await
            .context("failed to read Bing web search response")?;
        Ok(parse_bing_search_results(&body, self.max_results))
    }

    async fn instant_answer_search(&self, prompt: &str) -> Result<Vec<SearchResult>> {
        let url = format!(
            "{DUCKDUCKGO_API}?q={}&format=json&no_html=1&skip_disambig=1",
            percent_encode(prompt)
        );
        let response = self
            .http
            .get(url)
            .send()
            .await
            .context("failed to call web search endpoint")?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("DuckDuckGo instant answer returned {status}"));
        }

        let body = response
            .text()
            .await
            .context("failed to read DuckDuckGo instant answer response")?;
        let search = serde_json::from_str::<DuckDuckGoResponse>(&body)
            .context("invalid DuckDuckGo instant answer JSON")?;
        Ok(search_results(search, self.max_results))
    }

    async fn link_context(&self, urls: &[String]) -> Result<String> {
        let mut context = String::new();
        let mut seen = HashSet::<String>::new();

        for url in urls.iter().take(self.fetch_max_urls) {
            let page = match self.fetch_page(url).await {
                Ok(page) => page,
                Err(err) => {
                    warn!(url, error = %err, "failed to fetch linked page");
                    continue;
                }
            };

            if page.text.is_empty() {
                continue;
            }

            seen.insert(page.url.clone());

            if context.is_empty() {
                context.push_str("Linked page excerpts:\n");
            }

            context.push_str(&format!(
                "- URL: {url}\n  Excerpt: {}\n",
                trim_chars(&page.text, self.fetch_chars)
            ));

            if self.crawl_links {
                let mut detail_urls = Vec::new();
                for link in page
                    .links
                    .iter()
                    .filter(|link| same_origin(&page.url, link))
                {
                    if detail_urls.len() >= self.crawl_max_pages {
                        break;
                    }

                    if seen.insert(link.clone()) {
                        detail_urls.push(link.clone());
                    }
                }

                for detail_url in detail_urls {
                    let detail_page = match self.fetch_page(&detail_url).await {
                        Ok(page) => page,
                        Err(err) => {
                            warn!(url = %detail_url, error = %err, "failed to fetch detail page");
                            continue;
                        }
                    };

                    if detail_page.text.is_empty() {
                        continue;
                    }

                    seen.insert(detail_page.url.clone());
                    context.push_str(&format!(
                        "- Detail URL: {}\n  Excerpt: {}\n",
                        detail_page.url,
                        trim_chars(&detail_page.text, self.crawl_chars)
                    ));
                }
            }
        }

        Ok(context.trim().to_string())
    }

    async fn fetch_page(&self, url: &str) -> Result<FetchedPage> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("failed to fetch linked page: {url}"))?;

        if !response.status().is_success() {
            return Ok(FetchedPage::empty(url));
        }

        let final_url = response.url().to_string();
        if response
            .content_length()
            .is_some_and(|length| length > self.fetch_body_bytes as u64)
        {
            warn!(
                url,
                "linked page exceeds configured body limit; reading bounded prefix"
            );
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !content_type.is_empty()
            && !content_type.starts_with("text/")
            && !content_type.contains("html")
            && !content_type.contains("json")
            && !content_type.contains("xml")
        {
            warn!(url, content_type, "skipping non-text linked content");
            return Ok(FetchedPage::empty(&final_url));
        }
        let body = response
            .bytes()
            .await
            .with_context(|| format!("failed to read linked page body: {url}"))?;
        let body = &body[..body.len().min(self.fetch_body_bytes)];
        let body = String::from_utf8_lossy(body);
        let links = if self.crawl_links {
            extract_links(&final_url, &body)
        } else {
            Vec::new()
        };
        let text = html_to_text(&body);

        Ok(FetchedPage {
            url: final_url,
            text,
            links,
        })
    }
}

struct FetchedPage {
    url: String,
    text: String,
    links: Vec<String>,
}

impl FetchedPage {
    fn empty(url: &str) -> Self {
        Self {
            url: url.to_string(),
            text: String::new(),
            links: Vec::new(),
        }
    }
}

#[derive(Debug)]
struct SearchResult {
    text: String,
    url: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct DuckDuckGoResponse {
    #[serde(default)]
    heading: String,
    #[serde(default, alias = "Abstract", alias = "AbstractText")]
    abstract_text: String,
    #[serde(default, alias = "AbstractURL", alias = "AbstractUrl")]
    abstract_url: String,
    #[serde(default)]
    results: Vec<DuckDuckGoTopic>,
    #[serde(default)]
    related_topics: Vec<DuckDuckGoTopic>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DuckDuckGoTopic {
    #[serde(default)]
    text: String,
    #[serde(default, alias = "FirstURL")]
    first_url: String,
    #[serde(default)]
    topics: Vec<DuckDuckGoTopic>,
}

fn search_results(search: DuckDuckGoResponse, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    if !search.abstract_text.trim().is_empty() && !search.abstract_url.trim().is_empty() {
        let text = if search.heading.trim().is_empty() {
            search.abstract_text
        } else {
            format!("{}: {}", search.heading, search.abstract_text)
        };
        results.push(SearchResult {
            text,
            url: search.abstract_url,
        });
    }

    collect_topics(&search.results, max_results, &mut results);
    collect_topics(&search.related_topics, max_results, &mut results);
    results.truncate(max_results);
    results
}

fn parse_bing_search_results(html: &str, max_results: usize) -> Vec<SearchResult> {
    let document = Html::parse_document(html);
    let result_selector = Selector::parse("li.b_algo").expect("valid result selector");
    let title_selector = Selector::parse("h2 a").expect("valid title selector");
    let snippet_selector = Selector::parse(".b_caption p").expect("valid snippet selector");
    let mut results = Vec::new();

    for result in document.select(&result_selector) {
        if results.len() >= max_results {
            break;
        }

        let Some(title) = result.select(&title_selector).next() else {
            continue;
        };
        let Some(href) = title.value().attr("href").and_then(bing_result_url) else {
            continue;
        };
        let title_text = element_text(&title);
        let snippet = result
            .select(&snippet_selector)
            .next()
            .map(|element| element_text(&element))
            .unwrap_or_default();
        let text = match (title_text.is_empty(), snippet.is_empty()) {
            (false, false) => format!("{title_text}: {snippet}"),
            (false, true) => title_text,
            (true, false) => snippet,
            (true, true) => continue,
        };

        results.push(SearchResult { text, url: href });
    }

    results
}

fn element_text(element: &scraper::ElementRef<'_>) -> String {
    element
        .text()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn bing_result_url(href: &str) -> Option<String> {
    let parsed = Url::parse(href).ok()?;

    if parsed
        .domain()
        .is_some_and(|domain| domain.ends_with("bing.com"))
    {
        let encoded = parsed
            .query_pairs()
            .find(|(key, _)| key == "u")
            .map(|(_, value)| value.into_owned())?;
        let encoded = encoded.strip_prefix("a1").unwrap_or(&encoded);
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()?;
        let destination = String::from_utf8(bytes).ok()?;
        let destination = Url::parse(&destination).ok()?;
        return matches!(destination.scheme(), "http" | "https").then(|| destination.to_string());
    }

    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

fn collect_topics(topics: &[DuckDuckGoTopic], max_results: usize, results: &mut Vec<SearchResult>) {
    for topic in topics {
        if results.len() >= max_results {
            return;
        }

        if !topic.text.trim().is_empty() && !topic.first_url.trim().is_empty() {
            results.push(SearchResult {
                text: topic.text.clone(),
                url: topic.first_url.clone(),
            });
        }

        collect_topics(&topic.topics, max_results, results);
    }
}

fn format_web_context(query: &str, results: &[SearchResult]) -> String {
    let mut context = format!("Web reference results for query: {query}\n");

    for (index, result) in results.iter().enumerate() {
        context.push_str(&format!(
            "{}. {}\n   URL: {}\n",
            index + 1,
            trim_chars(&result.text, 400),
            result.url
        ));
    }

    context.push_str("Use these references only if relevant. Cite URLs when using web facts.");
    context
}

fn prompt_requests_web(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    let markers = [
        "search",
        "web",
        "internet",
        "latest",
        "recent",
        "today",
        "news",
        "price",
        "weather",
        "source",
        "reference",
        "link",
        "url",
        "site",
        "page",
        "verify",
        "look up",
        "lookup",
        "검색",
        "인터넷",
        "웹",
        "찾아봐",
        "알아봐",
        "검증",
        "최신",
        "최근",
        "오늘",
        "뉴스",
        "가격",
        "날씨",
        "출처",
        "참조",
        "자료",
        "링크",
        "사이트",
        "페이지",
    ];

    markers.iter().any(|marker| lower.contains(marker))
}

fn extract_urls(prompt: &str) -> Vec<String> {
    prompt
        .split_whitespace()
        .filter_map(|word| {
            let url = word.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'' | ',' | '.'
                )
            });

            if url.starts_with("http://") || url.starts_with("https://") {
                Some(url.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn extract_links(base_url: &str, html: &str) -> Vec<String> {
    let mut links = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;

    while let Some(position) = lower[offset..].find("href=") {
        let href_start = offset + position + "href=".len();
        let Some((href, consumed)) = read_attr_value(&html[href_start..]) else {
            offset = href_start;
            continue;
        };
        offset = href_start + consumed;

        if let Some(url) = resolve_url(base_url, href) {
            if is_probably_html_link(&url) && !links.contains(&url) {
                links.push(url);
            }
        }
    }

    links
}

fn read_attr_value(value: &str) -> Option<(&str, usize)> {
    let trimmed = value.trim_start();
    let skipped = value.len() - trimmed.len();
    let quote = trimmed.chars().next()?;

    if quote == '"' || quote == '\'' {
        let rest = &trimmed[quote.len_utf8()..];
        let end = rest.find(quote)?;
        return Some((
            &rest[..end],
            skipped + quote.len_utf8() + end + quote.len_utf8(),
        ));
    }

    let end = trimmed
        .find(|ch: char| ch.is_whitespace() || ch == '>')
        .unwrap_or(trimmed.len());
    Some((&trimmed[..end], skipped + end))
}

fn resolve_url(base_url: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty()
        || href.starts_with('#')
        || href.starts_with("mailto:")
        || href.starts_with("tel:")
        || href.starts_with("javascript:")
    {
        return None;
    }

    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(strip_fragment(href));
    }

    let (scheme, host, base_path) = split_url(base_url)?;
    if href.starts_with("//") {
        return Some(format!("{scheme}:{}", strip_fragment(href)));
    }

    if href.starts_with('/') {
        return Some(strip_fragment(&format!("{scheme}://{host}{href}")));
    }

    let directory = base_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("");
    Some(strip_fragment(&format!(
        "{scheme}://{host}{directory}/{href}"
    )))
}

fn same_origin(left: &str, right: &str) -> bool {
    match (split_url(left), split_url(right)) {
        (Some((left_scheme, left_host, _)), Some((right_scheme, right_host, _))) => {
            left_scheme == right_scheme && left_host == right_host
        }
        _ => false,
    }
}

fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..slash];
    let path = if slash < rest.len() {
        &rest[slash..]
    } else {
        "/"
    };

    Some((scheme, host, path))
}

fn strip_fragment(url: &str) -> String {
    url.split('#').next().unwrap_or(url).to_string()
}

fn is_probably_html_link(url: &str) -> bool {
    let path = split_url(url).map(|(_, _, path)| path).unwrap_or(url);
    let path = path.split('?').next().unwrap_or(path).to_ascii_lowercase();
    let blocked_extensions = [
        ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".css", ".js", ".zip", ".tar", ".gz",
        ".mp4", ".mp3", ".wav", ".avi", ".mov", ".pdf",
    ];

    !blocked_extensions
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn html_to_text(html: &str) -> String {
    let without_scripts = strip_between(html, "<script", "</script>");
    let without_styles = strip_between(&without_scripts, "<style", "</style>");
    let mut text = String::new();
    let mut in_tag = false;

    for ch in without_styles.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }

    decode_basic_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_between(value: &str, start_marker: &str, end_marker: &str) -> String {
    let mut output = String::new();
    let mut rest = value;

    while let Some(start) = rest.to_ascii_lowercase().find(start_marker) {
        output.push_str(&rest[..start]);
        let after_start = &rest[start..];
        let Some(end) = after_start.to_ascii_lowercase().find(end_marker) else {
            return output;
        };
        rest = &after_start[end + end_marker.len()..];
    }

    output.push_str(rest);
    output
}

fn decode_basic_entities(value: &str) -> String {
    value
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();

    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            b' ' => encoded.push('+'),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }

    encoded
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut trimmed = chars.by_ref().take(max_chars).collect::<String>();

    if chars.next().is_some() {
        trimmed.push_str("...");
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::{
        bing_result_url, extract_links, extract_urls, html_to_text, parse_bing_search_results,
        percent_encode, prompt_requests_web,
    };

    #[test]
    fn prompt_requests_web_detects_korean_markers() {
        assert!(prompt_requests_web("오늘 뉴스 검색해줘"));
        assert!(prompt_requests_web("인터넷에서 최신 자료 찾아봐"));
        assert!(!prompt_requests_web("이 첨부 문서 요약해줘"));
        assert!(!prompt_requests_web("이 파일 확인해줘"));
    }

    #[test]
    fn percent_encode_encodes_spaces_and_unicode() {
        assert_eq!(percent_encode("a b"), "a+b");
        assert_eq!(percent_encode("코미"), "%EC%BD%94%EB%AF%B8");
    }

    #[test]
    fn extract_urls_detects_http_links() {
        assert_eq!(
            extract_urls("봐줘 https://example.com/test."),
            vec!["https://example.com/test"]
        );
    }

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        assert_eq!(
            html_to_text("<p>Hello&nbsp;<b>world</b></p><script>bad()</script>"),
            "Hello world"
        );
    }

    #[test]
    fn extract_links_resolves_same_page_links() {
        let links = extract_links(
            "https://example.com/docs/index.html",
            r#"<a href="/detail">A</a><a href="more.html#top">B</a>"#,
        );

        assert_eq!(
            links,
            vec![
                "https://example.com/detail".to_string(),
                "https://example.com/docs/more.html".to_string()
            ]
        );
    }

    #[test]
    fn bing_result_url_extracts_destination() {
        assert_eq!(
            bing_result_url("https://www.bing.com/ck/a?u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9kb2M")
                .as_deref(),
            Some("https://example.com/doc")
        );
    }

    #[test]
    fn parse_bing_search_results_reads_title_snippet_and_url() {
        let html = r#"
            <li class="b_algo">
              <h2><a href="https://example.com/doc">Example title</a></h2>
              <div class="b_caption"><p>Useful <b>summary</b>.</p></div>
            </li>
        "#;
        let results = parse_bing_search_results(html, 5);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/doc");
        assert_eq!(results[0].text, "Example title: Useful summary.");
    }
}
