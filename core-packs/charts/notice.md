charts pack: third-party notices

The charts plugin embeds one font family, used to measure and draw all text.
The files are not modified: they are the published TrueType files, byte for
byte, from the 2.1.5 release.

LiberationSans-Regular.ttf
  Source:  https://github.com/liberationfonts/liberation-fonts/files/7261482/liberation-fonts-ttf-2.1.5.tar.gz
  SHA-256: 76d04c18ea243f426b7de1f3ad208e927008f961dc5945e5aad352d0dfde8ee8
  Author:  Copyright (c) 2012 Red Hat, Inc. with Reserved Font Name Liberation; digitized
           data copyright (c) 2010 Google Corporation with Reserved Font Arimo, Tinos and
           Cousine (original design by Steve Matteson, Ascender, Inc.)
  Licence: SIL Open Font License, Version 1.1 (OFL-1.1), https://openfontlicense.org/

LiberationSans-Bold.ttf
  Source:  the same archive.
  SHA-256: 788abee4c806d660e8aee46689dd8540cd4bb98da03dcc9d171ce3efd99a9173
  Author:  as above.
  Licence: OFL-1.1, as above.

The full licence text is committed next to the fonts, in
plugins/charts/fonts/LICENSE. The OFL allows embedding and redistributing the
fonts in software with this notice kept; the fonts may not be sold on their
own. The plugin code around them is separate and is licensed as the rest of
this repository; the `.afb` artifacts embed the fonts, so redistributing an
artifact carries this notice for them.

The plugins link these Rust crates, each under its own licence: resvg and usvg
(MPL-2.0), tiny-skia and tiny-skia-path (BSD-3-Clause), rustybuzz, ttf-parser,
fontdb, png, serde, serde_json, base64 and their dependencies (MIT, Apache-2.0
or both). Run `cargo tree` in plugins/charts for the full list. The palette
for the series colours is the colour-blind-safe set of Okabe and Ito (2008);
the sequential ramp samples viridis (CC0, van der Walt and Smith); the
diverging ramp follows ColorBrewer's RdBu (Apache-2.0, Cynthia Brewer). No
colour data is fetched or embedded beyond those few values.
