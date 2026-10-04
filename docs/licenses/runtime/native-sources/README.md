# Native source and license supply

This directory supplies notices and Eigen source for Neo's no-TTS native inputs
and the separately bundled ORT 1.30.0 wake DLLs. It is not a
release approval, a producer attestation, or a claim that all native components
have been rebuilt from source. Source availability and binary/source
correspondence are separate questions.

## Recipient source notice — Eigen / MPL-2.0

Eigen contains code covered by the Mozilla Public License 2.0. You may obtain,
modify and redistribute its covered Source Code Form under that license. The
original MPL text and additional Eigen notices are provided here; original
file-level notices are retained in the complete source archives.

The offline companion is **`neo-native-sources.zip`**. A distributor using this
companion must supply it with the binary (same download/release or delivery
media), together with this notice. Inside it:

| Source | Identity and scope |
| --- | --- |
| `sources/eigen-5.0.1.tar.gz` | Complete 1,913-file upstream archive used by the locally recorded Sherpa no-TTS source build. SHA-256 `e9c326dc8c05cd1e044c71f30f1b2e34a6161a3b6ecf445d56b53ff1669e3dec`. All files in the retained local Eigen build-input tree were compared; none differ or are added. |
| `sources/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip` | Complete 1,891-file, unpatched base source. SHA-256 `6a60d76351f97132669daeeb721d6bf14b008101883ad2d687a3201c5c461eb0`. Archive and file identities verified. Both the ORT 1.28.2 static baseline and the official ORT 1.30.0 Windows wheel's baseline vcpkg recipe select this revision; their modification routes differ as described below. |
| `notices/ort-1.28.2-eigen-s390x-build.patch`, `notices/ort-1.28.2-eigen-s390x-build-werror.patch` | Original patches referenced, in this order, by the ORT 1.28.2 static baseline's Eigen recipe. Supplied separately, not applied inside the base ZIP. **Do not apply these to the ORT 1.30.0 Windows wheel's vcpkg Eigen source route.** See [BUILD-MODIFICATIONS.md](BUILD-MODIFICATIONS.md) for the evidence and modification limits. |

Upstream source alternatives:

- Eigen 5.0.1: <https://gitlab.com/libeigen/eigen/-/archive/5.0.1/eigen-5.0.1.tar.gz>
- ORT's default Eigen pin: <https://github.com/eigen-mirror/eigen/archive/1d8b82b0740839c0de7f1242a3585e3390ff5f33/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip>
- ORT 1.28.2 static baseline recipe: <https://github.com/microsoft/onnxruntime/blob/33ca9628233dc8f002435e868d4c2e9f82766ca1/cmake/external/eigen.cmake>
- ORT 1.30.0 Windows wheel baseline vcpkg recipe: <https://github.com/microsoft/onnxruntime/blob/f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7/cmake/vcpkg-ports/eigen3/portfile.cmake>

The local companion removes dependence on an upstream download remaining
available. Merely storing it in a developer's `target/` directory does **not**
deliver it to recipients. Before distributing binaries, make the corresponding
covered source available by reasonable means in a timely manner, at no more
than the cost of distribution, and tell recipients how to obtain it (MPL 3.2).
Keep that access working for the relevant distribution. If choosing a hosted
source supply instead, verify the actual recipient-accessible URL and include
it in the binary's notices; do not substitute a private cache path or a promise
that has not been implemented.

MPL 3.1 and 3.4 govern the covered source and retention of notices. MPL 3.3
permits a Larger Work under other terms while its covered portions remain
subject to MPL. This is not a requirement to publish every native dependency,
Neo's entire source tree, proprietary code in separate files, or every build
log. Other components retain their own license requirements. Full Eigen
archives also contain tests/benchmarks under their own file notices; their
inclusion as source is not a claim that all such code is linked into Neo.

## Three distinct ONNX Runtime identities

1. **Main no-TTS static input:** producer release
   <https://github.com/csukuangfj/onnxruntime-libs/releases/tag/v1.28.2>, asset
   `onnxruntime-win-x64-static_lib-MT-Release-1.28.2.tar.bz2`,
   107,142,443 bytes, SHA-256
   `77c6cc2a419828f450a570851d7d6d2385523c411dbe858f841a4c7338a78881`.
   The producer release API asset digest matches this archive. The library
   reports baseline `33ca96282`; Microsoft's v1.28.2 resolves to
   `33ca9628233dc8f002435e868d4c2e9f82766ca1`.
   [MIT](onnxruntime-1.28.2-LICENSE) and
   [third-party notices](onnxruntime-1.28.2-ThirdPartyNotices.txt) here are
   byte-preserved texts from that upstream baseline, **not** files found inside
   the prebuilt archive or a verified producer component inventory.
