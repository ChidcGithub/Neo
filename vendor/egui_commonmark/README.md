# A commonmark viewer for [egui](https://github.com/emilk/egui)

[![Crate](https://img.shields.io/crates/v/egui_commonmark.svg)](https://crates.io/crates/egui_commonmark)
[![Documentation](https://docs.rs/egui_commonmark/badge.svg)](https://docs.rs/egui_commonmark)

<img src="https://raw.githubusercontent.com/lampsitter/egui_commonmark/master/assets/example-v4.png" alt="showcase" width=280/>

While this crate's main focus is commonmark, it also supports a subset of
Github's markdown syntax: tables, strikethrough, tasklists and footnotes.

## Neo local compatibility patch

Based on egui_commonmark 0.25.0; the upstream MIT/Apache licenses are retained.
The workspace selects this copy through `[patch.crates-io]`.

- Tables: retain column alignment, render text runs together, wrap long cells and constrain wide tables to a local horizontal scroll area. Vertically center using the measured galley height, never a zero-height child. Arbitrary link/math cells remember their measured height and request a new pass only when it changes; they are not rendered twice for measurement. Regression tests compare text centers with striped row bounds, resize/change fonts, grow/shrink callbacks and click the resulting link hitboxes.
- Nested block quotes: consume matched closing tags and use recursive event processing, retaining lists/tables inside quotes.
- Model-authored links: only HTTP(S) and internal heading fragments may navigate. Other schemes remain readable but inert.
- Images: visible alt-text and explicit HTTP(S) links, without automatic network or local-file loading. Attachments and image tools remain the supported image-viewing path.
- Application-side math hook uses actual allocated dimensions instead of zero-width text placeholders.

Application regression tests in `crates/neo-app/src/ui/markdown.rs` cover geometry, streaming updates, link clicks, clipboard copying, headings, nested quotes, and screenshot matrices. Recheck these cases when updating upstream.

## Usage

In Cargo.toml:

```toml
egui_commonmark = "0.25"
# Specify what image formats you want to use
image = { version = "0.25", default-features = false, features = ["png"] }
```

```rust
use egui_commonmark::*;
let markdown =
r"# Hello world

* A list
* [ ] Checkbox
";

let mut cache = CommonMarkCache::default();
CommonMarkViewer::new().show(ui, &mut cache, markdown);
```


## Compile time evaluation of markdown

If you want to embed markdown directly into the binary then you can enable the `macros` feature.
This will do the parsing of the markdown at compile time and output egui widgets.

### Example

```rust
use egui_commonmark::{CommonMarkCache, commonmark};
let mut cache = CommonMarkCache::default();
let _response = commonmark!(ui, &mut cache, "# ATX Heading Level 1");
```

Alternatively you can embed a file

### Example

```rust
use egui_commonmark::{CommonMarkCache, commonmark_str};
let mut cache = CommonMarkCache::default();
commonmark_str!(ui, &mut cache, "content.md");
```


## Features

* `macros`: macros for compile time parsing of markdown
* `better_syntax_highlighting`: Syntax highlighting inside code blocks with
  [`syntect`](https://crates.io/crates/syntect)
* `svg`: Support for viewing svg images
* `fetch`: Images with urls will be downloaded and displayed
* `embedded_image`: Load base64 image data urls from within markdown files


## Examples

For an easy intro check out the `hello_world` example. To see all the different
features egui_commonmark has to offer check out the `book` example.

## FAQ

### URL is not displayed when hovering over a link

By default egui does not show urls when you hover hyperlinks. To enable it,
you can do the following before calling any ui related functions:

```rust
ui.style_mut().url_in_tooltip = true;
```

## MSRV Policy

This crate uses the same MSRV as the latest released egui version.

## License

Licensed under either of

 * Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
