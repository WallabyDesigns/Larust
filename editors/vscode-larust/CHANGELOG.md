# Changelog

## 0.1.4

- No grammar/behavior changes - republished to pick up the Marketplace
  listing metadata (icon, repository link, corrected README install
  instructions, this changelog) that landed locally just after `0.1.3`
  was originally published.

## 0.1.3

- Attribute values now color by what they actually are: numbers
  (`width="1200"`), booleans (`priority="true"`), and strings each get
  their own distinct color instead of one flat tone.
- `@globals`/`persist` variable names (e.g. `title` in `title = expr`)
  are now colored.
- Hyphenated attributes (`data-count`, `aria-label`) are now covered by
  the same attribute-value coloring as everything else.

## 0.1.2

- Fixed single-quoted strings in directive arguments (e.g.
  `@extends('layouts.app')`) being misread as Rust lifetimes and
  rendering broken/partially unscoped.

## 0.1.1

- Fixed a red/invalid-looking opening paren on `@extends(...)`.
- Attribute values now always get a real string color, independent of
  the surrounding tag.
- Directive keywords are now split into two colors: block directives
  (`@if`/`@foreach`/`@section`/...) vs. simple ones (`@extends`/`@yield`/...).

## 0.1.0

- Initial release: syntax highlighting and editor-level indentation for
  `.blade.xr` templates - directives, `{{ }}`/`{!! !!}` interpolation,
  `<Image>`/`<wire:>`/`<resource:>` tags, `:dynamic=` attributes, and
  embedded Rust highlighting throughout.