2. **Separate bundled wake DLLs — official artifact binding verified:** the
   official PyPI `onnxruntime-1.30.0-cp311-cp311-win_amd64.whl`, 14,309,136 bytes,
   SHA-256 `0edd0145a6e3fce8a1276491dc784d615e3c58bcb952c9b4e5c876d5c6a12ad7`,
   was downloaded and hash-verified, then inspected without installation or execution.
   Its `onnxruntime.dll` (18,446,136 bytes, SHA-256
   `44741724166ad61c8c82885e394793807f2cf5707c83b9e33c2d78d75c6e7650`) and
   `onnxruntime_providers_shared.dll` (21,816 bytes, SHA-256
   `fabb99add45aa1a0386c80bc63e92479c5f56e9e95ab98c85fb7ad21c4092ce0`)
   are byte-identical to Neo's assets. The wheel's LICENSE and ThirdPartyNotices
   are also byte-identical to the existing parent-directory
   [ORT 1.30.0 MIT text](../onnxruntime-LICENSE) and
   [third-party notices](../onnxruntime-ThirdPartyNotices.txt), which must accompany
   the binary distribution (they are not separate entries in this source companion).
   All 322 hashed wheel RECORD entries passed verification. The DLL reports
   `1.30.0.20260909.6.f2c39fe`; official v1.30.0 resolves to
   `f2c39fe2f838cf35ce7da92824f5a5e3ee6e88a7`. Its Windows Python packaging
   recipe uses **vcpkg Eigen at the supplied base revision without the two s390x
   patches**. The exact Azure production run was not recovered; this limitation
   does not undo the verified official wheel-to-DLL binding. These DLLs and
   notices are distinct from the 1.28.2 static input.
3. **Optional drawing runtime:** Pyke ORT 1.22.0, TGZ SHA-256
   `540d19b3379fda6fb8f7280d8c15efde20ed225a67a357a6dae38c4300fe190d`,
   is another producer/artifact. Its historical recipe and default Eigen pin
   do not prove its actual producer checkout or covered modifications. Nothing
   here approves or changes that child project or its release gate.

For the static input, the recovered official
[windows-x64 run 33741118114](https://github.com/csukuangfj/onnxruntime-build/actions/runs/33741118114)
uses builder commit `6314c7b252578dc0b8ddd3919b593c4359812818`, not a current-main
snapshot. It succeeded after a `fix-macos` push at 2026-09-03 09:51:08Z. Its
fixed workflow selects ORT 1.28.2, x64 Release /MT and the matching release
filename/upload route; release asset 542612895 was uploaded at 10:38:42–45Z.
Official Actions artifact 9889408216 (106,572,207 bytes) binds to this run and
commit, with SHA-256
`60b5c3bbc1f2cd885fa2624bb6f64e64b603e64857613ae467844272657524f5`.
This is the **outer Actions archive digest**, not the release tar.bz2 digest;
its inner member was not downloaded/compared. Together with the local static
library baseline, this is **strongly inferred release-to-historical-recipe
correspondence**, not direct inner-artifact hash equality or a producer
no-modifications attestation. The release repository's shared-library workflow
is not the static builder. See [BUILD-MODIFICATIONS.md](BUILD-MODIFICATIONS.md).

MIT requires preservation of its copyright and permission notice in copies or
substantial portions. It does not, by itself, require signed builds,
bit-for-bit reproducibility, public release of all source, or an upstream
attestation. The remaining correspondence question matters particularly for
identifying and supplying the actual MPL-covered source, not because those
extra process controls are universal MIT terms.

## Included texts and local preparation

`index.json` records byte hashes and origins for these original texts:
Eigen MPL/Apache/BSD/MINPACK notices; ORT 1.28.2 MIT and third-party notices;
Sherpa, Kaldi native fbank/decoder, kaldifst, OpenFST, KISS FFT,
simple-sentencepiece and nlohmann/json notices. KISS FFT's `COPYING` points to
`LICENSES/BSD-3-Clause`; that full text is retained as `kissfft-BSD-3-Clause`.
An upstream notice list is not a final linked-component inventory.

Offline preparation from the repository root, with the verified archives
already present:

```text
python -B tools/native_source_bundle.py build --bundle target/native-source/neo-native-sources-updated.zip
python -B tools/native_source_bundle.py verify --bundle target/native-source/neo-native-sources-updated.zip
python -B -m unittest discover -s tools -p test_native_source_bundle.py
```

The helper checks pinned archive hashes, complete source inventories, the
recorded no-TTS receipt/manifest and local Eigen file equality, then creates
`target/native-source/neo-native-sources-updated.zip` for this revised notice set,
without overwriting the prior ZIP. The delivery name above may be assigned when
staging; use this updated archive's actual hash/size in the delivery lock, not
the old companion's pins. It copies only the indexed public
texts and the two source archives; private evidence, DLLs/LIBs, credentials and
upstream build scripts are not executed or copied as standalone build tools.
Scripts present in full upstream source archives are inert source contents.
`SHA256SUMS` checks integrity, not signatures or approval. The helper deliberately
refuses an overwritten output or an unreviewed new native build identity.

**Release handoff still required:** resolve or explicitly assess the prebuilt
ORT covered-source correspondence, supply the companion or another verified
source route with the actual binary, and verify final package contents. The
current packager/gates were not changed by this work. These staged materials
alone do not certify MPL compliance for unconfirmed producer modifications.
