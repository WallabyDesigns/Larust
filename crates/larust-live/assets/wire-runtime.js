// Larust reactive-component client runtime. No build step, no npm, no CDN -
// vendored in full and served at GET /__larust_wire/runtime.js, version-
// locked to the installed larust-live crate.
//
// v1 scope: wire:model (deferred - sent only when another trigger fires),
// wire:model.live (immediate, 150ms debounced), wire:click="action" and
// wire:submit="action" (no arguments). Explicitly out of scope for v1:
// .lazy/.throttle/custom debounce values, .number/.boolean coercion,
// action arguments.
(function () {
    "use strict";

    var DEBOUNCE_MS = 150;
    var inFlight = Object.create(null); // component id -> { promise, resyncPending }

    function csrfToken() {
        var meta = document.querySelector('meta[name="csrf-token"]');
        return meta ? meta.content : "";
    }

    function findRoot(el) {
        return el.closest("[data-wire-id]");
    }

    // Every sync sends the *entire* current wire:model/wire:model.live
    // field set for the component, not just whatever triggered it - this
    // is what correctly threads a deferred field's just-typed value
    // through when a different element (a click, or another field's live
    // sync) is what actually fires the request.
    function collectProps(root) {
        var props = {};
        root.querySelectorAll("[wire\\:model], [wire\\:model\\.live]").forEach(function (el) {
            var name = el.getAttribute("wire:model") || el.getAttribute("wire:model.live");
            if (!name) return;
            if (el.type === "checkbox") {
                props[name] = el.checked;
            } else {
                props[name] = el.value;
            }
        });
        return props;
    }

    function sync(root, action) {
        var id = root.getAttribute("data-wire-id");
        var body = JSON.stringify({
            props: collectProps(root),
            action: action || null,
        });

        var existing = inFlight[id];
        if (existing) {
            // A request for this component is already in flight - don't
            // fire a second, concurrent one (an older response could
            // otherwise land after and clobber a newer edit). Remember to
            // resync once the in-flight one settles instead.
            existing.resyncPending = { action: action || null };
            return;
        }

        var entry = { resyncPending: null };
        inFlight[id] = entry;

        entry.promise = fetch("/__larust_wire/" + encodeURIComponent(id), {
            method: "POST",
            headers: {
                "Content-Type": "application/json",
                "X-CSRF-TOKEN": csrfToken(),
            },
            body: body,
        })
            .then(function (response) {
                // An action's `Ok(Some(path))` (Livewire's `redirect()`)
                // arrives as a response header, checked before the body is
                // ever read - navigating away makes patching the current
                // fragment moot, and (unlike the body) a header is always
                // present even on this component's very last response.
                var redirect = response.headers.get("X-Wire-Redirect");
                if (redirect) {
                    window.location.href = redirect;
                    return null;
                }
                if (!response.ok) return null;
                return response.text();
            })
            .then(function (html) {
                if (html) applyFragment(root, html);
            })
            .catch(function (error) {
                // eslint-disable-next-line no-console
                console.error("larust-wire sync failed", error);
            })
            .finally(function () {
                delete inFlight[id];
                if (entry.resyncPending) {
                    sync(root, entry.resyncPending.action);
                }
            });
    }

    function applyFragment(root, html) {
        var template = document.createElement("template");
        template.innerHTML = html.trim();
        var newRoot = template.content.firstElementChild;
        if (!newRoot) return;
        larustWirePatch(root, newRoot);
    }

    // The vendored DOM patcher - deliberately not a general morphdom port.
    // Scope: attribute + text-node diffing, children matched by position +
    // tag (+ id when present on both sides), no keyed-list reordering (a
    // component's own re-render is a structurally-stable subtree, not a
    // general list-diffing target). This is what avoids the naive-
    // innerHTML-replace bug of destroying focus/cursor position on the
    // input the user is actively typing into: an attribute/property is
    // only written when its value actually differs from what's already
    // there, and the server always echoes back the same value the client
    // just sent for a wire:model field, so the active input's own value
    // never gets rewritten mid-edit.
    //
    // `wire:ignore` (same attribute name/meaning as real Livewire) opts an
    // element's entire subtree out of patching, full stop - needed for any
    // element a *different* piece of JS manages after mount (a rich-text
    // editor like Trix being the canonical case: it builds its own real
    // DOM children - an internal contenteditable surface - that never
    // exist in the server-rendered HTML, which only ever contains the
    // empty `<trix-editor>` tag itself. Without this, every re-render's
    // child-diff would see those Trix-owned children as extra nodes not
    // present in the fresh HTML and delete them, wiping out the editor's
    // visible content on every single sync.
    function larustWirePatch(oldEl, newEl) {
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
        if (
            oldChild.tagName !== newChild.tagName ||
            oldChild.id !== newChild.id
        ) {
            parent.replaceChild(newChild, oldChild);
            return;
        }
        larustWirePatch(oldChild, newChild);
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

        // Form controls: browsers don't always reflect an attribute change
        // onto the live JS property for a user-edited control, so sync the
        // property too - only when it actually differs, for the same
        // cursor/focus-preservation reason as the attribute diff above.
        //
        // Never overwrite the *currently focused* control's value/checked
        // state, though: a response reflects whatever value that field had
        // at the moment its triggering request was *sent*, not necessarily
        // now - if the user kept typing (a deferred `wire:model` field, or
        // fast typing outrunning the debounce) while this response was in
        // flight, blindly applying a stale echoed-back value here would
        // silently overwrite those newer, not-yet-synced keystrokes. The
        // field's own next sync (or the debounce that's already pending)
        // carries the truly current value forward instead.
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

    function debounce(fn, ms) {
        var timers = new WeakMap();
        return function (el) {
            var existing = timers.get(el);
            if (existing) clearTimeout(existing);
            timers.set(
                el,
                setTimeout(function () {
                    timers.delete(el);
                    fn(el);
                }, ms)
            );
        };
    }

    var debouncedWireSync = debounce(function (el) {
        var root = findRoot(el);
        if (root) sync(root);
    }, DEBOUNCE_MS);

    document.addEventListener("input", function (event) {
        var el = event.target;
        if (!el || !el.hasAttribute) return;
        if (el.hasAttribute("wire:model.live")) {
            debouncedWireSync(el);
        }
        // Plain wire:model is deferred - collectProps() reads its current
        // value from the DOM whenever some other trigger fires a sync, but
        // typing into it never triggers one on its own.
    });

    document.addEventListener("click", function (event) {
        var el = event.target.closest("[wire\\:click]");
        if (!el) return;
        var root = findRoot(el);
        if (!root) return;
        event.preventDefault();
        sync(root, { name: el.getAttribute("wire:click"), args: null });
    });

    document.addEventListener("submit", function (event) {
        var form = event.target;
        if (!form || !form.hasAttribute || !form.hasAttribute("wire:submit")) return;
        var root = findRoot(form);
        if (!root) return;
        event.preventDefault();
        sync(root, { name: form.getAttribute("wire:submit"), args: null });
    });
})();
