# Third-party licenses

Neo's original code is licensed under [Apache-2.0](../../LICENSE); see [NOTICE](../../NOTICE).
Third-party code, fonts, models and runtimes retain their own terms.

- [Cargo notices](cargo-notices.txt): collected license and notice texts from locked dependencies, including build, development and other-platform packages.
- [Embedded components and fonts](assets/README.md): licenses and attribution for copied components, icons and fonts.
- [ONNX Runtime](runtime/README.md): license and third-party notices for the bundled dynamic runtime.
- Bundled Git Bash retains its own license files under `runtime/gitbash/` in the distribution.

This collection is not a complete binary SBOM or a legal compliance certification. Native dependencies, corresponding-source obligations, model rights and missing notices still require release review. A source repository push is not release approval.

Maintainer inventories and detailed audit evidence are kept locally in `docs-pri/licenses/`, outside Git and release packages. Regenerate the Cargo inventory and public notices with `python -X utf8 tools/audit_licenses.py`.
