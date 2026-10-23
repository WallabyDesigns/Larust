---
title: Digging Deeper
nav_order: 7
has_children: true
---

# Digging Deeper

Everything an app tends to need beyond routing, models, and templates.
See [Optional Features](../optional-features/) for the opt-in Tier-1
shim crates (Cashier, Sanctum, Permissions, Shield, Reverb, Socialite,
Pennant, Scout, Passport, Db, Sitemap) - each has its own page there.

1. [Authentication & Authorization](authentication-and-authorization) - sessions, guards, policies
2. [Mail & Notifications](mail-and-notifications)
3. [Events, Queues, Scheduling & Commands](events-queues-and-scheduling)
4. [File Storage & Cache](file-storage-and-cache)
5. [Reactive Components](reactive-components) - `@wire(...)`, Livewire-shaped
6. [Image Optimization](image-optimization) - `<Image ...>`, Next.js-shaped
7. [Realtime & Broadcasting](realtime-and-broadcasting) - `@live(...)`, SPA mode
8. [Plugins](plugins) - how a crate contributes routes to your app's router
9. [Localization](localization) - `t(...)`/`resources/lang/`, locale negotiation
