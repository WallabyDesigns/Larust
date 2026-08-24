// Larust Reverb client runtime. No build step, no npm, no CDN — vendored
// in full and served at GET /__larust_reverb/runtime.js, version-locked
// to the installed larust-reverb crate. Unlike push-runtime.js (which
// patches a [data-live-channel] element's DOM with server-pushed HTML),
// this is a plain named-event dispatcher — nothing here touches the DOM
// at all, since a Reverb channel carries arbitrary JSON, not markup.
//
// Usage:
//   LarustReverb.channel("orders.42").listen("OrderShipped", function (data) {
//     ...
//   });
(function () {
    "use strict";

    var RECONNECT_DELAY_MS = 2000;

    // One entry per channel name, so calling .channel(name) twice reuses
    // the same WebSocket and listener registry instead of opening a
    // second redundant connection.
    var channels = {};

    function channel(name) {
        if (channels[name]) return channels[name];

        var listeners = {};
        var api = {
            listen: function (eventName, callback) {
                (listeners[eventName] = listeners[eventName] || []).push(callback);
                return api;
            }
        };
        channels[name] = api;
        connect(name, listeners);
        return api;
    }

    function connect(name, listeners) {
        var url =
            (location.protocol === "https:" ? "wss://" : "ws://") +
            location.host +
            "/__larust_reverb/" +
            encodeURIComponent(name);
        var socket = new WebSocket(url);

        socket.addEventListener("message", function (event) {
            var envelope;
            try {
                envelope = JSON.parse(event.data);
            } catch (error) {
                return;
            }
            var callbacks = listeners[envelope.event];
            if (!callbacks) return;
            for (var i = 0; i < callbacks.length; i++) {
                callbacks[i](envelope.data);
            }
        });

        // Same fixed-delay reconnect as push-runtime.js — "eventually
        // consistent again" matters more here than minimizing reconnect
        // traffic for what's meant to be a small number of concurrently
        // open channels per page.
        socket.addEventListener("close", function () {
            setTimeout(function () {
                connect(name, listeners);
            }, RECONNECT_DELAY_MS);
        });
    }

    window.LarustReverb = { channel: channel };
})();
