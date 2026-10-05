# Third-party licenses

ArcThumb itself is distributed under **MIT OR Apache-2.0** (see
`LICENSE-MIT` and `LICENSE-APACHE`). The following third-party
components are redistributed with ArcThumb (the `arcthumb.dll` shell
extension and/or `arcthumb-config.exe`) and require separate
acknowledgement.

## Slint

[Slint](https://slint.dev/) is used as the GUI toolkit for
`arcthumb-config.exe` under the **Slint Royalty-Free License 2.0**.

Full license text:
https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md

Attribution: ArcThumb satisfies the Slint Royalty-Free License 2.0
attribution requirement by displaying the `AboutSlint` widget inside
the **About** dialog of `arcthumb-config.exe` (reachable via the
**About** button in the settings window). The badge shows the Slint
logo and links back to https://slint.dev/.

Slint's own source is not modified and is linked statically into the
binary via the `slint` crate.

## jxl-rs

`arcthumb.dll` decodes JPEG XL with [jxl-rs](https://github.com/libjxl/jxl-rs)
(the `jxl` and `jxl-image-rs-integration` crates), the pure-Rust
decoder developed within the JPEG XL project and also shipped by
Chrome and Firefox. It is linked statically and unmodified, and is
compiled in by default (the `jxl` Cargo feature).

jxl-rs is licensed under the **BSD 3-Clause License**:

> Copyright (c) the JPEG XL Project Authors.
> All rights reserved.
>
> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions
> are met:
>
> 1. Redistributions of source code must retain the above copyright
>    notice, this list of conditions and the following disclaimer.
> 2. Redistributions in binary form must reproduce the above
>    copyright notice, this list of conditions and the following
>    disclaimer in the documentation and/or other materials provided
>    with the distribution.
> 3. Neither the name of the copyright holder nor the names of its
>    contributors may be used to endorse or promote products derived
>    from this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
> "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
> LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS
> FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE
> COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT,
> INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
> (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
> SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
> HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT,
> STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
> ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED
> OF THE POSSIBILITY OF SUCH DAMAGE.

The full text as shipped by the crate is at
https://github.com/libjxl/jxl-rs/blob/main/LICENSE

## Roboto (font)

`arcthumb.dll` embeds an A–Z / 0–9 subset of **Roboto Bold**
(Copyright 2015 Google Inc.) to draw the format labels in the
identification overlay. Roboto is licensed under the **Apache License
2.0**.

The subset font and a copy of its license live in `assets/fonts/`
(`Roboto-Bold-subset.ttf`, `LICENSE-Roboto.txt`); the subsetting
command is recorded in `assets/fonts/README.md`. Only the glyph data
is reduced — the outlines themselves are unmodified.

---

Other Rust crates used by ArcThumb (both the DLL and
`arcthumb-config.exe`) are redistributed under their respective MIT,
Apache-2.0, BSD, or similarly permissive licenses. Running
`cargo tree --format '{p} {l}'` from the repository root will list
every dependency together with its SPDX license identifier.
