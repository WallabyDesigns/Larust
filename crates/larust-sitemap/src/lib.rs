//! Laravel's `spatie/laravel-sitemap` - an XML `<urlset>` builder plus a
//! convenience response wrapper, not an automatically-mounted route (the
//! same "nothing auto-mounted, the app wires it explicitly" convention
//! every other shim crate this session follows - `larust_http::throttle`/
//! `csrf`/`responsecache` included). A route handler builds a
//! `Vec<SitemapEntry>` - mixing static pages discovered via
//! [`from_static_routes`] with dynamic per-model URLs the app supplies
//! itself, since this crate has no visibility into an app's own models,
//! the same reasoning every other shim crate here already documents for
//! not knowing an app's `users`/`posts`-shaped tables - and returns it
//! via [`response`]:
//!
//! ```ignore
//! async fn sitemap(router: &Router) -> impl IntoResponse {
//!     let mut entries = larust_sitemap::from_static_routes(
//!         &larust_support::url(""),
//!         &router.routes(),
//!     );
//!     for post in Post::all().await? {
//!         entries.push(
//!             SitemapEntry::new(larust_support::url(&format!("/posts/{}", post.id)))
//!                 .last_modified(post.updated_at)
//!                 .change_freq(ChangeFreq::Weekly),
//!         );
//!     }
//!     larust_sitemap::response(&entries)
//! }
//! ```
//!
//! ## Deliberately out of scope for this version
//!
//! - **No sitemap index / multi-file pagination.** The sitemap protocol
//!   caps a single file at 50,000 URLs; spatie's own package auto-splits
//!   into a `SitemapIndex` beyond that. This crate always emits one
//!   `<urlset>` - a real follow-up if an app's URL count ever approaches
//!   that limit, not attempted speculatively here.
//! - **No built-in caching.** Generating a sitemap can be expensive for a
//!   large site, and the app is best placed to decide the right TTL -
//!   wrap whichever route calls [`response`] in `larust_http::
//!   responsecache::for_minutes(...)` rather than this crate reinventing
//!   caching.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};

/// Laravel's own `changefreq` vocabulary - advisory only, per the sitemap
/// protocol spec (crawlers aren't required to honor it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeFreq {
    Always,
    Hourly,
    Daily,
    Weekly,
    Monthly,
    Yearly,
    Never,
}

impl ChangeFreq {
    fn as_str(self) -> &'static str {
        match self {
            ChangeFreq::Always => "always",
            ChangeFreq::Hourly => "hourly",
            ChangeFreq::Daily => "daily",
            ChangeFreq::Weekly => "weekly",
            ChangeFreq::Monthly => "monthly",
            ChangeFreq::Yearly => "yearly",
            ChangeFreq::Never => "never",
        }
    }
}

/// One `<url>` entry - build with [`SitemapEntry::new`], then chain
/// whichever optional fields apply (Laravel's own `Url::create($loc)
/// ->setLastModificationDate(...)->setChangeFrequency(...)
/// ->setPriority(...)`).
#[derive(Debug, Clone)]
pub struct SitemapEntry {
    loc: String,
    last_modified: Option<DateTime<Utc>>,
    change_freq: Option<ChangeFreq>,
    priority: Option<f32>,
}

impl SitemapEntry {
    /// `loc` must already be an absolute URL - see `larust_support::url()`
    /// (or [`from_static_routes`], which calls it for you) for building
    /// one from a relative path.
    pub fn new(loc: impl Into<String>) -> Self {
        Self {
            loc: loc.into(),
            last_modified: None,
            change_freq: None,
            priority: None,
        }
    }

    pub fn last_modified(mut self, when: DateTime<Utc>) -> Self {
        self.last_modified = Some(when);
        self
    }

    pub fn change_freq(mut self, freq: ChangeFreq) -> Self {
        self.change_freq = Some(freq);
        self
    }

    /// Clamped to the sitemap protocol's own `0.0..=1.0` range - a caller
    /// passing an out-of-range value still gets a valid sitemap, rather
    /// than one a strict crawler might reject outright.
    pub fn priority(mut self, priority: f32) -> Self {
        self.priority = Some(priority.clamp(0.0, 1.0));
        self
    }
}

/// Every `GET` route registered on a `larust_http::Router` (pass
/// `&router.routes()`) with no `{param}` placeholder - the static half of
/// a sitemap, turned into absolute-URL entries under `base_url`. Dynamic
/// per-model URLs (`/posts/{post}`) aren't included: this crate has no
/// visibility into an app's own models to enumerate them. Combine this
/// function's output with the app's own dynamically-built entries.
pub fn from_static_routes(base_url: &str, routes: &[larust_http::RouteInfo]) -> Vec<SitemapEntry> {
    routes
        .iter()
        .filter(|route| route.method == "GET" && !route.path.contains('{'))
        .map(|route| SitemapEntry::new(join_url(base_url, &route.path)))
        .collect()
}

