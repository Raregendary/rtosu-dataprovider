//! Serving of user-supplied, tosu v2 API compatible browser overlays.
//!
//! Users drop overlay folders into the configured `browser_overlays` directory
//! and the server renders them over its own origin, so a browser source URL in
//! OBS can point straight at them. Overlays written against tosu usually point
//! at a hardcoded `ws://127.0.0.1:24050/...` socket, which cannot be relied on
//! because the port is configurable, so every served `index.html` gets a small
//! compatibility shim injected ahead of its own scripts. The shim redirects the
//! tosu endpoints onto whichever origin the overlay was actually loaded from.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use anyhow::{Context, Result};
use std::sync::Arc;

/// Base path the overlay dashboard, assets, and shim are served under.
pub const OVERLAYS_BASE: &str = "/overlays";

/// Reserved path segment that holds server-generated assets instead of user
/// files. It cannot collide with an overlay slug because slugs are real
/// directory names on disk and this segment never is.
const RESERVED_SEGMENT: &str = "__rtosu";

/// Relative URL of the injected compatibility shim.
pub fn shim_url() -> String {
    format!("{OVERLAYS_BASE}/{RESERVED_SEGMENT}/overlay-shim.js")
}

/// Absolute URL of the compatibility shim for a given host header, so overlays
/// loaded from another device's LAN address still resolve it.
pub fn shim_url_for_host(host: &str) -> String {
    if host.is_empty() {
        shim_url()
    } else {
        format!("http://{host}{}", shim_url())
    }
}

/// One overlay directory found on disk.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// URL path segment, equal to the directory name.
    pub slug: String,
    /// Absolute path of the overlay directory.
    pub dir: PathBuf,
    /// Absolute path of the entry document.
    pub index: PathBuf,
    pub metadata: OverlayMetadata,
}

impl Overlay {
    /// URL a browser source should be pointed at.
    pub fn url(&self) -> String {
        format!("{OVERLAYS_BASE}/{}/", percent_encode(&self.slug))
    }
}

/// Parsed `metadata.txt` of an overlay.
///
/// The format is tosu's: one `Key: Value` pair per line, with `\n` escapes
/// inside values. Unknown keys are ignored and missing keys fall back to the
/// directory name so a bare `index.html` folder still lists cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayMetadata {
    pub usecase: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub compatible_with: String,
    pub resolution: String,
    pub author_links: String,
    pub notes: String,
}

impl OverlayMetadata {
    /// Parse tosu `metadata.txt` content.
    pub fn parse(content: &str) -> Self {
        let mut meta = Self::default();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = unescape_metadata_value(value.trim());
            let value = value.as_str();

            match key.trim().to_ascii_lowercase().as_str() {
                "usecase" => meta.usecase = value.to_string(),
                "name" => meta.name = value.to_string(),
                "version" => meta.version = value.to_string(),
                "author" => meta.author = value.to_string(),
                "compatiblewith" => meta.compatible_with = value.to_string(),
                "resolution" => meta.resolution = value.to_string(),
                "authorlinks" => meta.author_links = value.to_string(),
                "notes" => meta.notes = value.to_string(),
                _ => {}
            }
        }

        meta
    }

    /// Read `metadata.txt` from an overlay directory, falling back to defaults
    /// when the file is missing or unreadable.
    pub fn load(dir: &Path) -> Self {
        fs::read_to_string(dir.join("metadata.txt"))
            .map(|content| Self::parse(&content))
            .unwrap_or_default()
    }

    /// Display name, falling back to the directory name.
    pub fn display_name(&self, fallback: &str) -> String {
        if self.name.is_empty() {
            fallback.to_string()
        } else {
            self.name.clone()
        }
    }
}

/// tosu writes literal `\n` sequences rather than real newlines, because the
/// value has to stay on one line.
fn unescape_metadata_value(value: &str) -> String {
    value.replace("\\n", "\n").replace("\\r", "\r")
}

/// Cached discovery result, keyed by the root directory's modification time.
type DiscoveryCache = RwLock<Option<(Option<SystemTime>, Arc<Vec<Overlay>>)>>;

/// Serves overlays from a root directory and re-scans it when it changes, so a
/// folder dropped in during a tournament shows up without a restart.
pub struct OverlayStore {
    pub root: PathBuf,
    cache: DiscoveryCache,
}

