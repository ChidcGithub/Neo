# Build and covered-source modification scope

## Neo-owned no-TTS build — recorded facts

The recorded Sherpa source is version 1.13.8, commit
`11afbd009a7f8c08f4bcf2fc1b265d0df4670fbf`, source archive SHA-256
`0a8db6c55dd318f4a688faba85f7760b99a6c92e8ef8864479d418531bee1ac2`.
Neo's build config selects static Windows x64 Release /MT, disables TTS,
speaker diarization, GPU/DirectML, Python/JNI, examples/tests and network fetches
during configuration. The top-level Sherpa recipes select Eigen 5.0.1 and
OpenFST 1.8.5-2026-07-09 rather than older child defaults.

Neo adapted the Rust sys build integration to require the validated no-TTS
native libraries and reject fallback to legacy TTS binaries. Details are in
[`vendor/sherpa-onnx-sys/NEO-ASR.md`](../../../../vendor/sherpa-onnx-sys/NEO-ASR.md)
in a checkout; that link is not an offline-companion source-delivery promise.
The repository build tool is `tools/build_sherpa_asr.py`. Original native source
was not replaced with stubs. The recorded extraction omitted/inventoried
273 non-build Sherpa example/script symlinks, not native/CMake/license files.
This is a statement of Neo's own recorded process, not the ORT producer's.

On 2026-10-04 all **1,913 Eigen 5.0.1 files** in the retained source input were
compared with the pinned original tarball: exact bytes, no missing or extra
files. The receipt records that archive as input and the native graph checksum
matches. No Eigen covered-source modifications were found in this local build
input. The companion retains the complete original tarball, not just selected
headers or license files. The present-day file comparison supports the recorded
build; it is not a signed historical file-access trace or a reproducible-build
claim.

The manifest SHA-256 for this local build is
`042914d1d3c427986d34343e40926e782694ba5cf8f06049783b103127239d5e`;
its receipt SHA-256 is
`5fef57cc8591730386b76fa7d1b7c78439258066026e030459f714dc2f9c81bb`.
These are integrity references, not gate signatures. No native compilation or
relink was performed during this source-supply review.

## Prebuilt ORT — do not inherit the no-modifications statement

Neo's build consumes an existing ORT 1.28.2 static library; it does not rebuild
ORT. The upstream baseline at `33ca9628233dc8f002435e868d4c2e9f82766ca1`
selects Eigen commit `1d8b82b0740839c0de7f1242a3585e3390ff5f33` with archive
SHA-1 `05b19b49e6fbb91246be711d801160528c135e34`. This is **not Eigen 5.0.1**.
The supplied original ZIP's SHA-256 is
`6a60d76351f97132669daeeb721d6bf14b008101883ad2d687a3201c5c461eb0`.
Its 1,891 files were checked against the existing verified file inventory and
Git blob identities, reusing the already downloaded source rather than fetching
another copy.

ORT's baseline `cmake/external/eigen.cmake` specifies these two patches in order:

1. `ort-1.28.2-eigen-s390x-build.patch`
2. `ort-1.28.2-eigen-s390x-build-werror.patch`

The original patch bytes are included alongside the complete base ZIP. The
first is a multi-commit patch series touching ZVector headers **and
`Eigen/src/Core/RandomImpl.h`**; the second comments out an unused ZVector
variable. The recipe's patch command is unconditional: do not infer that these
source changes can be discarded because the target is x64 or the filenames
mention s390x. To inspect/reconstruct that upstream recipe's source, unpack the
base archive and apply the supplied patches in this order with strip level 1
(the recipe uses `patch --binary --ignore-whitespace -p1`). They were read as
text, not applied to the live build or executed by this review. Source plus
patches describes the upstream baseline recipe; it does not prove the producer
used that recipe without dependency overrides or additional modifications.

The inspected producer static-builder snapshot selects an ORT release tag,
removes SOVERSION lines from ORT CMake, configures/bundles static dependencies
and has conditional platform-specific changes. No claim is made that this
current snapshot is the exact historical recipe of the downloaded artifact,
or that its producer's Eigen files were unmodified. No producer confirmation,
issue or attestation was authored on anyone's behalf.

## Optional Pyke ORT 1.22.0

The child's historical candidate builder is
`77ec493e3495901a361469951ab992181e52fd05`, while the library self-reports ORT
baseline `f217402897f40ebba457e2421bc0a4702771968e`. Its Eigen default uses the
same `1d8b82b…` revision, but this is a **different ORT baseline**: the 1.28.2
patches above must not be described as Pyke 1.22.0's actual patches. Source
identity verification does not establish precise binary correspondence. The
child's legal files were read only; no child files, approvals or gates changed.
