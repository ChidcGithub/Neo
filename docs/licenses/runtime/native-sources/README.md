# Native source and license supply

This directory supplies notices for Neo's no-TTS native inputs. It is not a
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
| `sources/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip` | Complete 1,891-file source at the Eigen revision named by ORT 1.28.2's upstream dependency table. SHA-256 `6a60d76351f97132669daeeb721d6bf14b008101883ad2d687a3201c5c461eb0`. Archive and file identities verified. **Exact correspondence to the producer's prebuilt ORT remains unconfirmed.** |
| `notices/ort-1.28.2-eigen-s390x-build.patch`, `notices/ort-1.28.2-eigen-s390x-build-werror.patch` | Original patches referenced, in this order, by ORT 1.28.2's Eigen recipe. Supplied with the complete base source, not silently omitted. See [BUILD-MODIFICATIONS.md](BUILD-MODIFICATIONS.md). Their actual use by the producer is not attested. |

Upstream source alternatives:

- Eigen 5.0.1: <https://gitlab.com/libeigen/eigen/-/archive/5.0.1/eigen-5.0.1.tar.gz>
- ORT's default Eigen pin: <https://github.com/eigen-mirror/eigen/archive/1d8b82b0740839c0de7f1242a3585e3390ff5f33/eigen-1d8b82b0740839c0de7f1242a3585e3390ff5f33.zip>
- ORT baseline recipe: <https://github.com/microsoft/onnxruntime/blob/33ca9628233dc8f002435e868d4c2e9f82766ca1/cmake/external/eigen.cmake>

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
2. **Separate bundled DLLs:** the parent directory's
   [ORT 1.30.0 notices](../README.md) relate to a locally matched Python package
   and DLLs whose recorded PE file/product version is
   `1.30.0.20260909.6.f2c39fe`. These do not identify or license the 1.28.2 static
   input. This work does not resolve that DLL's separate covered-source supply.
3. **Optional drawing runtime:** Pyke ORT 1.22.0, TGZ SHA-256
   `540d19b3379fda6fb8f7280d8c15efde20ed225a67a357a6dae38c4300fe190d`,
   is another producer/artifact. Its historical recipe and default Eigen pin
   do not prove its actual producer checkout or covered modifications. Nothing
   here approves or changes that child project or its release gate.

The producer's release README points to
<https://github.com/csukuangfj/onnxruntime-build> for **static** builds.
The release repository's Windows workflow builds shared libraries; using that
workflow as proof of this static library would be incorrect. A reviewed static
builder snapshot at `ef34ed32e315dfa777a058fc64f72cfe4d1b6094` explains naming,
MT selection and upload routing, but is a current snapshot, **not a recovered
historical build-run binding**. Compiled version strings and matching recipes
are evidence of a baseline, not proof of an unmodified producer tree.

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
python -B tools/native_source_bundle.py build
python -B tools/native_source_bundle.py verify
python -B -m unittest discover -s tools -p test_native_source_bundle.py
```

The helper checks pinned archive hashes, complete source inventories, the
recorded no-TTS receipt/manifest and local Eigen file equality, then creates
`target/native-source/neo-native-sources.zip`. It copies only the indexed public
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