impl OverlayStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cache: RwLock::new(None),
        }
    }

    /// Modification time of the root directory, which changes whenever an
    /// overlay folder is added or removed.
    fn stamp(&self) -> Option<SystemTime> {
        fs::metadata(&self.root)
            .and_then(|meta| meta.modified())
            .ok()
    }

    /// All overlays currently on disk, rescanning only when the root changed.
    pub async fn list(&self) -> Arc<Vec<Overlay>> {
        let stamp = self.stamp();

        if let Some((cached_stamp, overlays)) = self.cache.read().ok().and_then(|g| g.clone())
            && cached_stamp == stamp
        {
            return overlays;
        }

        let root = self.root.clone();
        let found = match tokio::task::spawn_blocking(move || discover(&root)).await {
            Ok(found) => found,
            Err(err) => {
                tracing::error!("overlay scan task failed: {err}");
                return Arc::new(Vec::new());
            }
        };

        let overlays = Arc::new(found);
        if let Ok(mut guard) = self.cache.write() {
            *guard = Some((stamp, overlays.clone()));
        }
        overlays
    }

    /// Look up one overlay by its directory name.
    pub async fn find(&self, slug: &str) -> Option<Overlay> {
        self.list()
            .await
            .iter()
            .find(|overlay| overlay.slug == slug)
            .cloned()
    }
}

/// Minimal percent-encoder: RFC 3986 `unreserved` set only, so every other byte
/// is escaped as `%XX`.
///
/// The escape set includes `%` itself, which is what makes the pair round-trip:
/// a folder named `100% Pure` becomes `100%25%20Pure` and decodes back exactly,
/// while leaving `%` bare would let an existing `%2F` in a folder name be read
/// back as a path separator. `+` is escaped too, because the decoder does no
/// form decoding and so would hand back `%2B` unchanged.
pub fn percent_encode(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0x0F)] as char);
            }
        }
    }

    out
}

/// Minimal percent-decoder: overlays frequently use spaces in folder names
/// (`"Luscent Remake by Dartandr"`), which arrive as `%20`.
pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Discover every overlay directory under `root`.
///
/// A directory qualifies when it contains an `index.html`. Hidden directories,
/// the reserved segment, and non-directories are skipped, and unreadable
/// entries are logged rather than aborting the whole scan.
pub fn discover(root: &Path) -> Vec<Overlay> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "overlay directory '{}' is not readable: {err}; browser overlays are disabled",
                root.display()
            );
            return Vec::new();
        }
    };

    let mut overlays = Vec::new();

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let Some(slug) = dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if slug.starts_with('.') || slug == RESERVED_SEGMENT {
            continue;
        }

        let index = dir.join("index.html");
        if !index.is_file() {
            continue;
        }

        overlays.push(Overlay {
            slug: slug.to_string(),
            dir: dir.clone(),
            index,
            metadata: OverlayMetadata::load(&dir),
        });
    }

    overlays.sort_by(|a, b| {
        a.metadata
            .display_name(&a.slug)
            .to_ascii_lowercase()
            .cmp(&b.metadata.display_name(&b.slug).to_ascii_lowercase())
    });

    overlays
}

/// Resolve a request path inside an overlay directory.
///
/// Returns `None` for traversal attempts, absolute components, and paths that
/// escape the overlay directory. The result is guaranteed to stay under `dir`.
pub fn resolve_within(dir: &Path, rel_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(rel_path);
    if decoded.contains('\0') {
        return None;
    }

    let mut resolved = dir.to_path_buf();
    let mut saw_component = false;

    for part in decoded.split(['/', '\\']) {
        // Empty parts come from repeated separators, which are harmless.
        if part.is_empty() || part == "." {
            continue;
        }
        // `..` and Windows prefixes/reparse points never get to touch the FS.
        if part == ".." {
            return None;
        }
        if part.contains(':') {
            return None;
        }

        let candidate = Path::new(part);
        if candidate.is_absolute() {
            return None;
        }
        if candidate
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        {
            return None;
        }

        resolved.push(part);
        saw_component = true;
    }

    if !saw_component {
        return None;
    }

    // Belt and braces: the lexical checks above already exclude traversal, but
    // verify against the fully resolved path in case of symlinked entries.
    let canonical_dir = fs::canonicalize(dir).ok()?;
    let canonical = fs::canonicalize(&resolved).ok()?;
    if !canonical.starts_with(&canonical_dir) {
        return None;
    }

    Some(canonical)
}

