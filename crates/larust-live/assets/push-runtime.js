// Larust server-push client runtime. No build step, no npm, no CDN -
// vendored in full and served at GET /__larust_push/runtime.js, version-
// locked to the installed larust-live crate. Companion to wire-runtime.js:
// that one is client-*initiated* (a user action triggers a sync back to
// the server); this one is server-*initiated* (a WebSocket push swaps in
// new HTML with nobody in this tab doing anything at all). The two never
// fight over the same element - wire:model/wire:click only ever live
// inside a [data-wire-id] subtree, and [data-live-channel] is a wholly
// separate top-level marker.
(function () {
    "use strict";

    var RECONNECT_DELAY_MS = 2000;

    function connect(root) {
        var channel = root.getAttribute("data-live-channel");
        var url =
            (location.protocol === "https:" ? "wss://" : "ws://") +
            location.host +
            "/__larust_push/" +
            encodeURIComponent(channel);
        var socket = new WebSocket(url);

        socket.addEventListener("message", function (event) {
            applyFragment(root, event.data);
        });

        // A closed socket (server restart, network blip, a backgrounded
        // tab losing its connection) reconnects on a fixed delay rather
        // than giving up - "eventually consistent again" matters more
        // here than minimizing reconnect traffic for what's meant to be a
        // small number of concurrently open channels per page.
        socket.addEventListener("close", function () {
            setTimeout(function () {
                connect(root);
            }, RECONNECT_DELAY_MS);
        });
    }

    function applyFragment(root, html) {
        var template = document.createElement("template");
        template.innerHTML = html.trim();
        var newRoot = template.content.firstElementChild;
        if (!newRoot) return;
        larustPushPatch(root, newRoot);
    }

    // Same vendored DOM patcher *shape* as wire-runtime.js's own
    // larustWirePatch - duplicated, not shared, since these are two
    // independently-served vendored files with no bundler/module system
    // to share code through. See that file's own doc comment for the full
    // design reasoning (attribute + text-node diffing, position-matched
    // children, wire:ignore support for elements a *different* piece of
    // JS manages after mount).
    function larustPushPatch(oldEl, newEl) {
        if (isIgnored(oldEl)) return;
        if (oldEl.tagName !== newEl.tagName) {
            oldEl.replaceWith(newEl);
            return;
        }

        patchAttributes(oldEl, newEl);

        var oldChildren = Array.prototype.slice.call(oldEl.childNodes);
        var newChildren = Array.prototype.slice.call(newEl.childNodes);
        var max = Math.max(oldChildren.length, newChildren.length);

        for (var i = 0; i < max; i++) {
            var oldChild = oldChildren[i];
            var newChild = newChildren[i];

            if (oldChild && isIgnored(oldChild)) continue;

            if (oldChild && !newChild) {
                oldEl.removeChild(oldChild);
                continue;
            }
            if (!oldChild && newChild) {
                oldEl.appendChild(newChild);
                continue;
            }

            patchNode(oldEl, oldChild, newChild);
        }
    }

    function isIgnored(node) {
        return (
            node.nodeType === Node.ELEMENT_NODE &&
            node.hasAttribute &&
            node.hasAttribute("wire:ignore")
        );
    }

    function patchNode(parent, oldChild, newChild) {
        if (oldChild.nodeType !== newChild.nodeType) {
            parent.replaceChild(newChild, oldChild);
            return;
        }
        if (oldChild.nodeType === Node.TEXT_NODE) {
            if (oldChild.data !== newChild.data) oldChild.data = newChild.data;
            return;
        }
        if (oldChild.nodeType !== Node.ELEMENT_NODE) {
            parent.replaceChild(newChild, oldChild);
            return;
        }
        if (oldChild.tagName !== newChild.tagName || oldChild.id !== newChild.id) {
            parent.replaceChild(newChild, oldChild);
            return;
        }
        larustPushPatch(oldChild, newChild);
    }

    function patchAttributes(oldEl, newEl) {
        var oldAttrs = oldEl.attributes;
        var newAttrs = newEl.attributes;

        for (var i = oldAttrs.length - 1; i >= 0; i--) {
            var name = oldAttrs[i].name;
            if (!newEl.hasAttribute(name)) oldEl.removeAttribute(name);
        }
        for (var j = 0; j < newAttrs.length; j++) {
            var attr = newAttrs[j];
            if (oldEl.getAttribute(attr.name) !== attr.value) {
                oldEl.setAttribute(attr.name, attr.value);
            }
        }

        var isEditingThis =
            typeof document !== "undefined" && document.activeElement === oldEl;
        if (!isEditingThis) {
            if ("value" in newEl && oldEl.value !== newEl.value) {
                oldEl.value = newEl.value;
            }
            if ("checked" in newEl && oldEl.checked !== newEl.checked) {
                oldEl.checked = newEl.checked;
            }
        }
    }

    document.querySelectorAll("[data-live-channel]").forEach(connect);
})();
