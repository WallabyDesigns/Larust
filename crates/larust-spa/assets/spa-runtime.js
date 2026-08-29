// Larust SPA-navigation client runtime. No build step, no npm, no CDN —
// vendored in full and served at GET /__larust_spa/runtime.js, version-
// locked to the installed larust-spa crate.
//
// Unlike wire-runtime.js/push-runtime.js (larust-live), there is no server-
// side rendering path here at all: every navigation fetches the exact same
// full HTML page an ordinary request would get, and this script extracts
// what changed via DOMParser — no fragment endpoint, no content
// negotiation. See docs/MACROS.md's `@spa` section for the full design and
// its stated v1 limitations.
//
// v1 scope: intercepts same-origin <a href> clicks and <form> submits
// (GET and mutating alike), swapping the single #__larust_spa_root region
// wholesale (a plain innerHTML replace, not a diff-patch — deliberately
// not wire-runtime.js's own positional patcher, which is built for one
// structurally-stable component subtree, not two unrelated pages) and
// updating <title> + browser history. Explicitly out of scope for v1: any
// <head> reconciliation beyond <title>, multipart/form-data (file upload)
// forms (always a real, native submission), prefetching, scroll-position
// memory, and — since no browser/JS test harness exists anywhere in this
// codebase (confirmed: no Playwright, no headless-browser setup, nothing
// under crates/larust-cli/src/dev.rs) — any automated test coverage at
// all; this is verified only by manual/E2E testing against a real browser,
// the same as wire-runtime.js/push-runtime.js are today.
//
// Two extension points for page-specific JS, dispatched on `document`
// around every swap (see `swapInto`):
//   - `larust:spa:navigating` — fires just BEFORE the old content is
//     replaced. This is the only chance a widget initialized on the
//     outgoing content (a map, a chart, anything holding a live reference
//     to a DOM node about to be destroyed) gets to tear itself down —
//     otherwise its listeners on `window`/`document` (as opposed to
//     listeners on its own now-discarded element, which just vanish) leak
//     silently across every navigation.
//   - `larust:spa:navigated` — fires just AFTER the new content lands,
//     for anything that needs to (re)initialize against it.
// Ordinary `<script>` tags inside the swapped region ARE re-executed on
// every swap (see `executeScripts` below) — plain `.innerHTML` assignment
// alone would leave them inert (standard, spec'd behavior), so this file
// explicitly clones each one into a fresh element to force it to run,
// same technique Turbo/htmx use. A `src` script is only fetched and run
// once per unique absolute URL for the lifetime of the page — the first
// time it's encountered, whether that's the initial document or a later
// swap — so a shared library tag repeated on every page's content region
// doesn't get re-downloaded and re-executed on every navigation. Inline
// scripts have no such identity to dedupe on and are re-run every time
// they appear, matching the intent of putting page-specific init code
// directly in the page's own content.
(function () {
    "use strict";

    var SPA_ROOT_ID = "__larust_spa_root";
    // Escape hatch: place on an <a>/<form> itself, or any ancestor
    // container, to blanket-exclude a whole subtree (a nav widget, a
    // third-party embed) from interception with one mechanism — same
    // "closest() from either the element or a wrapper" spirit as
    // wire-runtime.js's own `wire:ignore`, just a plain `data-` attribute
    // since this isn't tied to the `wire:` namespace at all.
    var IGNORE_ATTR = "data-spa-ignore";

    function getRoot() {
        // Defensive, not expected in practice: the script is only ever
        // emitted (via @larustscripts) for a page whose own resolved tree
        // actually uses @spa, so the id should always be present. Guards
        // against a stale cached script running on a page that changed.
        return document.getElementById(SPA_ROOT_ID);
    }

    // Every absolute `src` URL that has ever been fetched-and-run once,
    // across the page's whole lifetime (seeded from the initial document,
    // grown by every swap thereafter) — never reset, so a library tag
    // repeated on every page's content region only ever loads once.
    var loadedScriptSrcs = new Set();
    Array.prototype.forEach.call(document.querySelectorAll("script[src]"), function (script) {
        try {
            loadedScriptSrcs.add(new URL(script.getAttribute("src"), location.href).href);
        } catch (e) {
            // Unparseable src (rare) — nothing to dedupe on, leave it out.
        }
    });

    // Replaces every <script> under `root` with a freshly created element
    // carrying the same attributes/content, which — unlike the inert
    // <script> elements `.innerHTML` just inserted — the browser actually
    // executes. See this file's header comment for the dedupe rule.
    function executeScripts(root) {
        Array.prototype.forEach.call(root.querySelectorAll("script"), function (old) {
            var src = old.getAttribute("src");
            if (src) {
                var absolute;
                try {
                    absolute = new URL(src, location.href).href;
                } catch (e) {
                    absolute = src;
                }
                if (loadedScriptSrcs.has(absolute)) return;
                loadedScriptSrcs.add(absolute);
            }

            var fresh = document.createElement("script");
            Array.prototype.forEach.call(old.attributes, function (attr) {
                fresh.setAttribute(attr.name, attr.value);
            });
            if (!src) {
                fresh.textContent = old.textContent;
            }
            old.replaceWith(fresh);
        });
    }

    function isSameOrigin(url) {
        try {
            return new URL(url, location.href).origin === location.origin;
        } catch (e) {
            return false;
        }
    }

    // Returns false (let the browser handle it natively) for every case
    // native navigation semantics should win: modifier-clicks, a non-
    // primary mouse button, an explicit target, a download link, an
    // opted-out subtree, a cross-origin link, or a pure in-page
    // "#fragment" scroll link (no reason to intercept those at all).
    function shouldInterceptLink(anchor, event) {
        if (event.defaultPrevented) return false;
        if (event.button !== 0) return false;
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return false;
        if (anchor.target && anchor.target !== "_self") return false;
        if (anchor.hasAttribute("download")) return false;
        if (anchor.closest("[" + IGNORE_ATTR + "]")) return false;

        var href = anchor.getAttribute("href") || "";
        if (!href || href.charAt(0) === "#") return false;
        if (!isSameOrigin(anchor.href)) return false;

        var current = new URL(location.href);
        var target = new URL(anchor.href, location.href);
        if (target.pathname === current.pathname && target.search === current.search && target.hash) {
            // Same page, only the hash differs — a real in-page anchor
            // link; let the browser's native scroll-to-anchor run.
            return false;
        }

        return true;
    }

    function shouldInterceptForm(form) {
        if (form.closest("[" + IGNORE_ATTR + "]")) return false;
        // File uploads: always a real, native submission — see this
        // file's own header comment for why this is an explicit, accepted
        // v1 exclusion rather than an oversight.
        if ((form.enctype || "").toLowerCase() === "multipart/form-data") return false;
        if (!isSameOrigin(form.action || location.href)) return false;
        return true;
    }

    // Builds the { url, init } fetch() call for a form submission.
    // Critically serializes via URLSearchParams, not a raw FormData: fetch
    // sends a FormData body as multipart/form-data, which would silently
    // break both CSRF verification (crates/larust-http/src/csrf.rs checks
    // an application/x-www-form-urlencoded body field, same as a native
    // browser form submit already does) and ordinary FormRequest field
    // decoding for every intercepted, non-upload form.
    function buildFormRequest(form, submitter) {
        var method = (form.getAttribute("method") || "GET").toUpperCase();
        var formData = new FormData(form);
        if (submitter && submitter.name) {
            formData.append(submitter.name, submitter.value);
        }
        var params = new URLSearchParams(formData);

        if (method === "GET") {
            var url = new URL(form.action || location.href, location.href);
            url.search = params.toString();
            return { url: url.toString(), init: { method: "GET", redirect: "follow" } };
        }

        return {
            url: form.action || location.href,
            init: { method: method, body: params, redirect: "follow" },
        };
    }

    // Shared by both the link and form success paths. `finalUrl` is the
    // fetch response's own post-redirect URL, not the originally-clicked
    // href/action — this is what makes the address bar/back-button
    // correct for Laravel/Larust's own redirect-after-POST pattern
    // (including "validation failed, redirected back with flashed
    // errors," which needs no special-casing since it's structurally
    // identical to a success redirect, just different final HTML).
    function swapInto(finalUrl, html, pushHistory) {
        var doc = new DOMParser().parseFromString(html, "text/html");
        var newRoot = doc.getElementById(SPA_ROOT_ID);
        var root = getRoot();
        if (!newRoot || !root) {
            // The final URL landed somewhere not using @spa at all (or
            // this page's own root vanished) — nothing safe to swap into;
            // fall back to a real navigation rather than guessing.
            location.href = finalUrl;
            return;
        }

        document.dispatchEvent(
            new CustomEvent("larust:spa:navigating", { detail: { url: finalUrl } })
        );

        document.title = doc.title;
        root.innerHTML = newRoot.innerHTML;
        executeScripts(root);

        if (pushHistory) {
            history.pushState({ __larustSpa: true }, "", finalUrl);
            window.scrollTo(0, 0);
        }

        document.dispatchEvent(
            new CustomEvent("larust:spa:navigated", { detail: { url: finalUrl } })
        );
    }

    // GET-only path, used for both link clicks and popstate (with a
    // different `onFailure` in each case — see `onPopState` below for why
    // it can't just reuse this function's own default). Any network error
    // or non-2xx *final* status (fetch already followed redirects) falls
    // back to a real browser navigation — safe here specifically because
    // GET has no mutation side effect to risk repeating.
    function navigate(url, pushHistory, onFailure) {
        var fail = onFailure || function () { location.href = url; };
        fetch(url, { redirect: "follow" })
            .then(function (response) {
                if (!response.ok) {
                    fail();
                    return null;
                }
                return response.text().then(function (html) {
                    swapInto(response.url, html, pushHistory);
                });
            })
            .catch(fail);
    }

    // Mutating-form path. Per this feature's own accepted v1 tradeoff: a
    // non-2xx final response falls back to a real, native resubmission
    // (form.submit(), which does not re-fire the `submit` listener, so it
    // can't loop) — for a POST/PUT/PATCH/DELETE form specifically, this
    // carries a small, accepted risk of double-submitting if the original
    // fetch attempt had already partially succeeded server-side before
    // returning an error status. The common validation-failure case never
    // reaches this branch at all (it's a redirect -> 200, handled by the
    // normal success path above).
    function submitForm(form, submitter) {
        var built = buildFormRequest(form, submitter);
        fetch(built.url, built.init)
            .then(function (response) {
                if (!response.ok) {
                    form.submit();
                    return null;
                }
                return response.text().then(function (html) {
                    swapInto(response.url, html, true);
                });
            })
            .catch(function () {
                form.submit();
            });
    }

    function onDocumentClick(event) {
        var anchor = event.target.closest("a[href]");
        if (!anchor) return;
        if (!shouldInterceptLink(anchor, event)) return;
        event.preventDefault();
        navigate(anchor.href, true);
    }

    function onDocumentSubmit(event) {
        var form = event.target;
        if (!(form instanceof HTMLFormElement)) return;
        if (!shouldInterceptForm(form)) return;
        event.preventDefault();
        submitForm(form, event.submitter);
    }

    function onPopState() {
        // The browser has already moved the history pointer — re-fetch and
        // swap without pushing a new entry (`pushHistory: false`). On
        // failure, reload rather than `navigate`'s own default
        // `location.href = url` fallback: the pointer already moved, so
        // re-setting `location.href` to the same URL wouldn't reliably
        // force a fresh navigation the way `location.reload()` does.
        navigate(location.href, false, function () {
            location.reload();
        });
    }

    document.addEventListener("click", onDocumentClick);
    document.addEventListener("submit", onDocumentSubmit);
    window.addEventListener("popstate", onPopState);

    // State-shape consistency for the very first back-navigation, so an
    // entry pushed by this script and the page's own initial load look the
    // same to `popstate`.
    history.replaceState({ __larustSpa: true }, "", location.href);
})();