/// Inject the compatibility shim into a served HTML document so it installs
/// before any overlay script runs.
///
/// A `<head>` is preferred, then `<html>`, then the doctype; if none exist the
/// script is prefixed, which still executes before everything else.
pub fn inject_shim(html: &str, shim_src: &str) -> String {
    let tag = format!(r#"<script src="{shim_src}"></script>"#);

    // `<head>` first, then `<html>`. The insert point is just past the opening
    // tag's `>` so attributes such as `<html lang="en">` stay intact.
    if let Some(index) = find_tag(html, "head") {
        return insert_at(html, tag_end(html, index), &tag);
    }

    if let Some(index) = find_tag(html, "html") {
        return insert_at(html, tag_end(html, index), &tag);
    }

    if let Some(index) = find_doctype(html) {
        return insert_at(html, index, &tag);
    }

    format!("{tag}{html}")
}

fn insert_at(html: &str, at: usize, tag: &str) -> String {
    let mut out = String::with_capacity(html.len() + tag.len());
    out.push_str(&html[..at]);
    out.push_str(tag);
    out.push_str(&html[at..]);
    out
}

/// Index just past the `>` closing the tag that starts at `start`.
///
/// Quoted attribute values are skipped so a `>` inside one cannot end the tag
/// early.
fn tag_end(html: &str, start: usize) -> usize {
    let bytes = html.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;

    while i < bytes.len() {
        let byte = bytes[i];
        match quote {
            Some(open) if byte == open => quote = None,
            Some(_) => {}
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return i + 1,
                _ => {}
            },
        }
        i += 1;
    }

    html.len()
}

/// Byte offset of `<tag`, ignoring case and not matching `<header>` when
/// looking for `head`.
fn find_tag(html: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let haystack = html.to_ascii_lowercase();

    let mut from = 0;
    while let Some(relative) = haystack[from..].find(&needle) {
        let index = from + relative;
        let after = &haystack[index + needle.len()..];
        let boundary = after
            .chars()
            .next()
            .is_none_or(|c| c == '>' || c == '/' || c.is_whitespace());
        if boundary {
            return Some(index);
        }
        from = index + needle.len();
    }

    None
}

fn find_doctype(html: &str) -> Option<usize> {
    let end = html.find('>')?;
    if html[..end].trim_start().len() >= 9
        && html[..end].to_ascii_lowercase().starts_with("<!doctype")
    {
        Some(end + 1)
    } else {
        None
    }
}

/// The client-side compatibility shim.
///
/// Overlays are third-party files that assume a hardcoded tosu origin. This
/// rewrites the tosu socket and file endpoints onto the overlay's own origin so
/// a folder can be dropped in unmodified, and also exposes `window.__rtosu` for
/// overlays that prefer to read the provider address explicitly.
pub const OVERLAY_SHIM_JS: &str = r##"/* rtosu-dataprovider browser overlay compatibility shim.
 * Injected by the server into every overlay page. Rewrites the tosu API
 * endpoints onto the origin the overlay was actually loaded from, so overlays
 * that hardcode ws://127.0.0.1:24050 keep working on a custom port or over LAN.
 */
