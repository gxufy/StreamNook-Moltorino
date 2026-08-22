# Source Availability

This document describes source materials prepared for open-source components
redistributed with **StreamNook Bluzyrino 8.4.2-beta.1**.

The release source bundle is prepared under:

`C:\Dev\StreamNook-Bluzyrino-Release-Sources`

Upload these source archives alongside the public binary release so the
distributor controls their availability.

## Qt 6.9.3

Prepared source archives:

- `qtbase-everywhere-src-6.9.3.tar.xz`
  SHA-256:
  `c5a1a2f660356ec081febfa782998ae5ddbc5925117e64f50e4be9cd45b8dc6e`
- `qtsvg-everywhere-src-6.9.3.tar.xz`
  SHA-256:
  `db76aa3358cbbe6fce7da576ff4669cb9801920188c750d3b12783bbe97026e2`
- `qtimageformats-everywhere-src-6.9.3.tar.xz`
  SHA-256:
  `4fb26bdbfbd4b8e480087896514e11c33aba7b6b39246547355ea340c4572ffe`

These were obtained from Qt's official 6.9.3 source distribution.

## OpenSSL 3.6.3

The release bundles app-local `libssl-3-x64.dll` and `libcrypto-3-x64.dll`
built by vcpkg from OpenSSL 3.6.3.

Prepared source archive:

- `openssl-3.6.3-source.tar.gz`
  SHA-256:
  `c5524dd6bfaa8e8ff0f1be885c390d14f3ff0bd2de62a7311b65fcbb75cb7546`

The archive was acquired by vcpkg from the official OpenSSL 3.6.3 source tag.

## 7-Zip 24.09

Prepared source archive:

- `7zip-24.09-source.zip`
  SHA-256:
  `9b10be21cbb7b29d89feeb4ccab23cd629eb12d49174ef530de47bd8b381799c`

The source archive corresponds to the public 7-Zip tag/release 24.09.

## Moltorino / Chatterino

The Moltorino/Chatterino lineage is MIT-licensed. The retained MIT notice is
distributed at:

`licenses/Moltorino-Chatterino-MIT.txt`

Bluzyrino-specific modifications are redistributed with permission from the
Bluzyrino maintainer.

## Microsoft Visual C++ runtime

The Microsoft VC runtime DLLs bundled app-local with the release are
redistributable Microsoft components, not open-source components, so no
corresponding-source archive is offered for them.

## Release source checksums

`SHA256SUMS.txt` in the release-source directory contains checksums for all five
prepared source archives:

- Qt Base 6.9.3
- Qt SVG 6.9.3
- Qt Image Formats 6.9.3
- 7-Zip 24.09
- OpenSSL 3.6.3