/// The same "exactly one `/` between the two halves, regardless of
/// whether either side already has one" joining rule
/// `larust_support::url_helper::join_url` uses - duplicated here rather
/// than depending on `larust-support` for it, which would be a circular
/// dependency (`larust-support` itself depends on this crate to
/// re-export it).
fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Builds the full `<?xml ...?><urlset>...</urlset>` document - the
/// sitemap protocol (`https://www.sitemaps.org/protocol.html`).
pub fn build_xml(entries: &[SitemapEntry]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n",
    );
    for entry in entries {
        xml.push_str("  <url>\n");
        xml.push_str(&format!("    <loc>{}</loc>\n", xml_escape(&entry.loc)));
        if let Some(last_modified) = entry.last_modified {
            xml.push_str(&format!(
                "    <lastmod>{}</lastmod>\n",
                last_modified.to_rfc3339()
            ));
        }
        if let Some(change_freq) = entry.change_freq {
            xml.push_str(&format!(
                "    <changefreq>{}</changefreq>\n",
                change_freq.as_str()
            ));
        }
        if let Some(priority) = entry.priority {
            xml.push_str(&format!("    <priority>{priority:.1}</priority>\n"));
        }
        xml.push_str("  </url>\n");
    }
    xml.push_str("</urlset>\n");
    xml
}

/// XML-escapes `&`/`<`/`>` - the only characters that can break `<loc>`'s
/// own element structure. A URL has no legal use for a literal `<`/`>`
/// anyway, but `&` is common (query strings) and must not reach the
/// output unescaped.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// [`build_xml`], wrapped as a ready-to-return axum response with the
/// right content type - Laravel's own package's `Sitemap::render()`
/// equivalent. Not auto-mounted anywhere; a route handler calls this
/// directly (see this crate's own doc comment for a full example).
pub fn response(entries: &[SitemapEntry]) -> Response {
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        build_xml(entries),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn build_xml_produces_an_empty_urlset_for_no_entries() {
        assert_eq!(
            build_xml(&[]),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n\
             </urlset>\n"
        );
    }

    #[test]
    fn build_xml_includes_only_the_fields_that_were_set() {
        let entries = [SitemapEntry::new("https://example.test/")];
        let xml = build_xml(&entries);
        assert!(xml.contains("<loc>https://example.test/</loc>"));
        assert!(!xml.contains("<lastmod>"));
        assert!(!xml.contains("<changefreq>"));
        assert!(!xml.contains("<priority>"));
    }

    #[test]
    fn build_xml_includes_every_set_field() {
        let when = Utc.with_ymd_and_hms(2026, 8, 23, 12, 0, 0).unwrap();
        let entries = [SitemapEntry::new("https://example.test/posts/1")
            .last_modified(when)
            .change_freq(ChangeFreq::Weekly)
            .priority(0.8)];
        let xml = build_xml(&entries);
        assert!(xml.contains("<loc>https://example.test/posts/1</loc>"));
        assert!(xml.contains("<lastmod>2026-08-23T12:00:00+00:00</lastmod>"));
        assert!(xml.contains("<changefreq>weekly</changefreq>"));
        assert!(xml.contains("<priority>0.8</priority>"));
    }

    #[test]
    fn priority_is_clamped_to_the_valid_range() {
        let entries = [
            SitemapEntry::new("https://example.test/a").priority(5.0),
            SitemapEntry::new("https://example.test/b").priority(-1.0),
        ];
        let xml = build_xml(&entries);
        assert!(xml.contains("<priority>1.0</priority>"));
        assert!(xml.contains("<priority>0.0</priority>"));
    }

    #[test]
    fn loc_is_xml_escaped() {
        let entries = [SitemapEntry::new("https://example.test/search?q=a&b=<c>")];
        let xml = build_xml(&entries);
        assert!(xml.contains("<loc>https://example.test/search?q=a&amp;b=&lt;c&gt;</loc>"));
        assert!(!xml.contains("q=a&b="));
    }

    #[test]
    fn from_static_routes_keeps_only_get_routes_with_no_placeholder() {
        let routes = [
            larust_http::RouteInfo {
                method: "GET",
                path: "/posts".to_string(),
                name: Some("posts.index".to_string()),
            },
            larust_http::RouteInfo {
                method: "GET",
                path: "/posts/{post}".to_string(),
                name: Some("posts.show".to_string()),
            },
            larust_http::RouteInfo {
                method: "POST",
                path: "/posts".to_string(),
                name: Some("posts.store".to_string()),
            },
        ];
        let entries = from_static_routes("https://example.test", &routes);
        let xml = build_xml(&entries);
        assert_eq!(entries.len(), 1);
        assert!(xml.contains("<loc>https://example.test/posts</loc>"));
        assert!(!xml.contains("{post}"));
    }

    #[test]
    fn from_static_routes_joins_base_url_and_path_with_exactly_one_slash() {
        let routes = [larust_http::RouteInfo {
            method: "GET",
            path: "/about".to_string(),
            name: None,
        }];
        let entries = from_static_routes("https://example.test/", &routes);
        assert!(build_xml(&entries).contains("<loc>https://example.test/about</loc>"));
    }
}