(function () {
  'use strict';

  if (window.__rtosuOverlayShim) return;
  window.__rtosuOverlayShim = true;

  var ORIGIN = window.location.origin;
  var HOST = window.location.host;
  var LOOPBACK = ['127.0.0.1', 'localhost', '0.0.0.0', '[::1]', '::1', '::'];

  // tosu socket endpoints -> rtosu-dataprovider equivalents. rtosu serves the
  // tosu v2 payload on /websocket/v2, so the tosu v1 /ws path is mapped onto it.
  var SOCKET_ROUTES = {
    '/ws': '/websocket/v2',
    '/websocket/v2': '/websocket/v2',
    '/websocket/v2/precise': '/websocket/v2/precise'
  };

  // tsu file endpoints used by overlays to show the beatmap background.
  var FILE_ROUTES = [
    ['/backgroundImage', '/files/beatmap/background'],
    ['/Songs/', '/files/beatmap/'],
    ['/files/beatmap/', '/files/beatmap/'],
    ['/files/skin/', '/files/skin/']
  ];

  function isLoopback(hostname) {
    return LOOPBACK.indexOf(String(hostname).toLowerCase()) !== -1;
  }

  function resolve(input) {
    if (typeof input !== 'string' || input === '') return null;
    try {
      return new URL(input, ORIGIN);
    } catch (err) {
      return null;
    }
  }

  /* Overlay is already on our origin when the host matches or is loopback. */
  function targetsProvider(url) {
    return url.host === HOST || isLoopback(url.hostname);
  }

  function rewriteSocketUrl(input) {
    var url = resolve(input);
    if (!url) return input;
    if (url.protocol !== 'ws:' && url.protocol !== 'wss:') return input;
    if (!targetsProvider(url)) return input;

    // A bare origin such as ws://127.0.0.1:24050 means the tosu stream root.
    var route = SOCKET_ROUTES[url.pathname];
    if (!route) {
      if (url.pathname !== '' && url.pathname !== '/') return input;
      route = '/websocket/v2';
    }

    // Reuse the page scheme so an https page does not open a mixed-content ws.
    var scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    return scheme + '//' + HOST + route + url.search;
  }

  function rewriteHttpUrl(input) {
    var url = resolve(input);
    if (!url) return input;
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return input;
    if (!targetsProvider(url)) return input;

    for (var i = 0; i < FILE_ROUTES.length; i++) {
      var from = FILE_ROUTES[i][0];
      if (url.pathname === from || url.pathname.indexOf(from) === 0) {
        return ORIGIN + FILE_ROUTES[i][1] + url.pathname.slice(from.length) + url.search;
      }
    }

    return input;
  }

  /* WebSocket: a Proxy keeps instanceof, the prototype chain, and the static
   * CONNECTING/OPEN constants intact, and also covers ReconnectingWebSocket
   * because that library constructs a global WebSocket internally. */
  var NativeWebSocket = window.WebSocket;
  if (NativeWebSocket) {
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct: function (target, args) {
        if (args.length > 0) args[0] = rewriteSocketUrl(args[0]);
        return Reflect.construct(target, args);
      }
    });
  }

  var nativeFetch = window.fetch;
  if (typeof nativeFetch === 'function') {
    window.fetch = function (input, init) {
      if (typeof input === 'string') {
        input = rewriteHttpUrl(input);
      } else if (window.Request && input instanceof window.Request) {
        var rewritten = rewriteHttpUrl(input.url);
        if (rewritten !== input.url) input = new window.Request(rewritten, input);
      }
      return nativeFetch.call(this, input, init);
    };
  }

  if (window.XMLHttpRequest) {
    var nativeOpen = window.XMLHttpRequest.prototype.open;
    window.XMLHttpRequest.prototype.open = function (method, url) {
      var args = Array.prototype.slice.call(arguments);
      if (args.length > 1) args[1] = rewriteHttpUrl(args[1]);
      return nativeOpen.apply(this, args);
    };
  }

  /* Explicit handle for overlay authors that would rather not rely on the
   * rewrite, e.g. to pick the precise stream or a poll fallback. */
  window.__rtosu = {
    origin: ORIGIN,
    socketUrl: '/websocket/v2',
    preciseSocketUrl: '/websocket/v2/precise',
    jsonUrl: '/json/v2',
    healthUrl: '/health',
    backgroundUrl: '/files/beatmap/background',
    overlayBase: '/overlays/',
    socket: function (path) {
      var scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      return new NativeWebSocket(scheme + '//' + HOST + (path || '/websocket/v2'));
    }
  };
})();
"##;

