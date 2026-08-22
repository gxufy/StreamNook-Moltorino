# Distribution Notes

## Scope

These notes apply to **StreamNook Bluzyrino 8.4.2-beta.1** and its bundled
Bluzyrino runtime **2.0.3**.

Bluzyrino is redistributed with permission from its maintainer. Third-party
components remain subject to their own licenses and notices.

## Package identity

The release runtime is pinned to:

- `Bluzyrino.exe` version `2.0.3`
- x86-64 architecture
- Bluzyrino.exe SHA-256
  `aa4b2101ffab24d271361d1b25c01026d8b61bfcda3e32b08d932262021af6ed`
- 39 manifest payload files
- 78,217,948 manifest payload bytes

The original 31-file Bluzyrino payload remains byte-for-byte unchanged. The
release runtime adds only:

- OpenSSL 3.6.3:
  - `libssl-3-x64.dll`
  - `libcrypto-3-x64.dll`
- Microsoft VC runtime 14.51.36247.0:
  - `MSVCP140.dll`
  - `MSVCP140_1.dll`
  - `MSVCP140_2.dll`
  - `MSVCP140_ATOMIC_WAIT.dll`
  - `VCRUNTIME140.dll`
  - `VCRUNTIME140_1.dll`

## Compliance files accompanying the application

The final portable and installed distributions should contain, beside the
application:

- `THIRD_PARTY_NOTICES.md`
- `DISTRIBUTION_NOTES.md`
- `SOURCES.md`
- `licenses/Qt-LGPL-3.0.txt`
- `licenses/Qt-GPL-3.0.txt`
- `licenses/7-Zip-LGPL-2.1.txt`
- `licenses/Moltorino-Chatterino-MIT.txt`
- `licenses/OpenSSL-Apache-2.0.txt`

The existing `chat-runtime/support/7zip/License.txt` remains inside the
manifest-controlled runtime.

## Corresponding-source package

The release source directory:

`C:\Dev\StreamNook-Bluzyrino-Release-Sources`

contains:

- Qt Base 6.9.3 source
- Qt SVG 6.9.3 source
- Qt Image Formats 6.9.3 source
- 7-Zip 24.09 source
- OpenSSL 3.6.3 source
- `SHA256SUMS.txt`

Upload those source files alongside the public binary release.

## Dynamic Qt replacement

Bluzyrino dynamically links the separately shipped Qt DLLs. The packaging must
not prevent recipients from replacing the LGPL-covered Qt libraries with
compatible modified versions, and no release term should prohibit reverse
engineering for debugging such modifications.

## App-local runtime dependencies

OpenSSL and the Microsoft Visual C++ runtime are now bundled app-local in
`chat-runtime`. The release no longer relies on the development machine's
System32 OpenSSL installation or a separately installed VC runtime for those
DLL names.

## Final pre-release checklist

1. Run the compliance-aware package script using:
   `-RuntimeRoot C:\Dev\Bluzyrino_release_staged`
2. Confirm the 39-file runtime validates with zero hash/size mismatches.
3. Confirm all eight compliance/license files above are present in the portable
   artifact and installed application directory.
4. Confirm portable ZIP and installer launch and preserve tested behavior.
5. Test the rebuilt installer and Bluzyrino on a fresh Windows Sandbox.
6. Confirm there are no missing OpenSSL or VC runtime DLL errors.
7. Upload the five prepared source archives and `SHA256SUMS.txt` alongside the
   binary release.
8. Recompute and publish final installer/portable SHA-256 checksums.
9. Only then create/publish the public release.
