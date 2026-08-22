# Third-Party Notices

This document applies to **StreamNook Bluzyrino 8.4.2-beta.1** and the bundled
x86-64 Bluzyrino runtime **2.0.3**.

Bluzyrino is redistributed with permission from its maintainer. That permission
applies to Bluzyrino itself and does not replace or override third-party terms.

## Runtime identity

The release runtime contains the original Bluzyrino 2.0.3 payload plus app-local
runtime dependencies needed for clean-machine operation.

- Entrypoint: `chat-runtime/Bluzyrino.exe`
- Bluzyrino version: `2.0.3`
- Architecture: `x86_64`
- Entrypoint SHA-256:
  `aa4b2101ffab24d271361d1b25c01026d8b61bfcda3e32b08d932262021af6ed`
- Release manifest payload: 39 files
- Release manifest payload size: 78,217,948 bytes

The original 31-file Bluzyrino payload remains unchanged; the additional files
are the two OpenSSL DLLs and six Microsoft Visual C++ runtime DLLs documented
below.

## Moltorino / Chatterino lineage

The Moltorino source snapshot used as provenance evidence contains an MIT
license. Chatterino7 and Chatterino2, from which Moltorino descends, also use
the MIT license.

The retained MIT notice is supplied at:

`licenses/Moltorino-Chatterino-MIT.txt`

Bluzyrino-specific modifications are redistributed with permission from the
Bluzyrino maintainer. No separate formal Bluzyrino license name is asserted.

## Qt 6.9.3

The bundled runtime contains dynamically linked Qt **6.9.3.0** libraries and
plugins. This distribution relies on Qt's **LGPL-3.0-only** option for these
components.

License texts:

- `licenses/Qt-LGPL-3.0.txt`
- `licenses/Qt-GPL-3.0.txt`

The GPLv3 text accompanies LGPLv3 because LGPLv3 incorporates and refers to
GPLv3; its inclusion does not mean this distribution selects Qt's alternative
GPL licensing option for StreamNook Bluzyrino.

`Bluzyrino.exe` directly imports `Qt6Widgets.dll`, `Qt6Svg.dll`,
`Qt6Gui.dll`, `Qt6Network.dll`, and `Qt6Core.dll`. These Qt libraries remain
separate DLL/plugin files. The distribution does not impose a term preventing
replacement of the LGPL-covered Qt DLLs with compatible modified versions or
reverse engineering for debugging such modifications.

Corresponding Qt 6.9.3 source archives are identified in `SOURCES.md`.

## OpenSSL 3.6.3

The release runtime now includes app-local OpenSSL **3.6.3** DLLs built with
vcpkg from the OpenSSL 3.6.3 source release:

- `libssl-3-x64.dll`
  - version `3.6.3`
  - SHA-256
    `889f4e5deb416e8461a591e034fa6bb3377fce314b731cbfe33873067914f56e`
- `libcrypto-3-x64.dll`
  - version `3.6.3`
  - SHA-256
    `8c13a1d313d2b45b8da461524ab45fa31e63b0ced9685d49c3fb49a1f2402d10`

OpenSSL 3.x is distributed under the Apache License 2.0. The license text copied
from the installed vcpkg OpenSSL package is supplied at:

`licenses/OpenSSL-Apache-2.0.txt`

The exact OpenSSL 3.6.3 source archive used for this release is made available
with the release as described in `SOURCES.md`.

## Microsoft Visual C++ runtime

The release runtime includes the following app-local x64 Microsoft Visual C++
runtime DLLs from:

`Visual Studio 18 BuildTools\VC\Redist\MSVC\14.51.36231\x64\Microsoft.VC145.CRT`

All six report file version **14.51.36247.0**:

- `MSVCP140.dll`
- `MSVCP140_1.dll`
- `MSVCP140_2.dll`
- `MSVCP140_ATOMIC_WAIT.dll`
- `VCRUNTIME140.dll`
- `VCRUNTIME140_1.dll`

These are Microsoft redistributable runtime components and are not represented
as open-source software.

## 7-Zip

The runtime includes:

- `support/7zip/7z.exe`
- `support/7zip/7z.dll`
- `support/7zip/License.txt`
- `support/7zip/readme.txt`

The bundled 7-Zip notice states that most 7-Zip code is licensed under the GNU
LGPL version 2.1 or later, with additional BSD-licensed portions and the stated
unRAR restriction for applicable code. The bundled `License.txt` remains with
the binaries.

The complete LGPLv2.1 text is also supplied at:

`licenses/7-Zip-LGPL-2.1.txt`

The PE version metadata of the staged `7z.exe` and `7z.dll` identifies version
**24.09**. The bundled `readme.txt` says 24.08; that original text is preserved
unchanged. 7-Zip 24.09 source is made available with this release as described
in `SOURCES.md`.

## D3Dcompiler

The runtime contains `D3Dcompiler_47.dll`:

- Version `6.3.9600.16384`
- SHA-256
  `e994847e01a6f1e4cbdc5a864616ac262f67ee4f14db194984661a8d927ab7f4`

It is a Microsoft binary and is not represented here as open-source software.

## Additional embedded third-party notices

`Bluzyrino.exe` contains notice/resource references for additional projects,
including Boost, fmt, Lua, QtKeychain, RapidJSON, semver, and others. Their
exact versions and linkage relationships have not all been independently
reconstructed. No unsupported version or license claim is made here beyond the
components specifically identified above.