/// Escape text for interpolation into HTML content or a quoted attribute.
pub fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Render the overlay dashboard listing every discovered overlay.
pub fn dashboard_html(overlays: &[Overlay], root: &Path) -> String {
    let mut cards = String::new();

    for overlay in overlays {
        let meta = &overlay.metadata;
        let title = escape_html(&meta.display_name(&overlay.slug));
        let url = escape_html(&overlay.url());

        let mut details = String::new();
        for (label, value) in [
            ("Author", &meta.author),
            ("Version", &meta.version),
            ("Resolution", &meta.resolution),
            ("Use case", &meta.usecase),
            ("Compatible with", &meta.compatible_with),
        ] {
            if !value.is_empty() {
                details.push_str(&format!(
                    "<dt>{}</dt><dd>{}</dd>",
                    escape_html(label),
                    escape_html(value)
                ));
            }
        }

        if !meta.author_links.is_empty() {
            let link = meta.author_links.lines().next().unwrap_or_default().trim();
            if link.starts_with("http://") || link.starts_with("https://") {
                details.push_str(&format!(
                    "<dt>Links</dt><dd><a href=\"{0}\" target=\"_blank\" rel=\"noreferrer noopener\">{0}</a></dd>",
                    escape_html(link)
                ));
            }
        }

        if !meta.notes.is_empty() {
            details.push_str(&format!(
                "<dt>Notes</dt><dd class=\"notes\">{}</dd>",
                escape_html(&meta.notes)
            ));
        }

        if details.is_empty() {
            details.push_str("<dt>Status</dt><dd>ready</dd>");
        }

        cards.push_str(&format!(
            r#"<article class="card">
  <h2>{title}</h2>
  <dl>{details}</dl>
  <div class="row">
    <input class="url" type="text" readonly value="{url}" aria-label="Browser source URL">
    <button type="button" data-copy="{url}">Copy</button>
    <a class="open" href="{url}" target="_blank" rel="noreferrer noopener">Open</a>
  </div>
</article>"#
        ));
    }

    let body = if overlays.is_empty() {
        format!(
            r#"<p class="empty">No overlays found in <code>{}</code>.</p>
<p class="hint">Create a folder per overlay containing an <code>index.html</code>, then reload this page.
Drop-in tosu v2 overlays work unmodified &mdash; their tosu API calls are rewritten to this server automatically.</p>"#,
            escape_html(&root.display().to_string())
        )
    } else {
        format!(r#"<div class="grid">{cards}</div>"#)
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>rtosu browser overlays</title>
<style>
:root {{ color-scheme: dark; }}
* {{ box-sizing: border-box; }}
body {{
  margin: 0;
  padding: 32px;
  font: 14px/1.5 "Segoe UI", system-ui, sans-serif;
  background: #12141a;
  color: #e7e9ee;
}}
header {{ margin-bottom: 24px; }}
h1 {{ margin: 0 0 4px; font-size: 20px; }}
.sub {{ margin: 0; color: #8b93a7; }}
.grid {{ display: grid; gap: 16px; grid-template-columns: repeat(auto-fill, minmax(340px, 1fr)); }}
.card {{
  display: flex; flex-direction: column; gap: 12px;
  padding: 18px; border: 1px solid #262b36; border-radius: 10px; background: #191c24;
}}
.card h2 {{ margin: 0; font-size: 16px; }}
dl {{ display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 0; font-size: 13px; }}
dt {{ color: #8b93a7; }}
dd {{ margin: 0; overflow-wrap: anywhere; }}
dd.notes {{ white-space: pre-line; color: #b6bdcd; }}
.row {{ display: flex; gap: 8px; margin-top: auto; }}
.url {{
  flex: 1; min-width: 0; padding: 7px 9px; font: 12px/1.4 ui-monospace, Consolas, monospace;
  color: #cfd6e6; background: #0f1116; border: 1px solid #262b36; border-radius: 6px;
}}
button, .open {{
  padding: 7px 12px; font-size: 12px; color: #e7e9ee; cursor: pointer; text-decoration: none;
  background: #2a3040; border: 1px solid #39405a; border-radius: 6px;
}}
button:hover, .open:hover {{ background: #39405a; }}
.empty, .hint {{ color: #b6bdcd; }}
code {{ font-family: ui-monospace, Consolas, monospace; color: #ff9ec4; }}
a {{ color: #8ab4ff; }}
</style>
</head>
<body>
<header>
  <h1>rtosu browser overlays</h1>
  <p class="sub">{count} overlay(s) in <code>{root}</code> &mdash; add the URL as an OBS <em>Browser</em> source.</p>
</header>
{body}
<script>
document.addEventListener('click', function (event) {{
  var button = event.target.closest('button[data-copy]');
  if (!button) return;
  var value = button.getAttribute('data-copy');
  var done = function () {{
    var previous = button.textContent;
    button.textContent = 'Copied';
    setTimeout(function () {{ button.textContent = previous; }}, 1200);
  }};
  if (navigator.clipboard) {{ navigator.clipboard.writeText(value).then(done, done); }} else {{ done(); }}
}});
</script>
</body>
</html>"#,
        count = overlays.len(),
        root = escape_html(&root.display().to_string()),
    )
}

/// Map a file extension to a content type.
///
/// Overlays ship fonts, images, audio, and shader files, so the common overlay
/// asset types are covered; anything unrecognised falls back to a byte stream.
pub fn content_type_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "txt" | "metadata" => "text/plain; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "wasm" => "application/wasm",
        "glb" | "gltf" => "model/gltf-binary",
        "bin" | "osu" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// Read a file for serving, mapping the common IO failures onto a message the
/// caller can turn into a status code.
pub async fn read_file(path: &Path) -> Result<Vec<u8>> {
    tokio::fs::read(path)
        .await
        .with_context(|| format!("reading overlay file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rtosu-overlays-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    fn write(root: &Path, rel: &str, content: &str) -> PathBuf {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(&path, content).expect("write file");
        path
    }

    #[test]
    fn metadata_parses_tosu_format() {
        let meta = OverlayMetadata::parse(
            "Usecase: BGCC5 player overlay\nName: BGCC5PlayerOverlay\nVersion: 1.0.0\nAuthor: Raregendary\nCompatibleWith: gosu, tosu\nResolution: 380x100\n",
        );

        assert_eq!(meta.usecase, "BGCC5 player overlay");
        assert_eq!(meta.name, "BGCC5PlayerOverlay");
        assert_eq!(meta.version, "1.0.0");
        assert_eq!(meta.author, "Raregendary");
        assert_eq!(meta.compatible_with, "gosu, tosu");
        assert_eq!(meta.resolution, "380x100");
    }

    #[test]
    fn metadata_is_case_insensitive_on_keys() {
        let meta = OverlayMetadata::parse("NAME: Spaced\nauthorlinks: https://example.com\n");
        assert_eq!(meta.name, "Spaced");
        assert_eq!(meta.author_links, "https://example.com");
    }

    #[test]
    fn metadata_unescapes_newlines_and_skips_junk() {
        let meta = OverlayMetadata::parse(
            "Notes: first line\\nsecond line\n\nnot a pair\n# comment\nAuthor: me\n",
        );
        assert_eq!(meta.notes, "first line\nsecond line");
        assert_eq!(meta.author, "me");
    }

    #[test]
    fn metadata_display_name_falls_back_to_directory() {
        let meta = OverlayMetadata::default();
        assert_eq!(meta.display_name("My Overlay"), "My Overlay");
    }

    #[test]
    fn discover_finds_overlays_and_skips_the_rest() {
        let root = temp_root("discover");
        write(&root, "Alpha/index.html", "<html></html>");
        write(&root, "Alpha/metadata.txt", "Name: Alpha\n");
        write(&root, "Beta/index.html", "<html></html>");
        write(&root, "notes.txt", "no index");
        fs::create_dir_all(root.join(".hidden")).unwrap();
        write(&root, ".hidden/index.html", "<html></html>");
        fs::create_dir_all(root.join(RESERVED_SEGMENT)).unwrap();
        write(&root, "__rtosu/index.html", "<html></html>");

        let overlays = discover(&root);
        let names: Vec<_> = overlays.iter().map(|o| o.slug.as_str()).collect();

        assert_eq!(names, vec!["Alpha", "Beta"]);
        assert_eq!(overlays[0].metadata.name, "Alpha");
        assert_eq!(overlays[0].url(), "/overlays/Alpha/");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn discover_on_missing_root_is_empty() {
        let missing = std::env::temp_dir().join("rtosu-overlays-missing-does-not-exist");
        assert!(discover(&missing).is_empty());
    }

    #[test]
    fn overlay_url_percent_encodes_a_slug_with_a_percent_sign() {
        let root = temp_root("url-encode");
        write(&root, "100% Pure/index.html", "<html></html>");

        let overlays = discover(&root);
        let overlay = overlays
            .iter()
            .find(|overlay| overlay.slug == "100% Pure")
            .expect("folder with a percent sign and a space is discovered");
        // A bare `%` would be read back as an escape and a raw space is not
        // legal in a `Location` header, so the URL carries neither.
        let url = overlay.url();
        assert_eq!(url, "/overlays/100%25%20Pure/");
        let segment = url
            .strip_prefix(OVERLAYS_BASE)
            .and_then(|rest| rest.strip_prefix('/'))
            .and_then(|rest| rest.strip_suffix('/'))
            .expect("the url is one segment under the overlays base");
        assert_eq!(percent_decode(segment), "100% Pure");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn overlay_url_leaves_a_plain_slug_alone() {
        let root = temp_root("url-plain");
        write(&root, "Alpha/index.html", "<html></html>");

        let overlays = discover(&root);
        assert_eq!(overlays[0].url(), "/overlays/Alpha/");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn percent_encode_and_decode_round_trip() {
        for raw in [
            "Alpha",
            "rtosu Example",
            "100% Pure",
            "a+b",
            "a#b",
            "a?b",
            "a&b",
            "Café Münster",
            "",
        ] {
            let encoded = percent_encode(raw);
            assert_eq!(
                percent_decode(&encoded),
                raw,
                "round trip failed for {raw:?}"
            );
            // Nothing outside the unreserved set may survive, or the value is
            // not a legal path segment.
            assert!(
                encoded
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b)),
                "{raw:?} encoded to {encoded:?} with a character RFC 3986 forbids"
            );
        }
    }

    #[test]
    fn percent_encode_escapes_everything_outside_the_unreserved_set() {
        assert_eq!(percent_encode("rtosu Tourney"), "rtosu%20Tourney");
        assert_eq!(percent_encode("100%20Pure"), "100%2520Pure");
        assert_eq!(percent_encode("a+b"), "a%2Bb");
        assert_eq!(percent_encode("a/b"), "a%2Fb");
        assert_eq!(percent_encode("a\\b"), "a%5Cb");
        assert_eq!(percent_encode("a#b"), "a%23b");
        assert_eq!(percent_encode("a?b"), "a%3Fb");
        assert_eq!(percent_encode("a&b"), "a%26b");
        assert_eq!(percent_encode("-._~"), "-._~");
        assert_eq!(percent_encode(""), "");
        // Non-ASCII is escaped byte-wise, as RFC 3986 requires.
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn resolve_rejects_traversal_and_absolute_paths() {
        let root = temp_root("resolve");
        write(&root, "Overlay/index.html", "ok");
        write(&root, "secret.txt", "nope");
        let dir = root.join("Overlay");

        assert!(resolve_within(&dir, "index.html").is_some());
        assert!(resolve_within(&dir, "sub/../index.html").is_none());
        assert!(resolve_within(&dir, "../secret.txt").is_none());
        assert!(resolve_within(&dir, "..%2fsecret.txt").is_none());
        assert!(resolve_within(&dir, "%2e%2e/secret.txt").is_none());
        assert!(resolve_within(&dir, "..\\secret.txt").is_none());
        assert!(resolve_within(&dir, "/etc/passwd").is_none());
        assert!(resolve_within(&dir, "C:/Windows/win.ini").is_none());
        assert!(resolve_within(&dir, "").is_none());
        assert!(resolve_within(&dir, "./").is_none());
        assert!(resolve_within(&dir, "a\0b").is_none());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn resolve_decodes_percent_encoded_names() {
        let root = temp_root("decode");
        write(&root, "Overlay/index.html", "ok");
        let dir = root.join("Overlay");

        assert!(resolve_within(&dir, "my%20file.css").is_none());
        write(&dir, "my file.css", "body{}");
        assert!(resolve_within(&dir, "my%20file.css").is_some());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn shim_is_injected_at_the_top_of_head() {
        let html = "<!DOCTYPE html><html><head><title>x</title></head><body></body></html>";
        let out = inject_shim(html, "/overlays/__rtosu/overlay-shim.js");

        let head = out.find("<head>").unwrap();
        let script = out.find("<script src=").unwrap();
        let title = out.find("<title>").unwrap();
        assert!(
            head < script && script < title,
            "shim must precede overlay scripts"
        );
        assert!(out.ends_with("</html>"));
    }

    #[test]
    fn shim_is_injected_after_html_when_head_is_absent() {
        let html = "<!doctype html>\n<html lang=\"en\">\n<body>hi</body>\n</html>";
        let out = inject_shim(html, "shim.js");

        let html_tag = out.find("<html").unwrap();
        let script = out.find("<script src=").unwrap();
        let body = out.find("<body>").unwrap();
        assert!(html_tag < script && script < body);
    }

    #[test]
    fn shim_is_prefixed_when_the_document_has_no_tags() {
        let out = inject_shim("just text", "shim.js");
        assert!(out.starts_with(r#"<script src="shim.js"></script>"#));
        assert!(out.ends_with("just text"));
    }

    #[test]
    fn shim_does_not_match_a_longer_tag_name() {
        let html = "<html><body><header>x</header></body></html>";
        let out = inject_shim(html, "shim.js");
        // No <head>, so the script belongs just after the <html> open tag, and
        // <header> must not be mistaken for it.
        assert!(out.starts_with(r#"<html><script src="shim.js"></script>"#));
    }

    #[test]
    fn shim_preserves_html_tag_attributes() {
        let out = inject_shim(
            "<!DOCTYPE html><html lang=\"en\"><body></body></html>",
            "shim.js",
        );
        assert!(out.contains(r#"<html lang="en"><script src="shim.js"></script>"#));
    }

    #[test]
    fn shim_is_inserted_past_a_gt_inside_an_attribute() {
        let html = r#"<html data-note="a > b"><body></body></html>"#;
        let out = inject_shim(html, "shim.js");
        assert!(out.contains(r#"<html data-note="a > b"><script src="shim.js"></script>"#));
    }

    #[test]
    fn dashboard_lists_every_overlay_and_escapes_metadata() {
        let root = temp_root("dashboard");
        write(&root, "Evil/index.html", "<html></html>");
        write(
            &root,
            "Evil/metadata.txt",
            "Name: <img src=x onerror=alert(1)>\nAuthor: a & b\n",
        );

        let overlays = discover(&root);
        let html = dashboard_html(&overlays, &root);

        assert!(html.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(!html.contains("<img src=x onerror=alert(1)>"));
        assert!(html.contains("a &amp; b"));
        assert!(html.contains("/overlays/Evil/"));
        assert!(html.contains("1 overlay(s)"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dashboard_explains_an_empty_folder() {
        let root = temp_root("dashboard-empty");
        let html = dashboard_html(&[], &root);
        assert!(html.contains("No overlays found"));
        assert!(html.contains("0 overlay(s)"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dashboard_only_links_http_author_links() {
        let root = temp_root("dashboard-links");
        write(&root, "Linked/index.html", "<html></html>");
        write(
            &root,
            "Linked/metadata.txt",
            "Name: Linked\nauthorLinks: javascript:alert(1)\n",
        );

        let html = dashboard_html(&discover(&root), &root);
        assert!(!html.contains("javascript:"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn content_types_cover_overlay_assets() {
        assert_eq!(
            content_type_for(Path::new("a.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type_for(Path::new("a.JS")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type_for(Path::new("a.woff2")), "font/woff2");
        assert_eq!(content_type_for(Path::new("a.png")), "image/png");
        assert_eq!(content_type_for(Path::new("a.jpg")), "image/jpeg");
        assert_eq!(
            content_type_for(Path::new("a.unknown")),
            "application/octet-stream"
        );
        assert_eq!(
            content_type_for(Path::new("noext")),
            "application/octet-stream"
        );
    }

    #[test]
    fn shim_source_rewrites_the_documented_tosu_routes() {
        let js = OVERLAY_SHIM_JS;
        for needle in [
            "'/ws': '/websocket/v2'",
            "'/websocket/v2/precise'",
            "'/backgroundImage', '/files/beatmap/background'",
            "'/Songs/', '/files/beatmap/'",
            "new Proxy(NativeWebSocket",
        ] {
            assert!(js.contains(needle), "shim must contain {needle}");
        }
    }
}
